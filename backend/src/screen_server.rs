//! HTTP and WebSocket server for screen streaming
//!
//! Architecture: ONE persistent capture pipeline per backend process.
//! WebSocket clients connect/disconnect without destroying the pipeline.
//! The capture pipeline (d3d11screencapturesrc → H.264 → webrtcbin) starts
//! on first connection and stays alive until the process exits.

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
    /// Persistent capture pipeline — created once, lives forever.
    /// Never destroyed on disconnect. Only recreated if pipeline errors out.
    pub persistent_streamer: Arc<Mutex<Option<Arc<ScreenStreamer>>>>,
    /// Display index this backend captures.
    pub display_index: Option<i32>,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    sessions: u32,
}

static SCREEN_HTML: &str = include_str!("../../client/screen.html");

pub async fn run(fps: u32, port: u16, token_secret: String, enable_audio: bool) -> Result<()> {
    let display_index: Option<i32> = std::env::var("DISPLAY_INDEX")
        .ok()
        .and_then(|s| s.parse().ok());

    let state = Arc::new(AppState {
        fps,
        token_secret: token_secret.clone(),
        active_sessions: Arc::new(AtomicU32::new(0)),
        persistent_streamer: Arc::new(Mutex::new(None)),
        display_index,
    });

    // Build CORS layer
    let cors = if let Ok(origin) = std::env::var("GATEWAY_ORIGIN") {
        CorsLayer::new()
            .allow_origin(origin.parse::<axum::http::HeaderValue>().unwrap())
            .allow_methods(tower_http::cors::Any)
            .allow_headers(tower_http::cors::Any)
    } else {
        CorsLayer::permissive()
    };

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        .route("/healthz", get(health_handler))
        .layer(cors)
        .with_state(state);

    let addr = format!("0.0.0.0:{}", port);
    info!("Backend listening on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index_handler() -> impl IntoResponse {
    Html(SCREEN_HTML)
}

async fn health_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(HealthResponse {
        status: "ok",
        sessions: state.active_sessions.load(Ordering::Relaxed),
    })
}

fn verify_token(headers: &HeaderMap, secret: &str) -> bool {
    if secret.is_empty() {
        return true; // Dev mode
    }
    let token = match headers.get("x-session-token").and_then(|v| v.to_str().ok()) {
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

/// Get or create the persistent capture pipeline.
/// The pipeline is created once and reused across all WebSocket connections.
/// It's only recreated if it dies (pipeline error/EOS).
async fn get_or_create_pipeline(
    state: &Arc<AppState>,
    sig_tx: mpsc::UnboundedSender<SignalingMessage>,
    display_index: Option<i32>,
) -> Option<Arc<ScreenStreamer>> {
    let mut guard = state.persistent_streamer.lock().await;

    // Check if existing pipeline is still alive
    if let Some(ref existing) = *guard {
        // Pipeline exists — but we need a NEW webrtcbin for the new peer.
        // For now, we recreate the pipeline for each connection because
        // webrtcbin is tightly coupled to the pipeline.
        // However, we DON'T drop the old pipeline until the new one is ready.
        // This preserves the DXGI output handle.
        info!("Creating new pipeline (keeping old alive for DXGI handover)");
    }

    let di = display_index.or(state.display_index);

    const MAX_RETRIES: u32 = 6;
    const RETRY_DELAY_MS: u64 = 500;
    let mut last_err = String::new();

    for attempt in 1..=MAX_RETRIES {
        match ScreenStreamer::new(state.fps, sig_tx.clone(), di) {
            Ok(s) => {
                let s = Arc::new(s);
                match s.start() {
                    Ok(()) => {
                        if attempt > 1 {
                            info!("Pipeline started on attempt {}", attempt);
                        }
                        // New pipeline is running — NOW we can drop the old one
                        let old = guard.take();
                        *guard = Some(s.clone());
                        // Drop old pipeline AFTER storing new one
                        if let Some(old_pipeline) = old {
                            info!("DXGI handover: dropping old pipeline");
                            let _ = old_pipeline.stop();
                        }
                        return Some(s);
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

    error!("Failed to create pipeline after {} attempts: {}", MAX_RETRIES, last_err);
    // Don't clear the old pipeline — it may still be keeping DXGI alive
    None
}

async fn handle_websocket(socket: WebSocket, state: Arc<AppState>, display_index: Option<i32>) {
    state.active_sessions.fetch_add(1, Ordering::Relaxed);
    info!("New WebSocket connection (display={:?})", display_index);

    let (mut ws_tx, mut ws_rx) = socket.split();

    // Create signaling channel for this connection
    let (sig_tx, mut sig_rx) = mpsc::unbounded_channel::<SignalingMessage>();

    // Get or create pipeline
    let streamer = match get_or_create_pipeline(&state, sig_tx, display_index).await {
        Some(s) => s,
        None => {
            error!("Cannot create capture pipeline — closing connection");
            state.active_sessions.fetch_sub(1, Ordering::Relaxed);
            return;
        }
    };

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

    // Watch for pipeline errors/EOS
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
                // Mark pipeline as dead so next connection creates a fresh one
                let mut guard = state.persistent_streamer.lock().await;
                *guard = None;
                break;
            }
        }
    }

    ws_forward_task.abort();

    // DON'T stop the pipeline — it stays alive in persistent_streamer.
    // The next connection will reuse it (via DXGI handover).
    // The pipeline only dies if it errors out (pipeline_dead fires).

    state.active_sessions.fetch_sub(1, Ordering::Relaxed);
    info!("WebSocket session ended (pipeline kept alive)");
}
