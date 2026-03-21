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
    // Early diagnostic: dump environment to a file so we can debug headless launches
    {
        let pid = std::process::id();
        let diag_path = format!(r"C:\build\backend-diag-{}.txt", pid);
        let mut diag = String::new();
        diag.push_str(&format!("PID: {}\n", pid));
        diag.push_str(&format!("EXE: {:?}\n", std::env::current_exe()));
        diag.push_str(&format!("CWD: {:?}\n", std::env::current_dir()));
        for (k, v) in std::env::vars() {
            diag.push_str(&format!("{}={}\n", k, v));
        }
        // Try multiple locations
        if std::fs::write(&diag_path, &diag).is_err() {
            let alt = format!(r"C:\Users\Public\backend-diag-{}.txt", pid);
            if std::fs::write(&alt, &diag).is_err() {
                let temp = format!(r"C:\Windows\Temp\backend-diag-{}.txt", pid);
                let _ = std::fs::write(&temp, &diag);
            }
        }
    }

    // Write panics to a file when running headless (launched by session manager)
    if let Ok(log_path) = std::env::var("STREAMIO_LOG_FILE") {
        let panic_path = log_path.clone();
        std::panic::set_hook(Box::new(move |info| {
            let msg = format!("BACKEND PANIC: {}\n", info);
            let _ = std::fs::write(&panic_path, &msg);
        }));
    }

    // Set up bundled GStreamer if present (must be before gstreamer::init)
    setup_bundled_gstreamer();

    // Initialize logging — write to file if STREAMIO_LOG_FILE is set (e.g. session manager launch)
    let env_filter = tracing_subscriber::EnvFilter::new(
        std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
    );
    if let Ok(log_path) = std::env::var("STREAMIO_LOG_FILE") {
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .expect("Failed to open log file");
        tracing_subscriber::registry()
            .with(env_filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::sync::Mutex::new(log_file))
                    .with_ansi(false),
            )
            .init();
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer())
            .init();
    }

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
    let token_secret = std::env::var("BACKEND_TOKEN_SECRET").unwrap_or_default();
    let enable_audio = std::env::var("ENABLE_AUDIO")
        .map(|s| s == "0" || s.eq_ignore_ascii_case("false"))
        .map(|disabled| !disabled)
        .unwrap_or(true); // Audio ON by default
    screen_server::run(fps, port, token_secret, enable_audio).await?;

    Ok(())
}
