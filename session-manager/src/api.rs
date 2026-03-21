//! REST API for gateway integration.
//!
//! Endpoints:
//!   POST   /api/sessions          — Create or reuse a session (display + account + backend)
//!   DELETE /api/sessions/:id      — Mark session inactive (backend+display kept alive)
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
        "Session request for user {} ({}x{}@{}Hz)",
        req.user_id, req.width, req.height, req.refresh_hz
    );

    // ── Check if this user already has a session (reuse it) ──
    {
        let sessions = state.sm.sessions.read().await;
        if let Some(existing) = sessions.values().find(|s| s.user_id == req.user_id) {
            info!(
                "Reusing existing session {} for user {} (port={}, display={})",
                existing.session_id, req.user_id, existing.backend_port, existing.display_index
            );

            // Check if backend is still alive
            let backend_alive = if let Some(pid) = existing.backend_pid {
                is_process_alive(pid)
            } else {
                false
            };

            if backend_alive {
                return (
                    StatusCode::OK,
                    Json(serde_json::json!(SessionResponse {
                        session_id: existing.session_id.clone(),
                        backend_port: existing.backend_port,
                        display_index: existing.display_index,
                        os_user: existing.os_user.clone(),
                    })),
                )
                    .into_response();
            } else {
                warn!("Existing session {} has dead backend, will recreate backend only",
                      existing.session_id);
                // Backend died but display is still there — just relaunch backend
                let session_id = existing.session_id.clone();
                let port = existing.backend_port;
                let monitor_index = existing.display_index;
                let os_user = existing.os_user.clone();
                drop(sessions); // release read lock

                // Get account info
                let account = match crate::accounts::create_or_get_account(&req.user_id) {
                    Ok(a) => a,
                    Err(e) => {
                        error!("Failed to get account: {}", e);
                        return (StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({"error": format!("Account error: {}", e)})))
                            .into_response();
                    }
                };

                let backend_pid = match crate::launcher::launch_backend(
                    &state.sm.backend_path, &account.username, &account.password,
                    port, monitor_index, &session_id,
                    &state.sm.token_secret, &state.sm.gateway_url,
                ) {
                    Ok(proc) => {
                        let pid = proc.pid;
                        std::mem::forget(proc);
                        Some(pid)
                    }
                    Err(e) => {
                        error!("Failed to relaunch backend: {}", e);
                        return (StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({"error": format!("Backend relaunch failed: {}", e)})))
                            .into_response();
                    }
                };

                // Update session with new PID
                {
                    let mut sessions = state.sm.sessions.write().await;
                    if let Some(s) = sessions.get_mut(&session_id) {
                        s.backend_pid = backend_pid;
                    }
                }
                state.sm.persist_state().await;

                info!("Backend relaunched for session {} (pid={:?})", session_id, backend_pid);
                return (
                    StatusCode::OK,
                    Json(serde_json::json!(SessionResponse {
                        session_id,
                        backend_port: port,
                        display_index: monitor_index,
                        os_user,
                    })),
                )
                    .into_response();
            }
        }
    }

    // ── No existing session — create everything from scratch ──
    info!("Creating new session for user {}", req.user_id);

    // 1. Create or get local OS account
    let account = match crate::accounts::create_or_get_account(&req.user_id) {
        Ok(a) => a,
        Err(e) => {
            error!("Failed to create account: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Account creation failed: {}", e)})))
                .into_response();
        }
    };

    // 2. Determine display for this user.
    // Count current sessions to decide: first user gets the physical display (0),
    // additional users get virtual displays.
    let current_session_count = state.sm.sessions.read().await.len();

    let (display_id, monitor_index, display_rect) = if current_session_count == 0 {
        // First user: use physical display 0 — no virtual display needed
        info!("First user — using physical display 0");
        let displays = crate::display::enumerate_displays();
        let primary = displays.first();
        let rect = primary
            .map(|d| (d.x, d.y, d.width, d.height))
            .unwrap_or((0, 0, req.width, req.height));
        (0u32, 0u32, rect)
    } else {
        // Additional users: create virtual display
        let did = match crate::display::create_display(
            &state.sm.display_ctl_path, req.width, req.height, req.refresh_hz,
        ) {
            Ok(id) => id,
            Err(e) => {
                error!("Failed to create display: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("Display creation failed: {}", e)})))
                    .into_response();
            }
        };

        if let Err(e) = crate::display::extend_desktop() {
            warn!("Failed to extend desktop: {}", e);
        }

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let displays = crate::display::enumerate_displays();
        let new_display = displays.last();
        let rect = new_display
            .map(|d| (d.x, d.y, d.width, d.height))
            .unwrap_or((0, 0, req.width, req.height));
        let midx = new_display.map(|d| d.index).unwrap_or(did);
        (did, midx, rect)
    };

    info!(
        "Display {} (monitor_index={}) rect: ({}, {}, {}x{})",
        display_id, monitor_index, display_rect.0, display_rect.1,
        display_rect.2, display_rect.3
    );

    // 5. Allocate port and create session
    let port = state.sm.allocate_port().await;
    let session_id = uuid::Uuid::new_v4().to_string();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // 6. Start input pipe/socket
    #[cfg(target_os = "linux")]
    crate::input_router::register_session_display(&session_id, display_id);

    state.input.spawn_session_pipe(session_id.clone(), display_rect);

    // 7. Launch backend
    let backend_pid = match crate::launcher::launch_backend(
        &state.sm.backend_path, &account.username, &account.password,
        port, monitor_index, &session_id,
        &state.sm.token_secret, &state.sm.gateway_url,
    ) {
        Ok(proc) => {
            let pid = proc.pid;
            std::mem::forget(proc);
            Some(pid)
        }
        Err(e) => {
            error!("Failed to launch backend: {}", e);
            let _ = crate::display::destroy_display(&state.sm.display_ctl_path, display_id);
            return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Backend launch failed: {}", e)})))
                .into_response();
        }
    };

    // 8. Store session
    let session = UserSession {
        session_id: session_id.clone(),
        user_id: req.user_id.clone(),
        os_user: account.username.clone(),
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
            os_user: account.username,
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
            os_user: s.os_user.clone(),
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
            os_user: s.os_user.clone(),
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
    // Don't remove the session or kill anything — just log it.
    // The backend and display stay alive for reconnect.
    let sessions = state.sm.sessions.read().await;
    match sessions.get(&id) {
        Some(s) => {
            info!(
                "Session {} marked inactive (backend pid={:?}, display={} kept alive)",
                id, s.backend_pid, s.display_index
            );
            StatusCode::NO_CONTENT.into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn is_process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let output = std::process::Command::new(r"C:\Windows\system32\tasklist.exe")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output();
        if let Ok(out) = output {
            let s = String::from_utf8_lossy(&out.stdout);
            return s.contains("streamio.exe");
        }
        false
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}
