use crate::{middleware::RequireAdmin, AppState};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    Json,
};
use serde::Serialize;
use sqlx::Row;
use streamio_types::{AssignRequest, BackendInfo, ShadowRequest, UserAssignment};
use tracing::error;
use uuid::Uuid;

static ADMIN_HTML: &str = include_str!("../../client/admin.html");

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

/// GET /admin/api/sessions
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

/// POST /admin/api/sessions/:id/shadow
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

/// DELETE /admin/api/sessions/:id
pub async fn disconnect_session(
    _: RequireAdmin,
    Path(_backend_id): Path<Uuid>,
) -> impl IntoResponse {
    (StatusCode::NOT_IMPLEMENTED, "Use /admin/api/vdi-sessions/:id instead")
}
