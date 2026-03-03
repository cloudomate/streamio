//! HTTP and WebSocket server for screen streaming

use crate::input::InputController;
use crate::screen_capture::ScreenStreamer;
use anyhow::Result;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use futures::{SinkExt, StreamExt};
use jsonwebtoken::{decode, DecodingKey, Validation};
use serde::Serialize;
use std::{
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};
use streamio_types::{InputEvent, SessionClaims, SignalingMessage};
use tokio::sync::mpsc;
use tower_http::cors::CorsLayer;
use tracing::{error, info, warn};

/// Start input handling thread and return sender.
fn start_input_thread() -> mpsc::UnboundedSender<InputEvent> {
    let (tx, mut rx) = mpsc::unbounded_channel::<InputEvent>();
    std::thread::spawn(move || {
        let controller = InputController::new();
        while let Some(event) = rx.blocking_recv() {
            controller.handle_event(&event);
        }
    });
    tx
}

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub fps: u32,
    pub token_secret: String,
    pub active_sessions: Arc<AtomicU32>,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    sessions: u32,
}

/// Run the HTTP/WebSocket server.
pub async fn run_server(fps: u32, port: u16) -> Result<()> {
    let token_secret = std::env::var("BACKEND_TOKEN_SECRET")
        .unwrap_or_else(|_| String::new()); // empty = no auth (dev mode)

    let gateway_origin: Option<String> = std::env::var("GATEWAY_ORIGIN").ok();

    let state = Arc::new(AppState {
        fps,
        token_secret,
        active_sessions: Arc::new(AtomicU32::new(0)),
    });

    // CORS: restrict to gateway origin if configured, else permissive (dev mode)
    let cors = match gateway_origin {
        Some(ref origin) => CorsLayer::new()
            .allow_origin(
                origin
                    .parse::<axum::http::HeaderValue>()
                    .expect("invalid GATEWAY_ORIGIN"),
            ),
        None => CorsLayer::permissive(),
    };

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        .route("/healthz", get(health_handler))
        .layer(cors)
        .with_state(state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    info!("Backend listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index_handler() -> Html<&'static str> {
    Html(include_str!("../../client/screen.html"))
}

async fn health_handler(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        sessions: state.active_sessions.load(Ordering::Relaxed),
    })
}

/// Verify the X-Session-Token header if a token secret is configured.
fn verify_token(headers: &HeaderMap, secret: &str) -> bool {
    if secret.is_empty() {
        // Dev mode — no auth required
        return true;
    }
    let token = match headers.get("X-Session-Token").and_then(|v| v.to_str().ok()) {
        Some(t) => t,
        None => return false,
    };
    decode::<SessionClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .is_ok()
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !verify_token(&headers, &state.token_secret) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| handle_websocket(socket, state))
}

async fn handle_websocket(socket: WebSocket, state: Arc<AppState>) {
    state.active_sessions.fetch_add(1, Ordering::Relaxed);
    info!("New WebSocket connection");

    let (mut ws_tx, mut ws_rx) = socket.split();
    let (sig_tx, mut sig_rx) = mpsc::unbounded_channel::<SignalingMessage>();

    let streamer = match ScreenStreamer::new(state.fps, sig_tx) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            error!("Failed to create screen streamer: {}", e);
            state.active_sessions.fetch_sub(1, Ordering::Relaxed);
            return;
        }
    };

    if let Err(e) = streamer.start() {
        error!("Failed to start pipeline: {}", e);
        state.active_sessions.fetch_sub(1, Ordering::Relaxed);
        return;
    }

    // Forward outgoing signaling to WebSocket
    let ws_forward_task = tokio::spawn(async move {
        while let Some(msg) = sig_rx.recv().await {
            let json = serde_json::to_string(&msg).unwrap();
            if ws_tx.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
    });

    // Send offer after short delay to allow pipeline setup
    let streamer_offer = streamer.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        streamer_offer.create_offer();
    });

    let input_tx = start_input_thread();
    let streamer_msg = streamer.clone();

    while let Some(msg) = ws_rx.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                if let Ok(sig_msg) = serde_json::from_str::<SignalingMessage>(&text) {
                    if let Err(e) = streamer_msg.handle_signaling(sig_msg) {
                        error!("Signaling error: {}", e);
                    }
                    continue;
                }
                if let Ok(input_event) = serde_json::from_str::<InputEvent>(&text) {
                    let _ = input_tx.send(input_event);
                    continue;
                }
                warn!("Unknown message: {}", text);
            }
            Ok(Message::Close(_)) => {
                info!("WebSocket closed by client");
                break;
            }
            Err(e) => {
                error!("WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }

    ws_forward_task.abort();
    if let Err(e) = streamer.stop() {
        error!("Failed to stop streamer: {}", e);
    }

    state.active_sessions.fetch_sub(1, Ordering::Relaxed);
    info!("WebSocket session ended");
}
