//! Streamio Session Manager
//!
//! Cross-platform service that orchestrates multi-session VDI:
//! - Virtual display lifecycle (IddCx on Windows, Xvfb on Linux, CoreGraphics on macOS)
//! - Per-user OS accounts
//! - Backend process launcher
//! - Input routing (named pipes on Windows, Unix sockets on Linux/macOS)
//! - REST API for gateway integration

mod accounts;
mod api;
mod display;
mod input_router;
mod launcher;
mod window_mgr;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

/// Platform-specific state directory for persisting session data.
fn state_dir() -> &'static str {
    #[cfg(windows)]
    { r"C:\ProgramData\Streamio" }
    #[cfg(target_os = "linux")]
    { "/var/lib/streamio" }
    #[cfg(target_os = "macos")]
    { "/var/lib/streamio" }
}

/// Platform-specific log directory.
fn log_dir() -> &'static str {
    #[cfg(windows)]
    { r"C:\build" }
    #[cfg(not(windows))]
    { "/var/log/streamio" }
}

/// Default backend executable path.
fn default_backend_path() -> &'static str {
    #[cfg(windows)]
    { r"C:\Program Files\Streamio\streamio.exe" }
    #[cfg(not(windows))]
    { "/usr/local/bin/streamio" }
}

/// Default display-ctl executable path.
fn default_display_ctl_path() -> &'static str {
    #[cfg(windows)]
    { r"C:\Program Files\Streamio\display-ctl.exe" }
    #[cfg(not(windows))]
    { "/usr/local/bin/display-ctl" }
}

/// A user's active VDI session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSession {
    pub session_id: String,
    pub user_id: String,
    pub os_user: String,
    pub display_index: u32,
    /// (x, y, width, height) in virtual desktop coordinates
    pub display_rect: (i32, i32, u32, u32),
    pub backend_port: u16,
    #[serde(skip)]
    pub backend_pid: Option<u32>,
    pub created_at: u64, // unix timestamp
}

/// Shared state across all modules.
pub struct SessionManagerState {
    pub sessions: RwLock<HashMap<String, UserSession>>,
    pub next_port: RwLock<u16>,
    /// Path to display-ctl executable (Windows only, unused on Linux/macOS)
    pub display_ctl_path: String,
    /// Path to backend executable
    pub backend_path: String,
    /// JWT secret shared with gateway and backends
    pub token_secret: String,
    /// Gateway URL for backend self-registration
    pub gateway_url: String,
}

impl SessionManagerState {
    fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            next_port: RwLock::new(9001),
            display_ctl_path: std::env::var("DISPLAY_CTL_PATH")
                .unwrap_or_else(|_| default_display_ctl_path().to_string()),
            backend_path: std::env::var("BACKEND_PATH")
                .unwrap_or_else(|_| default_backend_path().to_string()),
            token_secret: std::env::var("BACKEND_TOKEN_SECRET").unwrap_or_default(),
            gateway_url: std::env::var("GATEWAY_URL").unwrap_or_default(),
        }
    }

    /// Allocate the next available port for a backend instance.
    pub async fn allocate_port(&self) -> u16 {
        let mut port = self.next_port.write().await;
        let p = *port;
        *port += 1;
        p
    }

    /// Save session state to disk for crash recovery.
    pub async fn persist_state(&self) {
        let sessions = self.sessions.read().await;
        let dir = state_dir();
        let state_file = format!("{}/sessions.json", dir);
        let _ = std::fs::create_dir_all(dir);
        match serde_json::to_string_pretty(&*sessions) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&state_file, &json) {
                    error!("Failed to persist session state: {}", e);
                }
            }
            Err(e) => error!("Failed to serialize sessions: {}", e),
        }
    }

    /// Restore session state from disk. Does not restart backends — just loads metadata.
    pub async fn restore_state(&self) {
        let state_file = format!("{}/sessions.json", state_dir());
        match std::fs::read_to_string(&state_file) {
            Ok(json) => {
                match serde_json::from_str::<HashMap<String, UserSession>>(&json) {
                    Ok(restored) => {
                        info!("Restored {} sessions from disk", restored.len());
                        let mut sessions = self.sessions.write().await;
                        *sessions = restored;
                    }
                    Err(e) => warn!("Failed to parse session state: {}", e),
                }
            }
            Err(_) => info!("No previous session state found"),
        }
    }
}

/// Core async entrypoint shared across all platforms.
async fn run() {
    let state = Arc::new(SessionManagerState::new());

    // Restore previous session metadata (won't restart backends)
    state.restore_state().await;

    // Start input router
    let input_state = input_router::start(state.clone()).await;

    // Start window manager (confines windows to display regions on Windows, no-op elsewhere)
    window_mgr::start(state.clone());

    // Start REST API server
    let api_port: u16 = std::env::var("SESSION_MANAGER_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9100);

    info!("Session manager starting on port {}", api_port);

    let app = api::router(state.clone(), input_state);
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], api_port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind API port");
    info!("Session manager API listening on http://{}", addr);

    // Self-register with gateway (if GATEWAY_URL is set)
    if !state.gateway_url.is_empty() {
        let gw_url = state.gateway_url.clone();
        let port = api_port;
        tokio::spawn(async move {
            register_with_gateway(&gw_url, port).await;
        });
    }

    axum::serve(listener, app).await.expect("API server failed");
}

/// Register this session manager with the gateway as a host.
/// Retries every 10s until successful, then heartbeats every 30s.
async fn register_with_gateway(gateway_url: &str, port: u16) {
    let host_id = std::env::var("HOST_ID")
        .ok()
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .unwrap_or_else(uuid::Uuid::new_v4);

    let label = std::env::var("HOST_LABEL").ok();
    let max_sessions: u32 = std::env::var("MAX_SESSIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    let platform = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "macos"
    };

    // Determine our own URL that the gateway can reach
    let self_url = std::env::var("SESSION_MANAGER_URL").unwrap_or_else(|_| {
        // Try to guess from hostname
        let hostname = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "localhost".to_string());
        format!("http://{}:{}", hostname, port)
    });

    let register_url = format!("{}/internal/hosts/register", gateway_url);
    let heartbeat_url = format!("{}/internal/hosts/{}/heartbeat", gateway_url, host_id);

    let body = serde_json::json!({
        "id": host_id,
        "url": self_url,
        "label": label,
        "platform": platform,
        "max_sessions": max_sessions,
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    // Registration loop — retry until successful
    loop {
        match client.post(&register_url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                info!("Registered with gateway as host {} ({})", host_id, self_url);
                break;
            }
            Ok(resp) => {
                warn!(
                    "Gateway registration returned {}, retrying in 10s",
                    resp.status()
                );
            }
            Err(e) => {
                warn!("Failed to reach gateway at {}: {}, retrying in 10s", register_url, e);
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }

    // Heartbeat loop — every 30s
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        if let Err(e) = client.post(&heartbeat_url).send().await {
            warn!("Gateway heartbeat failed: {}", e);
        }
    }
}

fn main() {
    // ── Windows: service entry with mutex guard and file logging ──
    #[cfg(windows)]
    {
        std::panic::set_hook(Box::new(|info| {
            let msg = format!("SESSION MANAGER PANIC: {}", info);
            let _ = std::fs::write(
                format!("{}/session-manager-panic.log", log_dir()),
                &msg,
            );
        }));

        // Single-instance guard via Windows named mutex
        #[link(name = "kernel32")]
        extern "system" {
            fn CreateMutexA(
                sa: *mut std::ffi::c_void,
                own: i32,
                name: *const u8,
            ) -> *mut std::ffi::c_void;
            fn GetLastError() -> u32;
        }
        let mutex_name = b"Global\\StreamioSessionManager\0";
        let handle =
            unsafe { CreateMutexA(std::ptr::null_mut(), 1, mutex_name.as_ptr()) };
        if handle.is_null() || unsafe { GetLastError() } == 183 {
            eprintln!("Another instance is already running, exiting.");
            std::process::exit(0);
        }

        // File-based logging (stderr may be closed as a service)
        use tracing_subscriber::EnvFilter;
        let log_path = format!("{}/session-manager.log", log_dir());
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .expect("Failed to open log file");

        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .with_writer(std::sync::Mutex::new(log_file))
            .with_ansi(false)
            .init();

        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(run());
    }

    // ── Linux / macOS: stderr logging, file lock, simple runtime ──
    #[cfg(not(windows))]
    {
        use tracing_subscriber::EnvFilter;

        // Ensure state and log directories exist
        let _ = std::fs::create_dir_all(state_dir());
        let _ = std::fs::create_dir_all(log_dir());

        // Single-instance guard via file lock
        let lock_path = format!("{}/session-manager.lock", state_dir());
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&lock_path);

        if let Ok(ref file) = lock_file {
            use std::os::unix::io::AsRawFd;
            let fd = file.as_raw_fd();
            // Try non-blocking exclusive lock
            let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if ret != 0 {
                eprintln!("Another instance is already running, exiting.");
                std::process::exit(0);
            }
            // Write our PID
            use std::io::Write;
            let _ = (&*file).write_all(format!("{}", std::process::id()).as_bytes());
        }

        // Log to stderr (or file if STREAMIO_LOG_FILE is set)
        if let Ok(log_path) = std::env::var("STREAMIO_LOG_FILE") {
            let log_file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .expect("Failed to open log file");
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .with_writer(std::sync::Mutex::new(log_file))
                .with_ansi(false)
                .init();
        } else {
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .init();
        }

        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(run());
    }
}
