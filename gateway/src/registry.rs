//! Legacy backend registry — direct backend pool management.
//!
//! Tracks backends that self-register via /internal/register,
//! polls their health, and assigns users to least-loaded backends.
//!
//! For the new host-based VDI flow, see hosts.rs and portal.rs.

use crate::AppState;
use anyhow::Result;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use reqwest::Client;
use sqlx::{PgPool, Row};
use std::{sync::Arc, time::Duration};
use streamio_types::{BackendInfo, RegisterRequest};
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

    /// Get-or-assign: find existing assignment or pick least-loaded backend.
    pub async fn get_or_assign(&self, user_sub: &str) -> Option<BackendInfo> {
        // Existing assignment?
        if let Some(id) = self.get_assignment(user_sub).await {
            if let Ok(Some(info)) = self.get_backend(id).await {
                if info.healthy {
                    return Some(info);
                }
            }
        }

        // Pick least-loaded healthy backend
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
                    "Backend {} ({}) health changed -> {}",
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
