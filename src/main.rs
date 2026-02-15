//! Streamio - VDI-style screen capture and WebRTC streaming
//!
//! Captures the screen/display and streams via WebRTC to browsers.

mod input;
mod screen_capture;
mod screen_server;

use anyhow::Result;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

const DEFAULT_FPS: u32 = 30;
const DEFAULT_PORT: u16 = 8123;

/// If bundled GStreamer libs exist next to the executable, configure env vars
/// to use them instead of system-installed GStreamer. Falls through to system
/// GStreamer when no bundled libs are found (normal dev mode).
fn setup_bundled_gstreamer() {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let exe_dir = match exe.parent() {
        Some(d) => d,
        None => return,
    };

    let lib_dir = exe_dir.join("lib");
    let plugin_dir = lib_dir.join("gstreamer-1.0");

    if !plugin_dir.exists() {
        return;
    }

    eprintln!("Using bundled GStreamer from {}", lib_dir.display());

    std::env::set_var("GST_PLUGIN_PATH", &plugin_dir);
    std::env::set_var("GST_PLUGIN_SYSTEM_PATH", "");
    std::env::set_var("GST_REGISTRY", exe_dir.join("gst-registry.bin"));
    std::env::set_var("GST_REGISTRY_REUSE_PLUGIN_SCANNER", "no");

    let scanner_name = if cfg!(windows) { "gst-plugin-scanner.exe" } else { "gst-plugin-scanner" };
    let scanner = exe_dir.join("libexec").join(scanner_name);
    if scanner.exists() {
        std::env::set_var("GST_PLUGIN_SCANNER", &scanner);
    }

    // Platform-specific shared library search path
    if cfg!(target_os = "macos") {
        std::env::set_var("DYLD_LIBRARY_PATH", &lib_dir);
    } else if cfg!(target_os = "linux") {
        std::env::set_var("LD_LIBRARY_PATH", &lib_dir);
    }
    // Windows searches the exe directory automatically
}

#[tokio::main]
async fn main() -> Result<()> {
    // Set up bundled GStreamer if present (must be before gstreamer::init)
    setup_bundled_gstreamer();

    // Initialize logging
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Initialize GStreamer
    gstreamer::init()?;

    // Check that critical plugins are available
    let registry = gstreamer::Registry::get();
    for plugin in ["webrtc", "nice", "dtls", "srtp", "rtp", "videoconvertscale"] {
        if registry.find_plugin(plugin).is_none() {
            tracing::warn!("GStreamer plugin '{}' not found — WebRTC may not work", plugin);
        }
    }

    // Read config from environment
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let fps: u32 = std::env::var("FPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_FPS);

    tracing::info!("Streamio v{}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Capturing screen at {} fps", fps);
    tracing::info!("Open http://localhost:{} to view", port);

    // Start server
    screen_server::run_server(fps, port).await?;

    Ok(())
}
