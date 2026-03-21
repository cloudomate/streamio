//! HTTP and WebSocket server for screen streaming

use crate::input::InputController;
use crate::screen_capture::ScreenStreamer;
use anyhow::Result;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        Query, State, WebSocketUpgrade,
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
use tokio::sync::{mpsc, Mutex};
use tower_http::cors::CorsLayer;
use tracing::{error, info, warn};

/// Start input handling thread and return sender.
/// Input events are forwarded as-is to the input pipe.
/// Coordinate translation is handled by the session manager (if present).
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
    /// Hold previous pipeline alive so DXGI output stays enumerable for the next one.
    /// On virtual displays, d3d11screencapturesrc's DXGI output handle disappears
    /// once the pipeline that opened it is destroyed. We keep the old pipeline alive
    /// until the new one has successfully started and acquired its own DXGI handle.
    pub prev_streamer: Arc<Mutex<Option<Arc<ScreenStreamer>>>>,
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
        prev_streamer: Arc::new(Mutex::new(None)),
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

    // Use SO_REUSEADDR to allow binding even if a previous process left orphaned sockets
    let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None)?;
    socket.set_reuse_address(true)?;
    socket.bind(&addr.into())?;
    socket.listen(128)?;
    socket.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(socket.into())?;

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
    Query(params): Query<std::collections::HashMap<String, String>>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !verify_token(&headers, &state.token_secret) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let display_index: Option<i32> = params.get("display").and_then(|s| s.parse().ok());
    ws.on_upgrade(move |socket| handle_websocket(socket, state, display_index))
}

async fn handle_websocket(socket: WebSocket, state: Arc<AppState>, display_index: Option<i32>) {
    state.active_sessions.fetch_add(1, Ordering::Relaxed);
    info!("New WebSocket connection (display={:?})", display_index);

    let (mut ws_tx, mut ws_rx) = socket.split();

    // Create new pipeline while the previous one is still alive (holds DXGI output open).
    // Retry because DXGI enumeration of virtual displays can take a moment to stabilize
    // even when the old pipeline is keeping the output alive.
    let (streamer, mut sig_rx) = {
        const MAX_RETRIES: u32 = 6;
        const RETRY_DELAY_MS: u64 = 500;
        let mut last_err = String::new();
        let mut result = None;

        for attempt in 1..=MAX_RETRIES {
            let (sig_tx, sig_rx_inner) = mpsc::unbounded_channel::<SignalingMessage>();
            match ScreenStreamer::new(state.fps, sig_tx, display_index) {
                Ok(s) => {
                    let s = Arc::new(s);
                    match s.start() {
                        Ok(()) => {
                            if attempt > 1 {
                                info!("Pipeline started on attempt {}", attempt);
                            }
                            result = Some((s, sig_rx_inner));
                            break;
                        }
                        Err(e) => {
                            last_err = format!("start failed: {}", e);
                            warn!("Pipeline attempt {}/{}: {}", attempt, MAX_RETRIES, last_err);
                            let _ = s.stop();
                        }
                    }
                }
                Err(e) => {
                    last_err = format!("create failed: {}", e);
                    warn!("Pipeline attempt {}/{}: {}", attempt, MAX_RETRIES, last_err);
                }
            }
            if attempt < MAX_RETRIES {
                tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
            }
        }

        match result {
            Some(r) => r,
            None => {
                error!("Failed to start pipeline after {} attempts: {}", MAX_RETRIES, last_err);
                state.active_sessions.fetch_sub(1, Ordering::Relaxed);
                return;
            }
        }
    };

    // New pipeline started successfully — now we can drop the previous one.
    {
        let mut prev = state.prev_streamer.lock().await;
        if let Some(old) = prev.take() {
            info!("Dropping previous pipeline (DXGI handover complete)");
            let _ = old.stop();
        }
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

    // Watch for pipeline errors/EOS (e.g., screen lock kills capture)
    let mut pipeline_dead = streamer.watch_for_errors();

    let input_tx = start_input_thread();
    let streamer_msg = streamer.clone();

    loop {
        tokio::select! {
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
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
                    Some(Ok(Message::Close(_))) => {
                        info!("WebSocket closed by client");
                        break;
                    }
                    Some(Err(e)) => {
                        error!("WebSocket error: {}", e);
                        break;
                    }
                    None => {
                        info!("WebSocket stream ended");
                        break;
                    }
                    _ => {}
                }
            }
            reason = &mut pipeline_dead => {
                let reason = reason.unwrap_or_else(|_| "unknown".to_string());
                warn!("Pipeline died ({}), closing WebSocket for client reconnect", reason);
                break;
            }
        }
    }

    ws_forward_task.abort();

    // Don't stop the pipeline — keep it alive so the next connection can
    // create a new d3d11screencapturesrc while DXGI output is still held open.
    // Store in prev_streamer; it will be dropped when the next pipeline starts.
    {
        let mut prev = state.prev_streamer.lock().await;
        info!("Preserving pipeline for DXGI handover to next connection");
        *prev = Some(streamer);
    }

    state.active_sessions.fetch_sub(1, Ordering::Relaxed);
    info!("WebSocket session ended");
}
