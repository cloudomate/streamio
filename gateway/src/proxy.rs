use crate::{middleware::RequireSession, AppState};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::{Html, IntoResponse, Redirect, Response},
};
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite};
use tracing::error;

static SCREEN_HTML: &str = include_str!("../../../client/screen.html");

/// GET / — serve the stream UI (requires session) or redirect to login.
pub async fn index_handler(session: Option<RequireSession>) -> Response {
    match session {
        Some(_) => Html(SCREEN_HTML).into_response(),
        None => Redirect::to("/auth/login").into_response(),
    }
}

/// GET /ws — proxy WebSocket to the user's assigned backend.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    RequireSession(claims): RequireSession,
    State(state): State<AppState>,
) -> Response {
    // Resolve backend assignment (auto-provisions a KubeVirt VM on first login if enabled)
    let backend = match state
        .registry
        .get_or_assign(
            &claims.sub,
            state.provisioner.as_ref(),
            state.default_vm.as_deref(),
        )
        .await
    {
        Some(b) => b,
        None => {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "No healthy backend available",
            )
                .into_response()
        }
    };

    // Re-issue JWT with backend_id stamped in (so backend knows the assignment)
    let token = match state.session.issue(
        claims.sub.clone(),
        claims.email.clone(),
        claims.role.clone(),
        Some(backend.id),
    ) {
        Ok(t) => t,
        Err(e) => {
            error!("Failed to re-issue JWT for proxy: {e}");
            return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let backend_ws_url = format!(
        "{}/ws",
        backend.url.replacen("http", "ws", 1)
    );

    ws.on_upgrade(move |socket| proxy_websocket(socket, backend_ws_url, token))
}

/// Splice two WebSocket connections bidirectionally.
async fn proxy_websocket(client_ws: WebSocket, backend_url: String, token: String) {
    // Connect to backend WebSocket with the auth token in a header
    let request = tungstenite::client::IntoClientRequest::into_client_request(&backend_url)
        .expect("invalid backend WS URL");
    let mut request = request;
    request
        .headers_mut()
        .insert("X-Session-Token", token.parse().unwrap());

    let backend_ws = match connect_async(request).await {
        Ok((ws, _)) => ws,
        Err(e) => {
            error!("Failed to connect to backend WS at {backend_url}: {e}");
            return;
        }
    };

    let (mut client_tx, mut client_rx) = client_ws.split();
    let (mut backend_tx, mut backend_rx) = backend_ws.split();

    // Forward: client → backend
    let c2b = async {
        while let Some(msg) = client_rx.next().await {
            match msg {
                Ok(Message::Text(t)) => {
                    if backend_tx
                        .send(tungstenite::Message::Text(t.to_string()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(Message::Binary(b)) => {
                    if backend_tx
                        .send(tungstenite::Message::Binary(b.to_vec()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    };

    // Forward: backend → client
    let b2c = async {
        while let Some(msg) = backend_rx.next().await {
            match msg {
                Ok(tungstenite::Message::Text(t)) => {
                    if client_tx.send(Message::Text(t.into())).await.is_err() {
                        break;
                    }
                }
                Ok(tungstenite::Message::Binary(b)) => {
                    if client_tx.send(Message::Binary(b.into())).await.is_err() {
                        break;
                    }
                }
                Ok(tungstenite::Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    };

    tokio::select! {
        _ = c2b => {},
        _ = b2c => {},
    }
}
