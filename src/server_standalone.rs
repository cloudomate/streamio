//! HTTP and WebSocket server for standalone build

use crate::renderer::{HorizonRenderer, InputEvent};
use crate::streamer_standalone::{SignalingMessage, WebRtcStreamer};
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
use std::time::Instant;
use tokio::sync::mpsc;
use tower_http::cors::CorsLayer;

/// Shared application state
pub struct AppState {
    pub renderer: Arc<HorizonRenderer>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// Run the HTTP/WebSocket server
pub async fn run_server(
    renderer: Arc<HorizonRenderer>,
    width: u32,
    height: u32,
    fps: u32,
    port: u16,
) -> Result<()> {
    let state = Arc::new(AppState {
        renderer,
        width,
        height,
        fps,
    });

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
    Html(include_str!("../client/index_standalone.html"))
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

    // Create WebRTC streamer
    let streamer = match WebRtcStreamer::new(state.width, state.height, state.fps, sig_tx).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            tracing::error!("Failed to create streamer: {}", e);
            return;
        }
    };

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
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if let Err(e) = streamer_offer.create_offer().await {
            tracing::error!("Failed to create offer: {}", e);
        }
    });

    // Start render loop after connection is established
    let renderer = state.renderer.clone();
    let streamer_render = streamer.clone();
    let fps = state.fps;
    let render_task = tokio::spawn(async move {
        let frame_duration = std::time::Duration::from_secs_f64(1.0 / fps as f64);

        // Wait for connection to be established
        tracing::info!("Render loop waiting for connection...");
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        tracing::info!("Starting render loop at {} fps", fps);

        let mut frame_count: u64 = 0;
        let start_time = Instant::now();

        loop {
            let frame_start = Instant::now();

            // Render frame
            match renderer.render_frame().await {
                Ok(rgba_data) => {
                    match streamer_render.push_frame(&rgba_data).await {
                        Ok(_) => {
                            frame_count += 1;
                            if frame_count % 30 == 0 {
                                let elapsed = start_time.elapsed().as_secs_f64();
                                let actual_fps = frame_count as f64 / elapsed;
                                tracing::info!("Frames sent: {}, FPS: {:.1}", frame_count, actual_fps);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to push frame {}: {}", frame_count, e);
                            break;
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Render error: {}", e);
                    break;
                }
            }

            // Maintain frame rate
            let elapsed = frame_start.elapsed();
            if elapsed < frame_duration {
                tokio::time::sleep(frame_duration - elapsed).await;
            }
        }
    });

    // Handle incoming WebSocket messages
    let renderer = state.renderer.clone();
    let streamer_msg = streamer.clone();

    while let Some(msg) = ws_rx.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                // Try to parse as signaling message
                if let Ok(sig_msg) = serde_json::from_str::<SignalingMessage>(&text) {
                    if let Err(e) = streamer_msg.handle_signaling(sig_msg).await {
                        tracing::error!("Signaling error: {}", e);
                    }
                    continue;
                }

                // Try to parse as input event
                if let Ok(input) = serde_json::from_str::<InputEvent>(&text) {
                    renderer.handle_input(&input);
                    continue;
                }

                tracing::warn!("Unknown message: {}", text);
            }
            Ok(Message::Close(_)) => {
                tracing::info!("WebSocket closed");
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
    render_task.abort();
    ws_forward_task.abort();
    if let Err(e) = streamer.close().await {
        tracing::error!("Failed to close streamer: {}", e);
    }

    tracing::info!("WebSocket session ended");
}
