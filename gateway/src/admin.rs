use crate::{middleware::RequireAdmin, AppState};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    Json,
};
use serde::Serialize;
use sqlx::Row;
use streamio_types::{AssignRequest, BackendInfo, ProvisionRequest, ShadowRequest, UserAssignment};
use tracing::error;
use uuid::Uuid;

static ADMIN_HTML: &str = include_str!("../../../client/admin.html");

/// GET /admin — serve admin panel UI.
pub async fn admin_ui_handler(_: RequireAdmin) -> impl IntoResponse {
    Html(ADMIN_HTML)
}

// ── Backends ─────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct BackendStatusResponse {
    pub id: Uuid,
    pub url: String,
    pub label: Option<String>,
    pub healthy: bool,
}

/// GET /admin/api/backends
pub async fn list_backends(
    _: RequireAdmin,
    State(state): State<AppState>,
) -> impl IntoResponse {
    match state.registry.list_backends().await {
        Ok(backends) => Json(
            backends
                .into_iter()
                .map(|b| BackendStatusResponse {
                    id: b.id,
                    url: b.url,
                    label: b.label,
                    healthy: b.healthy,
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => {
            error!("list_backends error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// POST /admin/api/backends/provision — provision a new KubeVirt VM for a user.
pub async fn provision_backend(
    _: RequireAdmin,
    State(state): State<AppState>,
    Json(req): Json<ProvisionRequest>,
) -> impl IntoResponse {
    let provisioner = match &state.provisioner {
        Some(p) => p.clone(),
        None => {
            return (StatusCode::NOT_IMPLEMENTED, "KubeVirt provisioner is not enabled")
                .into_response()
        }
    };

    let backend_id = Uuid::new_v4();

    let handle = match provisioner.provision(&req, backend_id).await {
        Ok(h) => h,
        Err(e) => {
            error!("provision_backend error: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let os_type_str = match &req.os_type {
        streamio_types::OsType::Windows11 => "windows11",
        streamio_types::OsType::Ubuntu => "ubuntu",
        streamio_types::OsType::Alpine => "alpine",
    };

    match sqlx::query(
        "INSERT INTO backends (id, url, label, healthy, last_seen, vm_type, vm_name, vm_ns, os_type, disk_pvc)
         VALUES ($1, 'pending://provisioning', $2, false, now(), 'kubevirt', $3, $4, $5, $6)",
    )
    .bind(backend_id)
    .bind(&req.label)
    .bind(&handle.vm_name)
    .bind(&handle.ns)
    .bind(os_type_str)
    .bind(&handle.disk_pvc)
    .execute(&state.db)
    .await
    {
        Ok(_) => {}
        Err(e) => {
            error!("Failed to insert provisioned backend: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    let _ = sqlx::query(
        "INSERT INTO vm_states (backend_id, state) VALUES ($1, 'stopped')",
    )
    .bind(backend_id)
    .execute(&state.db)
    .await;

    Json(BackendStatusResponse {
        id: backend_id,
        url: "pending://provisioning".into(),
        label: req.label.clone(),
        healthy: false,
    })
    .into_response()
}

// ── VM lifecycle management ───────────────────────────────────────────────────

#[derive(Serialize)]
struct VmStateResponse {
    backend_id: Uuid,
    state: String,
}

/// POST /admin/api/vms/:id/start — power on a stopped VM.
pub async fn vm_start(
    _: RequireAdmin,
    State(state): State<AppState>,
    Path(backend_id): Path<Uuid>,
) -> impl IntoResponse {
    let provisioner = match &state.provisioner {
        Some(p) => p.clone(),
        None => {
            return (StatusCode::NOT_IMPLEMENTED, "KubeVirt provisioner is not enabled")
                .into_response()
        }
    };

    let (vm_name, vm_ns) = match state.registry.get_vm_columns(backend_id).await {
        Some(cols) => cols,
        None => return (StatusCode::NOT_FOUND, "VM not found").into_response(),
    };

    match provisioner.start(&vm_name, &vm_ns).await {
        Ok(_) => {
            let _ = sqlx::query(
                "INSERT INTO vm_states (backend_id, state)
                 VALUES ($1, 'starting')
                 ON CONFLICT (backend_id) DO UPDATE SET state = 'starting', updated_at = now()",
            )
            .bind(backend_id)
            .execute(&state.db)
            .await;
            StatusCode::OK.into_response()
        }
        Err(e) => {
            error!("vm_start error for {backend_id}: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// POST /admin/api/vms/:id/stop — gracefully power off a running VM.
pub async fn vm_stop(
    _: RequireAdmin,
    State(state): State<AppState>,
    Path(backend_id): Path<Uuid>,
) -> impl IntoResponse {
    let provisioner = match &state.provisioner {
        Some(p) => p.clone(),
        None => {
            return (StatusCode::NOT_IMPLEMENTED, "KubeVirt provisioner is not enabled")
                .into_response()
        }
    };

    let (vm_name, vm_ns) = match state.registry.get_vm_columns(backend_id).await {
        Some(cols) => cols,
        None => return (StatusCode::NOT_FOUND, "VM not found").into_response(),
    };

    match provisioner.stop(&vm_name, &vm_ns).await {
        Ok(_) => {
            let _ = sqlx::query(
                "INSERT INTO vm_states (backend_id, state)
                 VALUES ($1, 'stopping')
                 ON CONFLICT (backend_id) DO UPDATE SET state = 'stopping', updated_at = now()",
            )
            .bind(backend_id)
            .execute(&state.db)
            .await;
            // Mark backend as unhealthy since it's shutting down
            state.registry.set_health(backend_id, false).await;
            StatusCode::OK.into_response()
        }
        Err(e) => {
            error!("vm_stop error for {backend_id}: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// DELETE /admin/api/vms/:id — delete VM, DataVolume, and PVC.
pub async fn vm_delete(
    _: RequireAdmin,
    State(state): State<AppState>,
    Path(backend_id): Path<Uuid>,
) -> impl IntoResponse {
    let provisioner = match &state.provisioner {
        Some(p) => p.clone(),
        None => {
            return (StatusCode::NOT_IMPLEMENTED, "KubeVirt provisioner is not enabled")
                .into_response()
        }
    };

    let (vm_name, vm_ns) = match state.registry.get_vm_columns(backend_id).await {
        Some(cols) => cols,
        None => return (StatusCode::NOT_FOUND, "VM not found").into_response(),
    };

    let disk_pvc = state
        .registry
        .get_disk_pvc(backend_id)
        .await
        .unwrap_or_default();

    if let Err(e) = provisioner.delete(&vm_name, &vm_ns, &disk_pvc).await {
        error!("vm_delete error for {backend_id}: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    // Remove backend + cascading vm_states/assignments rows
    let _ = sqlx::query("DELETE FROM backends WHERE id = $1")
        .bind(backend_id)
        .execute(&state.db)
        .await;

    StatusCode::NO_CONTENT.into_response()
}

/// GET /admin/api/vms/:id/state — query live power state from KubeVirt.
pub async fn vm_state(
    _: RequireAdmin,
    State(state): State<AppState>,
    Path(backend_id): Path<Uuid>,
) -> impl IntoResponse {
    let provisioner = match &state.provisioner {
        Some(p) => p.clone(),
        None => {
            return (StatusCode::NOT_IMPLEMENTED, "KubeVirt provisioner is not enabled")
                .into_response()
        }
    };

    let (vm_name, vm_ns) = match state.registry.get_vm_columns(backend_id).await {
        Some(cols) => cols,
        None => return (StatusCode::NOT_FOUND, "VM not found").into_response(),
    };

    match provisioner.state(&vm_name, &vm_ns).await {
        Ok(s) => {
            // Sync to vm_states table
            let _ = sqlx::query(
                "INSERT INTO vm_states (backend_id, state)
                 VALUES ($1, $2)
                 ON CONFLICT (backend_id) DO UPDATE SET state = $2, updated_at = now()",
            )
            .bind(backend_id)
            .bind(&s)
            .execute(&state.db)
            .await;
            Json(VmStateResponse { backend_id, state: s }).into_response()
        }
        Err(e) => {
            error!("vm_state error for {backend_id}: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ── User assignments ──────────────────────────────────────────────────────────

/// GET /admin/api/users
pub async fn list_users(
    _: RequireAdmin,
    State(state): State<AppState>,
) -> impl IntoResponse {
    match sqlx::query(
        "SELECT a.user_sub, a.backend_id, b.label as backend_label
         FROM assignments a
         LEFT JOIN backends b ON b.id = a.backend_id
         ORDER BY a.assigned_at DESC",
    )
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => Json(
            rows.into_iter()
                .map(|r: sqlx::postgres::PgRow| UserAssignment {
                    user_sub: r.get("user_sub"),
                    email: None,
                    backend_id: r.get("backend_id"),
                    backend_label: r.get("backend_label"),
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => {
            error!("list_users error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// POST /admin/api/assignments
pub async fn create_assignment(
    _: RequireAdmin,
    State(state): State<AppState>,
    Json(req): Json<AssignRequest>,
) -> impl IntoResponse {
    match sqlx::query(
        "INSERT INTO assignments (user_sub, backend_id)
         VALUES ($1, $2)
         ON CONFLICT (user_sub) DO UPDATE SET backend_id = $2, assigned_at = now()",
    )
    .bind(&req.user_sub)
    .bind(req.backend_id)
    .execute(&state.db)
    .await
    {
        Ok(_) => StatusCode::OK,
        Err(e) => {
            error!("create_assignment error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// DELETE /admin/api/assignments/:sub
pub async fn delete_assignment(
    _: RequireAdmin,
    State(state): State<AppState>,
    Path(sub): Path<String>,
) -> impl IntoResponse {
    match sqlx::query("DELETE FROM assignments WHERE user_sub = $1")
        .bind(&sub)
        .execute(&state.db)
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(e) => {
            error!("delete_assignment error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

// ── Sessions ──────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct SessionInfo {
    pub backend_id: Uuid,
    pub backend_url: String,
    pub active: bool,
}

/// GET /admin/api/sessions — proxies to backend /healthz for now.
pub async fn list_sessions(
    _: RequireAdmin,
    State(state): State<AppState>,
) -> impl IntoResponse {
    match state.registry.list_backends().await {
        Ok(backends) => Json(
            backends
                .into_iter()
                .map(|b| SessionInfo {
                    backend_id: b.id,
                    backend_url: b.url,
                    active: b.healthy,
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => {
            error!("list_sessions error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// POST /admin/api/sessions/:id/shadow — assign observer role to a user on a session.
pub async fn shadow_session(
    _: RequireAdmin,
    State(state): State<AppState>,
    Path(backend_id): Path<Uuid>,
    Json(req): Json<ShadowRequest>,
) -> impl IntoResponse {
    match sqlx::query(
        "INSERT INTO assignments (user_sub, backend_id)
         VALUES ($1, $2)
         ON CONFLICT (user_sub) DO UPDATE SET backend_id = $2, assigned_at = now()",
    )
    .bind(&req.user_sub)
    .bind(backend_id)
    .execute(&state.db)
    .await
    {
        Ok(_) => StatusCode::OK,
        Err(e) => {
            error!("shadow_session error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// DELETE /admin/api/sessions/:id — not yet implemented (requires backend API).
pub async fn disconnect_session(
    _: RequireAdmin,
    Path(_backend_id): Path<Uuid>,
) -> impl IntoResponse {
    // TODO (Phase 4): POST to backend /sessions/:id/disconnect
    (StatusCode::NOT_IMPLEMENTED, "Force-disconnect not yet implemented")
}

