//! Streamio Session Manager
//!
//! Windows service that orchestrates multi-session VDI:
//! - Virtual display lifecycle (IddCx via display-ctl)
//! - Per-user local Windows accounts
//! - Backend process launcher (CreateProcessAsUser)
//! - Input routing (per-session named pipes → VHID)
//! - REST API for gateway integration
//!
//! Replaces streamio-service.exe as the single input injection point.

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

/// A user's active VDI session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSession {
    pub session_id: String,
    pub user_id: String,
    pub windows_user: String,
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
    /// Path to display-ctl executable
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
                .unwrap_or_else(|_| r"C:\Program Files\Streamio\display-ctl.exe".to_string()),
            backend_path: std::env::var("BACKEND_PATH")
                .unwrap_or_else(|_| r"C:\Program Files\Streamio\streamio.exe".to_string()),
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
        let state_file = r"C:\ProgramData\Streamio\sessions.json";
        let _ = std::fs::create_dir_all(r"C:\ProgramData\Streamio");
        match serde_json::to_string_pretty(&*sessions) {
            Ok(json) => {
                if let Err(e) = std::fs::write(state_file, &json) {
                    error!("Failed to persist session state: {}", e);
                }
            }
            Err(e) => error!("Failed to serialize sessions: {}", e),
        }
    }

    /// Restore session state from disk. Does not restart backends — just loads metadata.
    pub async fn restore_state(&self) {
        let state_file = r"C:\ProgramData\Streamio\sessions.json";
        match std::fs::read_to_string(state_file) {
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

fn main() {
    // On Windows, run as service or standalone
    #[cfg(windows)]
    {
        // Panic handler — write to file
        std::panic::set_hook(Box::new(|info| {
            let msg = format!("SESSION MANAGER PANIC: {}", info);
            let _ = std::fs::write(r"C:\build\session-manager-panic.log", &msg);
        }));

        // Single-instance guard
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
            let _ = std::fs::write(r"C:\build\session-manager.log", "Another instance running, exiting.\n");
            std::process::exit(0);
        }

        run_session_manager();
    }

    #[cfg(not(windows))]
    {
        eprintln!("Session manager only runs on Windows.");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn run_session_manager() {
    use tracing_subscriber::EnvFilter;

    // Write logs to file — stderr may be closed when running as a detached process
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(r"C:\build\session-manager.log")
        .expect("Failed to open log file");

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::sync::Mutex::new(log_file))
        .with_ansi(false)
        .init();

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let state = Arc::new(SessionManagerState::new());

        // Restore previous session metadata (won't restart backends)
        state.restore_state().await;

        // Start input router (opens VHID device, accepts per-session pipes)
        let input_state = input_router::start(state.clone()).await;

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

        axum::serve(listener, app).await.expect("API server failed");
    });
}
