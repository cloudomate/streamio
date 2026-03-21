//! Host machine management — session manager agent registry.
//!
//! Hosts are machines running `streamio-session-manager`. They register
//! with the gateway and accept session creation requests. Admins assign
//! users to hosts; the gateway proxies connections accordingly.

use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use tracing::{error, info, warn};
use uuid::Uuid;

// ── Host registration (called by session manager agents) ────────────────────

pub async fn register_host(
    State(state): State<AppState>,
    Json(req): Json<streamio_types::HostRegisterRequest>,
) -> impl IntoResponse {
    info!("Host registering: {} ({:?}) at {}", req.id, req.platform, req.url);

    let platform = serde_json::to_value(&req.platform)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "windows".to_string());

    let result = sqlx::query(
        "INSERT INTO hosts (id, url, label, platform, healthy, last_seen, max_sessions)
         VALUES ($1, $2, $3, $4, true, now(), $5)
         ON CONFLICT (id) DO UPDATE SET url = $2, label = COALESCE($3, hosts.label),
         platform = $4, healthy = true, last_seen = now(), max_sessions = $5",
    )
    .bind(req.id)
    .bind(&req.url)
    .bind(&req.label)
    .bind(&platform)
    .bind(req.max_sessions as i32)
    .execute(&state.db)
    .await;

    match result {
        Ok(_) => {
            info!("Host {} registered successfully", req.id);
            StatusCode::OK.into_response()
        }
        Err(e) => {
            error!("Failed to register host: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

/// Heartbeat endpoint — session managers call this periodically.
pub async fn host_heartbeat(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let result = sqlx::query(
        "UPDATE hosts SET healthy = true, last_seen = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&state.db)
    .await;

    match result {
        Ok(r) if r.rows_affected() > 0 => StatusCode::OK,
        _ => StatusCode::NOT_FOUND,
    }
}

// ── Admin API — host management ─────────────────────────────────────────────

pub async fn list_hosts(State(state): State<AppState>) -> impl IntoResponse {
    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, String, bool, i32)>(
        "SELECT h.id, h.url, h.label, h.platform, h.healthy, h.max_sessions
         FROM hosts h ORDER BY h.label, h.url",
    )
    .fetch_all(&state.db)
    .await;

    match rows {
        Ok(rows) => {
            // Count active sessions per host
            let session_counts = sqlx::query_as::<_, (Uuid, i64)>(
                "SELECT host_id, COUNT(*) FROM vdi_sessions WHERE status = 'active' GROUP BY host_id",
            )
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();

            let hosts: Vec<streamio_types::HostInfo> = rows
                .into_iter()
                .map(|(id, url, label, platform, healthy, max_sessions)| {
                    let active = session_counts
                        .iter()
                        .find(|(hid, _)| *hid == id)
                        .map(|(_, c)| *c as u32)
                        .unwrap_or(0);
                    streamio_types::HostInfo {
                        id,
                        url,
                        label,
                        platform: match platform.as_str() {
                            "linux" => streamio_types::HostPlatform::Linux,
                            "macos" | "mac_os" => streamio_types::HostPlatform::MacOs,
                            _ => streamio_types::HostPlatform::Windows,
                        },
                        healthy,
                        max_sessions: max_sessions as u32,
                        active_sessions: active,
                    }
                })
                .collect();

            Json(hosts).into_response()
        }
        Err(e) => {
            error!("Failed to list hosts: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

pub async fn create_host(
    State(state): State<AppState>,
    Json(req): Json<streamio_types::HostRegisterRequest>,
) -> impl IntoResponse {
    // Same as register but for admin manual creation
    register_host(State(state), Json(req)).await
}

pub async fn delete_host(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    // First terminate any active sessions on this host
    let _ = sqlx::query("UPDATE vdi_sessions SET status = 'terminated' WHERE host_id = $1 AND status = 'active'")
        .bind(id)
        .execute(&state.db)
        .await;

    let result = sqlx::query("DELETE FROM hosts WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await;

    match result {
        Ok(r) if r.rows_affected() > 0 => {
            info!("Host {} deleted", id);
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            error!("Failed to delete host: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

// ── Admin API — known users (for assignment dropdowns) ──────────────────────

pub async fn list_known_users(State(state): State<AppState>) -> impl IntoResponse {
    let rows = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
        "SELECT sub, email, display_name FROM known_users ORDER BY email, sub",
    )
    .fetch_all(&state.db)
    .await;

    match rows {
        Ok(rows) => {
            let users: Vec<serde_json::Value> = rows
                .into_iter()
                .map(|(sub, email, display_name)| {
                    serde_json::json!({
                        "sub": sub,
                        "email": email,
                        "display_name": display_name,
                    })
                })
                .collect();
            Json(users).into_response()
        }
        Err(e) => {
            error!("Failed to list known users: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

// ── Admin API — user-host assignments ───────────────────────────────────────

pub async fn list_host_assignments(State(state): State<AppState>) -> impl IntoResponse {
    let rows = sqlx::query_as::<_, (String, Option<String>, Uuid, Option<String>, i32)>(
        "SELECT uha.user_sub, ku.email, uha.host_id, h.label, uha.priority
         FROM user_host_assignments uha
         JOIN hosts h ON h.id = uha.host_id
         LEFT JOIN known_users ku ON ku.sub = uha.user_sub
         ORDER BY ku.email, uha.priority DESC",
    )
    .fetch_all(&state.db)
    .await;

    match rows {
        Ok(rows) => {
            let assignments: Vec<serde_json::Value> = rows
                .into_iter()
                .map(|(user_sub, email, host_id, host_label, priority)| {
                    serde_json::json!({
                        "user_sub": user_sub,
                        "email": email,
                        "host_id": host_id,
                        "host_label": host_label,
                        "priority": priority,
                    })
                })
                .collect();
            Json(assignments).into_response()
        }
        Err(e) => {
            error!("Failed to list host assignments: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

pub async fn create_host_assignment(
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let host_id = match req.get("host_id").and_then(|v| v.as_str()).and_then(|s| s.parse::<Uuid>().ok()) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "host_id required").into_response(),
    };
    let priority = req.get("priority").and_then(|v| v.as_i64()).unwrap_or(0) as i32;

    // Accept either user_sub directly or email (resolve email → sub)
    let user_sub = if let Some(sub) = req.get("user_sub").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        sub.to_string()
    } else if let Some(email) = req.get("email").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        // Look up sub by email
        match sqlx::query_scalar::<_, String>("SELECT sub FROM known_users WHERE email = $1")
            .bind(email)
            .fetch_optional(&state.db)
            .await
        {
            Ok(Some(sub)) => sub,
            Ok(None) => {
                return (
                    StatusCode::NOT_FOUND,
                    format!("User '{}' not found. They must log in at least once first.", email),
                ).into_response();
            }
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    } else {
        return (StatusCode::BAD_REQUEST, "user_sub or email required").into_response();
    };

    let result = sqlx::query(
        "INSERT INTO user_host_assignments (user_sub, host_id, priority)
         VALUES ($1, $2, $3)
         ON CONFLICT (user_sub, host_id) DO UPDATE SET priority = $3",
    )
    .bind(&user_sub)
    .bind(host_id)
    .bind(priority)
    .execute(&state.db)
    .await;

    match result {
        Ok(_) => {
            info!("Assigned user {} to host {} (priority={})", user_sub, host_id, priority);
            StatusCode::CREATED.into_response()
        }
        Err(e) => {
            error!("Failed to create assignment: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

pub async fn delete_host_assignment(
    State(state): State<AppState>,
    Path((sub, host_id)): Path<(String, Uuid)>,
) -> impl IntoResponse {
    let result = sqlx::query(
        "DELETE FROM user_host_assignments WHERE user_sub = $1 AND host_id = $2",
    )
    .bind(&sub)
    .bind(host_id)
    .execute(&state.db)
    .await;

    match result {
        Ok(r) if r.rows_affected() > 0 => StatusCode::NO_CONTENT.into_response(),
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── Admin API — VDI session management ──────────────────────────────────────

pub async fn list_vdi_sessions(State(state): State<AppState>) -> impl IntoResponse {
    let rows = sqlx::query_as::<_, (String, String, Option<String>, Uuid, Option<String>, i32, Option<String>, String, String)>(
        "SELECT vs.id, vs.user_sub, vs.user_email, vs.host_id, h.label, vs.backend_port,
                vs.os_user, vs.status, vs.created_at::text
         FROM vdi_sessions vs
         JOIN hosts h ON h.id = vs.host_id
         WHERE vs.status = 'active'
         ORDER BY vs.created_at DESC",
    )
    .fetch_all(&state.db)
    .await;

    match rows {
        Ok(rows) => {
            let sessions: Vec<streamio_types::VdiSessionInfo> = rows
                .into_iter()
                .map(
                    |(id, user_sub, user_email, host_id, host_label, backend_port, os_user, status, created_at)| {
                        streamio_types::VdiSessionInfo {
                            id,
                            user_sub,
                            user_email,
                            host_id,
                            host_label,
                            backend_port: backend_port as u16,
                            os_user,
                            status,
                            created_at,
                        }
                    },
                )
                .collect();
            Json(sessions).into_response()
        }
        Err(e) => {
            error!("Failed to list VDI sessions: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

pub async fn kill_vdi_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    // Look up the session to find its host
    let row = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT host_id, status FROM vdi_sessions WHERE id = $1",
    )
    .bind(&session_id)
    .fetch_optional(&state.db)
    .await;

    match row {
        Ok(Some((host_id, _status))) => {
            // Get host URL
            let host_url = sqlx::query_scalar::<_, String>(
                "SELECT url FROM hosts WHERE id = $1",
            )
            .bind(host_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();

            // Call session manager to destroy the session
            if let Some(url) = host_url {
                let client = reqwest::Client::new();
                let destroy_url = format!("{}/api/sessions/{}", url, session_id);
                match client.delete(&destroy_url).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        info!("Session {} destroyed on host {}", session_id, host_id);
                    }
                    Ok(resp) => {
                        warn!(
                            "Session manager returned {} for destroy {}",
                            resp.status(),
                            session_id
                        );
                    }
                    Err(e) => {
                        warn!("Failed to reach session manager for {}: {}", session_id, e);
                    }
                }
            }

            // Mark session as terminated in DB
            let _ = sqlx::query(
                "UPDATE vdi_sessions SET status = 'terminated' WHERE id = $1",
            )
            .bind(&session_id)
            .execute(&state.db)
            .await;

            StatusCode::NO_CONTENT.into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            error!("Failed to lookup session: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

// ── Host health polling ─────────────────────────────────────────────────────

pub async fn host_health_poll_task(db: sqlx::PgPool) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;

        let hosts = sqlx::query_as::<_, (Uuid, String)>("SELECT id, url FROM hosts")
            .fetch_all(&db)
            .await;

        if let Ok(hosts) = hosts {
            for (id, url) in hosts {
                let health_url = format!("{}/api/sessions", url);
                let healthy = client.get(&health_url).send().await.map(|r| r.status().is_success()).unwrap_or(false);

                let _ = sqlx::query(
                    "UPDATE hosts SET healthy = $1, last_seen = CASE WHEN $1 THEN now() ELSE last_seen END WHERE id = $2",
                )
                .bind(healthy)
                .bind(id)
                .execute(&db)
                .await;
            }
        }
    }
}
