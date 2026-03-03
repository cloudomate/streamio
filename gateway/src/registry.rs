use crate::{
    provisioner::{DefaultVmSpec, VmProvisioner},
    AppState,
};
use anyhow::Result;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use reqwest::Client;
use sqlx::{PgPool, Row};
use std::{sync::Arc, time::Duration};
use streamio_types::{BackendInfo, OsType, RegisterRequest};
use tokio::time;
use tracing::{info, warn};
use uuid::Uuid;

pub struct BackendRegistry {
    db: PgPool,
    http: Client,
}

impl BackendRegistry {
    pub fn new(db: PgPool) -> Self {
        BackendRegistry {
            db,
            http: Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        }
    }

    /// Return the backend_id assigned to a user, if any.
    pub async fn get_assignment(&self, user_sub: &str) -> Option<Uuid> {
        let row = sqlx::query("SELECT backend_id FROM assignments WHERE user_sub = $1")
            .bind(user_sub)
            .fetch_optional(&self.db)
            .await
            .ok()
            .flatten()?;
        row.try_get("backend_id").ok()
    }

    /// Get-or-assign: find existing assignment, wake a stopped VM, or auto-provision a new one.
    pub async fn get_or_assign(
        &self,
        user_sub: &str,
        provisioner: Option<&Arc<dyn VmProvisioner>>,
        default_vm: Option<&DefaultVmSpec>,
    ) -> Option<BackendInfo> {
        // Existing assignment?
        if let Some(id) = self.get_assignment(user_sub).await {
            if let Ok(Some(info)) = self.get_backend(id).await {
                if info.healthy {
                    return Some(info);
                }
                // Backend exists but unhealthy — if it's a KubeVirt VM, try to wake it
                if let Some(p) = provisioner {
                    if let Some((vm_name, vm_ns)) = self.get_vm_columns(id).await {
                        info!("Waking stopped VM {vm_name} for user {user_sub}");
                        let _ = p.start(&vm_name, &vm_ns).await;
                        return self.wait_for_healthy(id, 120).await;
                    }
                }
            }
        }

        // Auto-provision if provisioner + default spec are available
        if let (Some(p), Some(spec)) = (provisioner, default_vm) {
            return self.auto_provision(user_sub, p, spec).await;
        }

        // Static/manual mode: pick least-loaded healthy backend
        let backend = self.pick_backend().await?;

        let _ = sqlx::query(
            "INSERT INTO assignments (user_sub, backend_id) VALUES ($1, $2)
             ON CONFLICT (user_sub) DO UPDATE SET backend_id = $2, assigned_at = now()",
        )
        .bind(user_sub)
        .bind(backend.id)
        .execute(&self.db)
        .await;

        Some(backend)
    }

    /// Create a new VM for a user, insert DB records, start it, and wait for self-registration.
    async fn auto_provision(
        &self,
        user_sub: &str,
        provisioner: &Arc<dyn VmProvisioner>,
        spec: &DefaultVmSpec,
    ) -> Option<BackendInfo> {
        let backend_id = Uuid::new_v4();
        let req = spec.into_provision_request(user_sub.to_string(), None);
        info!("Auto-provisioning VM for user {user_sub} (backend_id={backend_id})");

        let handle = match provisioner.provision(&req, backend_id).await {
            Ok(h) => h,
            Err(e) => {
                warn!("VM provision failed for {user_sub}: {e}");
                return None;
            }
        };

        let os_type_str = os_type_str(&req.os_type);

        // Insert backend record with placeholder URL; self-registration updates the real URL
        if let Err(e) = sqlx::query(
            "INSERT INTO backends (id, url, label, healthy, last_seen, vm_type, vm_name, vm_ns, os_type, disk_pvc)
             VALUES ($1, 'pending://provisioning', $2, false, now(), 'kubevirt', $3, $4, $5, $6)",
        )
        .bind(backend_id)
        .bind(&req.label)
        .bind(&handle.vm_name)
        .bind(&handle.ns)
        .bind(os_type_str)
        .bind(&handle.disk_pvc)
        .execute(&self.db)
        .await
        {
            warn!("Failed to insert backend record for {user_sub}: {e}");
            return None;
        }

        // Insert vm_states row
        let _ = sqlx::query(
            "INSERT INTO vm_states (backend_id, state)
             VALUES ($1, 'starting')
             ON CONFLICT (backend_id) DO UPDATE SET state = 'starting', updated_at = now()",
        )
        .bind(backend_id)
        .execute(&self.db)
        .await;

        // Persist user assignment
        let _ = sqlx::query(
            "INSERT INTO assignments (user_sub, backend_id) VALUES ($1, $2)
             ON CONFLICT (user_sub) DO UPDATE SET backend_id = $2, assigned_at = now()",
        )
        .bind(user_sub)
        .bind(backend_id)
        .execute(&self.db)
        .await;

        // Start the VM (patch spec.running = true)
        if let Err(e) = provisioner.start(&handle.vm_name, &handle.ns).await {
            warn!("Failed to start provisioned VM {}: {e}", handle.vm_name);
        }

        // Poll until the VM self-registers (healthy=true) or we time out
        self.wait_for_healthy(backend_id, 120).await
    }

    /// Poll the DB every 3 s until the backend is healthy, or timeout expires.
    async fn wait_for_healthy(&self, backend_id: Uuid, timeout_secs: u64) -> Option<BackendInfo> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        loop {
            interval.tick().await;
            if tokio::time::Instant::now() >= deadline {
                warn!("Timed out waiting for backend {backend_id} to become healthy");
                return None;
            }
            if let Ok(Some(info)) = self.get_backend(backend_id).await {
                if info.healthy {
                    return Some(info);
                }
            }
        }
    }

    /// Fetch (vm_name, vm_ns) from the backends table for a KubeVirt VM.
    pub async fn get_vm_columns(&self, backend_id: Uuid) -> Option<(String, String)> {
        let row = sqlx::query(
            "SELECT vm_name, vm_ns FROM backends WHERE id = $1 AND vm_name IS NOT NULL",
        )
        .bind(backend_id)
        .fetch_optional(&self.db)
        .await
        .ok()
        .flatten()?;

        let vm_name: String = row.try_get("vm_name").ok()?;
        let vm_ns: String = row.try_get("vm_ns").ok()?;
        Some((vm_name, vm_ns))
    }

    /// Fetch disk_pvc for a backend.
    pub async fn get_disk_pvc(&self, backend_id: Uuid) -> Option<String> {
        let row = sqlx::query(
            "SELECT disk_pvc FROM backends WHERE id = $1 AND disk_pvc IS NOT NULL",
        )
        .bind(backend_id)
        .fetch_optional(&self.db)
        .await
        .ok()
        .flatten()?;
        row.try_get("disk_pvc").ok()
    }

    pub async fn get_backend(&self, id: Uuid) -> Result<Option<BackendInfo>> {
        let row = sqlx::query("SELECT id, url, label, healthy FROM backends WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.db)
            .await?;

        Ok(row.map(|r: sqlx::postgres::PgRow| BackendInfo {
            id: r.get("id"),
            url: r.get("url"),
            label: r.get("label"),
            healthy: r.get("healthy"),
        }))
    }

    /// List all backends.
    pub async fn list_backends(&self) -> Result<Vec<BackendInfo>> {
        let rows = sqlx::query("SELECT id, url, label, healthy FROM backends ORDER BY label")
            .fetch_all(&self.db)
            .await?;

        Ok(rows
            .into_iter()
            .map(|r: sqlx::postgres::PgRow| BackendInfo {
                id: r.get("id"),
                url: r.get("url"),
                label: r.get("label"),
                healthy: r.get("healthy"),
            })
            .collect())
    }

    /// Pick the healthy backend with the fewest active assignments.
    async fn pick_backend(&self) -> Option<BackendInfo> {
        let row = sqlx::query(
            "SELECT b.id, b.url, b.label, b.healthy
             FROM backends b
             LEFT JOIN assignments a ON a.backend_id = b.id
             WHERE b.healthy = true
             GROUP BY b.id
             ORDER BY COUNT(a.user_sub) ASC
             LIMIT 1",
        )
        .fetch_optional(&self.db)
        .await
        .ok()
        .flatten()?;

        Some(BackendInfo {
            id: row.get("id"),
            url: row.get("url"),
            label: row.get("label"),
            healthy: row.get("healthy"),
        })
    }

    /// Update health status of a backend.
    pub async fn set_health(&self, id: Uuid, healthy: bool) {
        let _ = sqlx::query(
            "UPDATE backends SET healthy = $1, last_seen = now() WHERE id = $2",
        )
        .bind(healthy)
        .bind(id)
        .execute(&self.db)
        .await;
    }

    /// Poll a single backend's /healthz endpoint.
    pub async fn poll_backend(&self, info: &BackendInfo) -> bool {
        self.http
            .get(format!("{}/healthz", info.url))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

fn os_type_str(os: &OsType) -> &'static str {
    match os {
        OsType::Windows11 => "windows11",
        OsType::Ubuntu => "ubuntu",
        OsType::Alpine => "alpine",
    }
}

/// Background task: poll all backends every 30 seconds.
pub async fn health_poll_task(registry: Arc<BackendRegistry>, _db: PgPool) {
    let mut interval = time::interval(Duration::from_secs(30));
    loop {
        interval.tick().await;
        let backends = match registry.list_backends().await {
            Ok(b) => b,
            Err(e) => {
                warn!("Health poll: failed to list backends: {e}");
                continue;
            }
        };
        for backend in backends {
            let healthy = registry.poll_backend(&backend).await;
            if healthy != backend.healthy {
                info!(
                    "Backend {} ({}) health changed → {}",
                    backend.label.as_deref().unwrap_or("?"),
                    backend.id,
                    if healthy { "healthy" } else { "unhealthy" }
                );
            }
            registry.set_health(backend.id, healthy).await;
        }
    }
}

/// POST /internal/register — called by backends on startup.
pub async fn register_handler(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> impl IntoResponse {
    match sqlx::query(
        "INSERT INTO backends (id, url, label, healthy, last_seen)
         VALUES ($1, $2, $3, true, now())
         ON CONFLICT (id) DO UPDATE SET url = $2, label = $3, healthy = true, last_seen = now()",
    )
    .bind(req.id)
    .bind(&req.url)
    .bind(&req.label)
    .execute(&state.db)
    .await
    {
        Ok(_) => {
            info!("Backend registered: id={} url={}", req.id, req.url);
            StatusCode::OK
        }
        Err(e) => {
            tracing::error!("Failed to register backend: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}
