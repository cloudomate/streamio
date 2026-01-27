//! Screen Streamer - VDI-style screen capture and WebRTC streaming
//!
//! Captures the screen/display and streams via WebRTC to browsers.

mod input;
mod screen_capture;
mod screen_server;

use anyhow::Result;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

const DEFAULT_FPS: u32 = 30;
const DEFAULT_PORT: u16 = 8123;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Initialize GStreamer
    gstreamer::init()?;

    // Read config from environment
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let fps: u32 = std::env::var("FPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_FPS);

    tracing::info!("Screen Streamer v{}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Capturing screen at {} fps", fps);
    tracing::info!("Open http://localhost:{} to view", port);

    // Start server
    screen_server::run_server(fps, port).await?;

    Ok(())
}
