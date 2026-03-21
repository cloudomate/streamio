//! REST API for gateway integration.
//!
//! Endpoints:
//!   POST   /api/sessions          — Create a session (display + account + backend)
//!   DELETE /api/sessions/:id      — Destroy a session
//!   GET    /api/sessions          — List active sessions
//!   GET    /api/sessions/:id      — Get session details

use crate::input_router::InputRouterState;
use crate::{SessionManagerState, UserSession};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use std::sync::Arc;
use streamio_types::{SessionInfo, SessionRequest, SessionResponse};
use tracing::{error, info, warn};

/// Combined state for API handlers.
#[derive(Clone)]
pub struct ApiState {
    pub sm: Arc<SessionManagerState>,
    pub input: Arc<InputRouterState>,
}

pub fn router(sm: Arc<SessionManagerState>, input: Arc<InputRouterState>) -> Router {
    let state = ApiState { sm, input };

    Router::new()
        .route("/api/sessions", post(create_session))
        .route("/api/sessions", get(list_sessions))
        .route("/api/sessions/:id", get(get_session))
        .route("/api/sessions/:id", delete(destroy_session))
        .with_state(state)
}

async fn create_session(
    State(state): State<ApiState>,
    Json(req): Json<SessionRequest>,
) -> impl IntoResponse {
    info!(
        "Creating session for user {} ({}x{}@{}Hz)",
        req.user_id, req.width, req.height, req.refresh_hz
    );

    // 1. Create or get local Windows account
    let account = match crate::accounts::create_or_get_account(&req.user_id) {
        Ok(a) => a,
        Err(e) => {
            error!("Failed to create account: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Account creation failed: {}", e)})),
            )
                .into_response();
        }
    };

    // 2. Create virtual display
    let display_id = match crate::display::create_display(
        &state.sm.display_ctl_path,
        req.width,
        req.height,
        req.refresh_hz,
    ) {
        Ok(id) => id,
        Err(e) => {
            error!("Failed to create display: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Display creation failed: {}", e)})),
            )
                .into_response();
        }
    };

    // 3. Extend desktop to include new display
    if let Err(e) = crate::display::extend_desktop() {
        warn!("Failed to extend desktop (may already be extended): {}", e);
    }

    // Wait briefly for Windows to settle the display topology
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // 4. Query actual display rectangle from EnumDisplayMonitors
    // The new virtual display should be the last one enumerated
    let displays = crate::display::enumerate_displays();
    let new_display = displays.last();
    let display_rect = new_display
        .map(|d| (d.x, d.y, d.width, d.height))
        .unwrap_or((0, 0, req.width, req.height));
    // Use the Windows monitor enumeration index (not the IddCx driver index)
    // so the backend captures the correct monitor. Display 0 is the physical display.
    let monitor_index = new_display.map(|d| d.index).unwrap_or(display_id);
    // TODO: remove this debug override
    info!("DEBUG: enumerated {} displays, monitor_index={}", displays.len(), monitor_index);

    info!(
        "Display {} (monitor_index={}) rect: ({}, {}, {}x{})",
        display_id, monitor_index, display_rect.0, display_rect.1, display_rect.2, display_rect.3
    );

    // 5. Allocate port and create session
    let port = state.sm.allocate_port().await;
    let session_id = uuid::Uuid::new_v4().to_string();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // 6. Start input pipe for this session
    state
        .input
        .spawn_session_pipe(session_id.clone(), display_rect);

    // 7. Launch backend process under user account
    let backend_pid = match crate::launcher::launch_backend(
        &state.sm.backend_path,
        &account.username,
        &account.password,
        port,
        monitor_index,
        &session_id,
        &state.sm.token_secret,
        &state.sm.gateway_url,
    ) {
        Ok(proc) => {
            let pid = proc.pid;
            // Store the process handle somewhere we can monitor it
            // For now, just let it run — the process handle is dropped but the process continues
            std::mem::forget(proc); // don't close handles
            Some(pid)
        }
        Err(e) => {
            error!("Failed to launch backend: {}", e);
            // Clean up display on failure
            let _ = crate::display::destroy_display(&state.sm.display_ctl_path, display_id);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Backend launch failed: {}", e)})),
            )
                .into_response();
        }
    };

    // 8. Store session
    let session = UserSession {
        session_id: session_id.clone(),
        user_id: req.user_id.clone(),
        windows_user: account.username.clone(),
        display_index: display_id,
        display_rect,
        backend_port: port,
        backend_pid,
        created_at: now,
    };

    {
        let mut sessions = state.sm.sessions.write().await;
        sessions.insert(session_id.clone(), session);
    }
    state.sm.persist_state().await;

    info!(
        "Session {} created: user={}, display={}, port={}",
        session_id, req.user_id, display_id, port
    );

    (
        StatusCode::CREATED,
        Json(serde_json::json!(SessionResponse {
            session_id,
            backend_port: port,
            display_index: display_id,
            windows_user: account.username,
        })),
    )
        .into_response()
}

async fn list_sessions(State(state): State<ApiState>) -> impl IntoResponse {
    let sessions = state.sm.sessions.read().await;
    let list: Vec<SessionInfo> = sessions
        .values()
        .map(|s| SessionInfo {
            session_id: s.session_id.clone(),
            user_id: s.user_id.clone(),
            windows_user: s.windows_user.clone(),
            display_index: s.display_index,
            display_rect: s.display_rect,
            backend_port: s.backend_port,
            backend_pid: s.backend_pid,
            created_at: s.created_at,
        })
        .collect();
    Json(list)
}

async fn get_session(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let sessions = state.sm.sessions.read().await;
    match sessions.get(&id) {
        Some(s) => Json(serde_json::json!(SessionInfo {
            session_id: s.session_id.clone(),
            user_id: s.user_id.clone(),
            windows_user: s.windows_user.clone(),
            display_index: s.display_index,
            display_rect: s.display_rect,
            backend_port: s.backend_port,
            backend_pid: s.backend_pid,
            created_at: s.created_at,
        }))
        .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn destroy_session(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let session = {
        let mut sessions = state.sm.sessions.write().await;
        sessions.remove(&id)
    };

    match session {
        Some(s) => {
            info!("Destroying session {}", id);

            // Kill backend process
            if let Some(pid) = s.backend_pid {
                let _ = std::process::Command::new(r"C:\Windows\system32\taskkill.exe")
                    .args(["/F", "/PID", &pid.to_string()])
                    .output();
                info!("Killed backend pid {}", pid);
            }

            // Destroy virtual display
            if let Err(e) =
                crate::display::destroy_display(&state.sm.display_ctl_path, s.display_index)
            {
                warn!("Failed to destroy display {}: {}", s.display_index, e);
            }

            state.sm.persist_state().await;
            StatusCode::NO_CONTENT.into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
