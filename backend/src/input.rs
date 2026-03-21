//! Input forwarding for remote control.
//!
//! On Windows with a session manager, input events are forwarded to
//! a per-session named pipe: `\\.\pipe\streamio-input-<session_id>`.
//!
//! On Linux/macOS with a session manager, input events are forwarded to
//! a per-session Unix domain socket: `/tmp/streamio-input-<session_id>.sock`.
//!
//! The session manager handles coordinate translation and input injection.
//!
//! Falls back to enigo (direct injection) when no session pipe/socket is
//! available (standalone/dev mode).

use std::sync::Mutex;
use streamio_types::InputEvent;

// ═══════════════════════════════════════════════════════════════════════════════
// Windows implementation — named pipes to session manager
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(target_os = "windows")]
pub struct InputController {
    pipe: Mutex<PipeState>,
}

#[cfg(target_os = "windows")]
enum PipeState {
    Connected(std::fs::File),
    Legacy(std::fs::File),
    Disconnected { session_id: Option<String> },
}

#[cfg(target_os = "windows")]
impl InputController {
    pub fn new() -> Self {
        Self::set_dpi_aware();

        let session_id = std::env::var("SESSION_ID").ok();

        if session_id.is_none() {
            Self::spawn_input_helper();
        }

        Self {
            pipe: Mutex::new(PipeState::Disconnected { session_id }),
        }
    }

    fn spawn_input_helper() {
        use std::os::windows::process::CommandExt;
        use std::process::Command;

        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let candidates = [
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("streamio-service.exe"))),
            Some(std::path::PathBuf::from(
                r"C:\Program Files\Streamio\streamio-service.exe",
            )),
        ];

        for candidate in candidates.iter().flatten() {
            if candidate.exists() {
                match Command::new(candidate)
                    .creation_flags(CREATE_NO_WINDOW)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(child) => {
                        tracing::info!(
                            "Spawned legacy input helper: {} (pid={})",
                            candidate.display(),
                            child.id()
                        );
                        return;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to spawn input helper {}: {}",
                            candidate.display(),
                            e
                        );
                    }
                }
            }
        }
        tracing::info!("No input helper found — lock screen input unavailable in dev mode");
    }

    fn set_dpi_aware() {
        use std::ffi::c_void;
        type DPI_AWARENESS_CONTEXT = *mut c_void;
        const DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2: DPI_AWARENESS_CONTEXT =
            -4isize as DPI_AWARENESS_CONTEXT;

        #[link(name = "user32")]
        extern "system" {
            fn SetProcessDpiAwarenessContext(value: DPI_AWARENESS_CONTEXT) -> i32;
        }

        unsafe {
            let result =
                SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            if result != 0 {
                tracing::info!("Set DPI awareness to per-monitor-v2");
            } else {
                tracing::warn!("Failed to set DPI awareness (may already be set)");
            }
        }
    }

    pub fn handle_event(&self, event: &InputEvent) {
        self.send_to_pipe(event);
    }

    fn send_to_pipe(&self, event: &InputEvent) {
        use std::io::Write;

        let json = match serde_json::to_vec(event) {
            Ok(j) => j,
            Err(_) => return,
        };

        let mut guard = match self.pipe.lock() {
            Ok(g) => g,
            Err(_) => return,
        };

        match &mut *guard {
            PipeState::Connected(ref mut pipe) | PipeState::Legacy(ref mut pipe) => {
                let len = (json.len() as u32).to_le_bytes();
                if pipe.write_all(&len).is_ok()
                    && pipe.write_all(&json).is_ok()
                    && pipe.flush().is_ok()
                {
                    return;
                }
                tracing::warn!("Input pipe broken, reconnecting");
            }
            PipeState::Disconnected { .. } => {}
        }

        let session_id = match &*guard {
            PipeState::Disconnected { session_id } => session_id.clone(),
            _ => std::env::var("SESSION_ID").ok(),
        };

        let pipe_name = if let Some(ref sid) = session_id {
            format!(r"\\.\pipe\streamio-input-{}", sid)
        } else {
            r"\\.\pipe\streamio-input".to_string()
        };

        match std::fs::OpenOptions::new().write(true).open(&pipe_name) {
            Ok(mut pipe) => {
                let len = (json.len() as u32).to_le_bytes();
                if pipe.write_all(&len).is_ok()
                    && pipe.write_all(&json).is_ok()
                    && pipe.flush().is_ok()
                {
                    tracing::info!("Connected to input pipe: {}", pipe_name);
                    if session_id.is_some() {
                        *guard = PipeState::Connected(pipe);
                    } else {
                        *guard = PipeState::Legacy(pipe);
                    }
                    return;
                }
            }
            Err(_e) => {}
        }

        *guard = PipeState::Disconnected { session_id };
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Unix implementation — Unix domain sockets or enigo fallback
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(not(target_os = "windows"))]
pub struct InputController {
    /// Unix domain socket to session manager (when SESSION_ID is set)
    socket: Mutex<SocketState>,
    /// Fallback direct injection (dev mode, no session manager)
    enigo: Mutex<Option<enigo::Enigo>>,
}

#[cfg(not(target_os = "windows"))]
enum SocketState {
    Connected(std::os::unix::net::UnixStream),
    Disconnected { session_id: Option<String> },
}

#[cfg(not(target_os = "windows"))]
impl InputController {
    pub fn new() -> Self {
        let session_id = std::env::var("SESSION_ID").ok();

        // Try to connect to session manager socket immediately
        let (socket_state, need_enigo) = if let Some(ref sid) = session_id {
            let sock_path = format!("/tmp/streamio-input-{}.sock", sid);
            match std::os::unix::net::UnixStream::connect(&sock_path) {
                Ok(stream) => {
                    tracing::info!("Connected to session manager socket: {}", sock_path);
                    (SocketState::Connected(stream), false)
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to connect to session socket {} (will retry): {}",
                        sock_path,
                        e
                    );
                    (
                        SocketState::Disconnected {
                            session_id: session_id.clone(),
                        },
                        false, // Don't create enigo yet — will retry socket first
                    )
                }
            }
        } else {
            // No session manager — use enigo for direct injection (dev mode)
            (SocketState::Disconnected { session_id: None }, true)
        };

        let enigo = if need_enigo {
            match enigo::Enigo::new(&enigo::Settings::default()) {
                Ok(e) => Some(e),
                Err(err) => {
                    tracing::warn!("Failed to create Enigo: {}", err);
                    None
                }
            }
        } else {
            None
        };

        Self {
            socket: Mutex::new(socket_state),
            enigo: Mutex::new(enigo),
        }
    }

    pub fn handle_event(&self, event: &InputEvent) {
        // Try Unix socket first (session manager mode)
        if self.send_to_socket(event) {
            return;
        }

        // Fallback to enigo (dev mode)
        self.handle_event_enigo(event);
    }

    /// Send event via Unix domain socket. Returns true if sent successfully.
    fn send_to_socket(&self, event: &InputEvent) -> bool {
        use std::io::Write;

        let json = match serde_json::to_vec(event) {
            Ok(j) => j,
            Err(_) => return false,
        };

        let mut guard = match self.socket.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };

        // Try writing on existing connection
        if let SocketState::Connected(ref mut stream) = *guard {
            let len = (json.len() as u32).to_le_bytes();
            if stream.write_all(&len).is_ok()
                && stream.write_all(&json).is_ok()
                && stream.flush().is_ok()
            {
                return true;
            }
            tracing::warn!("Session socket broken, reconnecting");
        }

        // Extract session_id for reconnect
        let session_id = match &*guard {
            SocketState::Disconnected { session_id } => session_id.clone(),
            _ => std::env::var("SESSION_ID").ok(),
        };

        // No session ID means no session manager to connect to
        let sid = match &session_id {
            Some(s) => s,
            None => return false,
        };

        // Try reconnecting
        let sock_path = format!("/tmp/streamio-input-{}.sock", sid);
        match std::os::unix::net::UnixStream::connect(&sock_path) {
            Ok(mut stream) => {
                let len = (json.len() as u32).to_le_bytes();
                if stream.write_all(&len).is_ok()
                    && stream.write_all(&json).is_ok()
                    && stream.flush().is_ok()
                {
                    tracing::info!("Reconnected to session socket: {}", sock_path);
                    *guard = SocketState::Connected(stream);
                    return true;
                }
            }
            Err(_) => {}
        }

        *guard = SocketState::Disconnected { session_id };
        false
    }

    fn handle_event_enigo(&self, event: &InputEvent) {
        use enigo::{Button, Coordinate, Direction, Keyboard, Mouse};

        let mut guard = match self.enigo.lock() {
            Ok(g) => g,
            Err(_) => return,
        };

        let enigo = match guard.as_mut() {
            Some(e) => e,
            None => return,
        };

        match event {
            InputEvent::MouseMove { x, y } => {
                let _ = enigo.move_mouse(*x, *y, Coordinate::Abs);
            }
            InputEvent::MouseDown { button, x, y } => {
                let _ = enigo.move_mouse(*x, *y, Coordinate::Abs);
                let btn = match button {
                    0 => Button::Left,
                    1 => Button::Middle,
                    2 => Button::Right,
                    _ => Button::Left,
                };
                let _ = enigo.button(btn, Direction::Press);
            }
            InputEvent::MouseUp { button, x, y } => {
                let _ = enigo.move_mouse(*x, *y, Coordinate::Abs);
                let btn = match button {
                    0 => Button::Left,
                    1 => Button::Middle,
                    2 => Button::Right,
                    _ => Button::Left,
                };
                let _ = enigo.button(btn, Direction::Release);
            }
            InputEvent::Scroll { dx: _, dy } => {
                let amount = (-*dy / 10.0) as i32;
                if amount != 0 {
                    let _ = enigo.scroll(amount, enigo::Axis::Vertical);
                }
            }
            InputEvent::KeyDown { key, .. } => {
                if let Some(k) = map_key(key) {
                    let _ = enigo.key(k, Direction::Press);
                }
            }
            InputEvent::KeyUp { key, .. } => {
                if let Some(k) = map_key(key) {
                    let _ = enigo.key(k, Direction::Release);
                }
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn map_key(key: &str) -> Option<enigo::Key> {
    match key {
        "Shift" => Some(enigo::Key::Shift),
        "Control" => Some(enigo::Key::Control),
        "Alt" => Some(enigo::Key::Alt),
        "Meta" => Some(enigo::Key::Meta),
        "Enter" => Some(enigo::Key::Return),
        "Escape" => Some(enigo::Key::Escape),
        "Backspace" => Some(enigo::Key::Backspace),
        "Tab" => Some(enigo::Key::Tab),
        " " => Some(enigo::Key::Space),
        "ArrowUp" => Some(enigo::Key::UpArrow),
        "ArrowDown" => Some(enigo::Key::DownArrow),
        "ArrowLeft" => Some(enigo::Key::LeftArrow),
        "ArrowRight" => Some(enigo::Key::RightArrow),
        "Delete" => Some(enigo::Key::Delete),
        "Insert" => Some(enigo::Key::Other(0x2D)),
        "Home" => Some(enigo::Key::Home),
        "End" => Some(enigo::Key::End),
        "PageUp" => Some(enigo::Key::PageUp),
        "PageDown" => Some(enigo::Key::PageDown),
        "F1" => Some(enigo::Key::F1),
        "F2" => Some(enigo::Key::F2),
        "F3" => Some(enigo::Key::F3),
        "F4" => Some(enigo::Key::F4),
        "F5" => Some(enigo::Key::F5),
        "F6" => Some(enigo::Key::F6),
        "F7" => Some(enigo::Key::F7),
        "F8" => Some(enigo::Key::F8),
        "F9" => Some(enigo::Key::F9),
        "F10" => Some(enigo::Key::F10),
        "F11" => Some(enigo::Key::F11),
        "F12" => Some(enigo::Key::F12),
        "CapsLock" => Some(enigo::Key::CapsLock),
        "NumLock" => Some(enigo::Key::Other(0x90)),
        "ScrollLock" => Some(enigo::Key::Other(0x91)),
        s if s.len() == 1 => {
            let c = s.chars().next().unwrap();
            Some(enigo::Key::Unicode(c))
        }
        _ => None,
    }
}
