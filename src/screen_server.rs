//! HTTP and WebSocket server for screen streaming

use crate::input::{InputController, InputEvent};
use crate::screen_capture::{ScreenStreamer, SignalingMessage};
use anyhow::Result;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::mpsc;
use tower_http::cors::CorsLayer;

/// Start input handling thread and return sender
fn start_input_thread() -> mpsc::UnboundedSender<InputEvent> {
    let (tx, mut rx) = mpsc::unbounded_channel::<InputEvent>();

    // Spawn a blocking thread for input handling (Enigo is not Send)
    std::thread::spawn(move || {
        let controller = InputController::new();
        while let Some(event) = rx.blocking_recv() {
            controller.handle_event(&event);
        }
    });

    tx
}

/// Shared application state
pub struct AppState {
    pub fps: u32,
}

/// Run the HTTP/WebSocket server
pub async fn run_server(fps: u32, port: u16) -> Result<()> {
    let state = Arc::new(AppState { fps });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("Server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Serve the client HTML page
async fn index_handler() -> Html<&'static str> {
    Html(include_str!("../client/screen.html"))
}

/// Handle WebSocket connections
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_websocket(socket, state))
}

/// Handle a WebSocket session
async fn handle_websocket(socket: WebSocket, state: Arc<AppState>) {
    tracing::info!("New WebSocket connection");

    let (mut ws_tx, mut ws_rx) = socket.split();

    // Channel for outgoing signaling messages
    let (sig_tx, mut sig_rx) = mpsc::unbounded_channel::<SignalingMessage>();

    // Create screen streamer
    let streamer = match ScreenStreamer::new(state.fps, sig_tx) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            tracing::error!("Failed to create screen streamer: {}", e);
            return;
        }
    };

    // Start the pipeline
    if let Err(e) = streamer.start() {
        tracing::error!("Failed to start pipeline: {}", e);
        return;
    }

    // Task to forward outgoing signaling messages to WebSocket
    let ws_forward_task = tokio::spawn(async move {
        while let Some(msg) = sig_rx.recv().await {
            let json = serde_json::to_string(&msg).unwrap();
            if ws_tx.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
    });

    // Create offer after a short delay
    let streamer_offer = streamer.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        streamer_offer.create_offer();
    });

    // Start input handling on dedicated thread
    let input_tx = start_input_thread();

    // Handle incoming WebSocket messages
    let streamer_msg = streamer.clone();

    while let Some(msg) = ws_rx.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                // Try to parse as signaling message
                if let Ok(sig_msg) = serde_json::from_str::<SignalingMessage>(&text) {
                    if let Err(e) = streamer_msg.handle_signaling(sig_msg) {
                        tracing::error!("Signaling error: {}", e);
                    }
                    continue;
                }

                // Try to parse as input event
                if let Ok(input_event) = serde_json::from_str::<InputEvent>(&text) {
                    let _ = input_tx.send(input_event);
                    continue;
                }

                tracing::warn!("Unknown message: {}", text);
            }
            Ok(Message::Close(_)) => {
                tracing::info!("WebSocket closed by client");
                break;
            }
            Err(e) => {
                tracing::error!("WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }

    // Cleanup
    ws_forward_task.abort();
    if let Err(e) = streamer.stop() {
        tracing::error!("Failed to stop streamer: {}", e);
    }

    tracing::info!("WebSocket session ended");
}
