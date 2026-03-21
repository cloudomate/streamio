//! User portal — VDI dashboard for end users.
//!
//! After OIDC login, users see their assigned VDI hosts and can
//! connect/disconnect sessions. The gateway proxies session creation
//! to the appropriate session manager agent.

use crate::middleware::RequireSession;
use crate::AppState;
use axum::{extract::{Path, State}, http::StatusCode, response::IntoResponse, Json};
use tracing::{error, info, warn};
use uuid::Uuid;

/// GET /api/me — returns the authenticated user's info.
pub async fn me(RequireSession(claims): RequireSession) -> impl IntoResponse {
    Json(serde_json::json!({
        "sub": claims.sub,
        "email": claims.email,
        "role": claims.role,
    }))
}

/// GET /api/me/vdis — list the user's assigned VDI hosts with session status.
pub async fn my_vdis(
    RequireSession(claims): RequireSession,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, String, bool)>(
        "SELECT h.id, h.url, h.label, h.platform, h.healthy
         FROM user_host_assignments uha
         JOIN hosts h ON h.id = uha.host_id
         WHERE uha.user_sub = $1
         ORDER BY uha.priority DESC, h.label",
    )
    .bind(&claims.sub)
    .fetch_all(&state.db)
    .await;

    match rows {
        Ok(rows) => {
            let mut vdis = Vec::new();

            for (host_id, host_url, host_label, platform, healthy) in rows {
                // Check if there's an active session for this user on this host
                let session = sqlx::query_as::<_, (String, i32, Option<String>, String)>(
                    "SELECT id, backend_port, os_user, status
                     FROM vdi_sessions
                     WHERE user_sub = $1 AND host_id = $2 AND status IN ('active', 'disconnected')
                     ORDER BY created_at DESC LIMIT 1",
                )
                .bind(&claims.sub)
                .bind(host_id)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten();

                let gw_origin = &state.config.gateway_origin;
                let session_info = session.map(|(sid, port, _os_user, status)| {
                    let stream_url = format!("{}/vdi/{}", gw_origin, sid);
                    streamio_types::UserVdiSession {
                        session_id: sid.clone(),
                        backend_port: port as u16,
                        stream_url,
                        status,
                    }
                });

                vdis.push(streamio_types::UserVdi {
                    host_id,
                    host_label: if healthy {
                        host_label
                    } else {
                        host_label.map(|l| format!("{} (offline)", l))
                    },
                    platform,
                    session: session_info,
                });
            }

            Json(vdis).into_response()
        }
        Err(e) => {
            error!("Failed to fetch user VDIs: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

/// POST /api/me/connect — create a VDI session on the specified host.
pub async fn connect(
    RequireSession(claims): RequireSession,
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let host_id = match req.get("host_id").and_then(|v| v.as_str()).and_then(|s| s.parse::<Uuid>().ok()) {
        Some(id) => id,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "host_id is required"})),
            )
                .into_response();
        }
    };

    // Verify user is assigned to this host
    let assigned = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM user_host_assignments WHERE user_sub = $1 AND host_id = $2",
    )
    .bind(&claims.sub)
    .bind(host_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    if assigned == 0 {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "Not assigned to this host"})),
        )
            .into_response();
    }

    // Check if user already has an active or disconnected session on this host
    let existing = sqlx::query_as::<_, (String, i32)>(
        "SELECT id, backend_port FROM vdi_sessions
         WHERE user_sub = $1 AND host_id = $2 AND status IN ('active', 'disconnected')
         LIMIT 1",
    )
    .bind(&claims.sub)
    .bind(host_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    if let Some((session_id, port)) = existing {
        // Reactivate and return existing session (backend is still running)
        let _ = sqlx::query("UPDATE vdi_sessions SET status = 'active', last_activity = now() WHERE id = $1")
            .bind(&session_id)
            .execute(&state.db)
            .await;

        let stream_url = format!("{}/vdi/{}", state.config.gateway_origin, session_id);

        info!("Reconnecting to existing session {} on port {}", session_id, port);
        return Json(serde_json::json!({
            "session_id": session_id,
            "stream_url": stream_url,
            "status": "reconnected",
        }))
        .into_response();
    }

    // Get host URL
    let host_url = match get_host_url(&state.db, host_id).await {
        Some(url) => url,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Host not found"})),
            )
                .into_response();
        }
    };

    // Call session manager to create session
    let width = req.get("width").and_then(|v| v.as_u64()).unwrap_or(1920) as u32;
    let height = req.get("height").and_then(|v| v.as_u64()).unwrap_or(1080) as u32;

    let session_req = streamio_types::SessionRequest {
        user_id: claims.sub.clone(),
        width,
        height,
        refresh_hz: 60,
    };

    let client = reqwest::Client::new();
    let create_url = format!("{}/api/sessions", host_url);

    info!(
        "Creating VDI session for {} on host {} ({}x{})",
        claims.sub, host_id, width, height
    );

    let resp = match client
        .post(&create_url)
        .json(&session_req)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to reach session manager at {}: {}", create_url, e);
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("Cannot reach host: {}", e)})),
            )
                .into_response();
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        error!("Session manager returned {}: {}", status, body);
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("Session manager error: {}", body)})),
        )
            .into_response();
    }

    let session_resp: streamio_types::SessionResponse = match resp.json().await {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to parse session response: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Invalid session manager response"})),
            )
                .into_response();
        }
    };

    // Store session in gateway DB
    let _ = sqlx::query(
        "INSERT INTO vdi_sessions (id, user_sub, user_email, host_id, backend_port, display_index, os_user, status)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'active')",
    )
    .bind(&session_resp.session_id)
    .bind(&claims.sub)
    .bind(&claims.email)
    .bind(host_id)
    .bind(session_resp.backend_port as i32)
    .bind(session_resp.display_index as i32)
    .bind(&session_resp.os_user)
    .execute(&state.db)
    .await;

    // Stream URL goes through the gateway — browser can't reach private IPs directly
    let stream_url = format!("{}/vdi/{}", state.config.gateway_origin, session_resp.session_id);

    info!(
        "VDI session {} created for {} → {}",
        session_resp.session_id, claims.sub, stream_url
    );

    Json(serde_json::json!({
        "session_id": session_resp.session_id,
        "stream_url": stream_url,
        "backend_port": session_resp.backend_port,
        "status": "created",
    }))
    .into_response()
}

/// POST /api/me/disconnect — destroy an active VDI session.
pub async fn disconnect(
    RequireSession(claims): RequireSession,
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let session_id = match req.get("session_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "session_id is required"})),
            )
                .into_response();
        }
    };

    // Verify this session belongs to the user
    let row = sqlx::query_as::<_, (Uuid,)>(
        "SELECT host_id FROM vdi_sessions WHERE id = $1 AND user_sub = $2 AND status = 'active'",
    )
    .bind(&session_id)
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await;

    let host_id = match row {
        Ok(Some((hid,))) => hid,
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Session not found or not yours"})),
            )
                .into_response();
        }
    };

    // Notify session manager (it keeps the backend alive, just marks inactive)
    if let Some(host_url) = get_host_url(&state.db, host_id).await {
        let client = reqwest::Client::new();
        let url = format!("{}/api/sessions/{}", host_url, session_id);
        let _ = client.delete(&url).send().await;
    }

    // Mark disconnected (not terminated — backend is still alive for reconnect)
    let _ = sqlx::query("UPDATE vdi_sessions SET status = 'disconnected' WHERE id = $1")
        .bind(&session_id)
        .execute(&state.db)
        .await;

    info!("Session {} disconnected for user {} (backend kept alive)", session_id, claims.sub);
    StatusCode::NO_CONTENT.into_response()
}

/// Serve the portal HTML page.
pub async fn portal_ui(RequireSession(_claims): RequireSession) -> impl IntoResponse {
    static PORTAL_HTML: &str = include_str!("../../client/portal.html");
    axum::response::Html(PORTAL_HTML)
}

// ── VDI session proxy ───────────────────────────────────────────────────────

/// GET /vdi/{session_id} — serve the stream HTML for a VDI session.
pub async fn vdi_stream_ui(
    RequireSession(_claims): RequireSession,
    Path(_session_id): Path<String>,
) -> impl IntoResponse {
    static SCREEN_HTML: &str = include_str!("../../client/screen.html");
    axum::response::Html(SCREEN_HTML)
}

/// GET /vdi/{session_id}/ws — proxy WebSocket to the session's backend.
pub async fn vdi_ws_proxy(
    ws: axum::extract::ws::WebSocketUpgrade,
    RequireSession(claims): RequireSession,
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> axum::response::Response {
    // Look up the session and verify ownership
    let row = sqlx::query_as::<_, (Uuid, i32)>(
        "SELECT host_id, backend_port FROM vdi_sessions WHERE id = $1 AND user_sub = $2 AND status = 'active'",
    )
    .bind(&session_id)
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await;

    let (host_id, backend_port) = match row {
        Ok(Some(r)) => r,
        _ => {
            return (StatusCode::NOT_FOUND, "Session not found").into_response();
        }
    };

    let host_url = match get_host_url(&state.db, host_id).await {
        Some(u) => u,
        None => return (StatusCode::NOT_FOUND, "Host not found").into_response(),
    };

    // Build the backend WebSocket URL
    // host_url is like "http://192.168.4.140:9100", backend is on a different port
    let host_base = host_url.split(':').take(2).collect::<Vec<_>>().join(":");
    let backend_ws_url = format!("{}:{}/ws", host_base.replacen("http", "ws", 1), backend_port);

    info!("Proxying VDI WS for session {} → {}", session_id, backend_ws_url);

    // Issue a token for the backend
    let token = match state.session.issue(
        claims.sub.clone(),
        claims.email.clone(),
        claims.role.clone(),
        None,
    ) {
        Ok(t) => t,
        Err(e) => {
            error!("Failed to issue JWT for VDI proxy: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    ws.on_upgrade(move |socket| vdi_proxy_websocket(socket, backend_ws_url, token))
}

async fn vdi_proxy_websocket(
    client_ws: axum::extract::ws::WebSocket,
    backend_url: String,
    token: String,
) {
    use futures::{SinkExt, StreamExt};
    use axum::extract::ws::Message;

    let request = match tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(&backend_url) {
        Ok(mut r) => {
            r.headers_mut().insert("X-Session-Token", token.parse().unwrap());
            r
        }
        Err(e) => {
            error!("Invalid backend WS URL {}: {}", backend_url, e);
            return;
        }
    };

    let backend_ws = match tokio_tungstenite::connect_async(request).await {
        Ok((ws, _)) => ws,
        Err(e) => {
            error!("Failed to connect to backend WS at {}: {}", backend_url, e);
            return;
        }
    };

    let (mut client_tx, mut client_rx) = client_ws.split();
    let (mut backend_tx, mut backend_rx) = backend_ws.split();

    let c2b = async {
        while let Some(msg) = client_rx.next().await {
            match msg {
                Ok(Message::Text(t)) => {
                    if backend_tx.send(tokio_tungstenite::tungstenite::Message::Text(t.to_string())).await.is_err() { break; }
                }
                Ok(Message::Binary(b)) => {
                    if backend_tx.send(tokio_tungstenite::tungstenite::Message::Binary(b.to_vec())).await.is_err() { break; }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    };

    let b2c = async {
        while let Some(msg) = backend_rx.next().await {
            match msg {
                Ok(tokio_tungstenite::tungstenite::Message::Text(t)) => {
                    if client_tx.send(Message::Text(t.into())).await.is_err() { break; }
                }
                Ok(tokio_tungstenite::tungstenite::Message::Binary(b)) => {
                    if client_tx.send(Message::Binary(b.into())).await.is_err() { break; }
                }
                Ok(tokio_tungstenite::tungstenite::Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    };

    tokio::select! {
        _ = c2b => {},
        _ = b2c => {},
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

async fn get_host_url(db: &sqlx::PgPool, host_id: Uuid) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT url FROM hosts WHERE id = $1")
        .bind(host_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
}
