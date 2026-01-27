//! Horizon Streamer - Standalone build
//!
//! Single binary with no external dependencies.
//! Uses webrtc-rs and OpenH264 instead of GStreamer.

mod renderer;
mod server_standalone;
mod streamer_standalone;

use anyhow::Result;
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Default configuration
const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;
const DEFAULT_FPS: u32 = 30;
const DEFAULT_PORT: u16 = 8123;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,wgpu=warn".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Read config from environment
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let width: u32 = std::env::var("WIDTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_WIDTH);

    let height: u32 = std::env::var("HEIGHT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_HEIGHT);

    let fps: u32 = std::env::var("FPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_FPS);

    tracing::info!("Horizon Streamer v{} (Standalone)", env!("CARGO_PKG_VERSION"));
    tracing::info!("Resolution: {}x{} @ {} fps", width, height, fps);
    tracing::info!("Using OpenH264 encoder + webrtc-rs (no GStreamer)");

    // Initialize renderer
    tracing::info!("Initializing wgpu renderer...");
    let renderer = renderer::HorizonRenderer::new(width, height).await?;
    let renderer = Arc::new(renderer);

    tracing::info!("Renderer initialized, GPU ready");

    // Start server
    tracing::info!("Starting server on port {}...", port);
    tracing::info!("Open http://localhost:{}", port);
    server_standalone::run_server(renderer, width, height, fps, port).await?;

    Ok(())
}
