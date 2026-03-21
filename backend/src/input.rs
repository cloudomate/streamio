//! Input forwarding for remote control.
//!
//! On Windows with a session manager, input events are forwarded to
//! a per-session named pipe: `\\.\pipe\streamio-input-<session_id>`.
//! The session manager handles coordinate translation and VHID injection.
//!
//! Falls back to enigo (direct injection) on non-Windows or when no
//! session pipe is available (standalone/dev mode).

use std::sync::Mutex;
use streamio_types::InputEvent;

/// Input controller — routes events to the appropriate injection path.
pub struct InputController {
    /// Per-session pipe (Windows with session manager)
    #[cfg(target_os = "windows")]
    pipe: Mutex<PipeState>,
    /// Fallback direct injection (non-Windows or dev mode)
    #[cfg(not(target_os = "windows"))]
    enigo: Mutex<enigo::Enigo>,
}

#[cfg(target_os = "windows")]
enum PipeState {
    /// Connected to session manager pipe
    Connected(std::fs::File),
    /// Using legacy shared pipe (streamio-service)
    Legacy(std::fs::File),
    /// Not connected yet
    Disconnected {
        session_id: Option<String>,
    },
}

impl InputController {
    pub fn new() -> Self {
        #[cfg(target_os = "windows")]
        {
            Self::set_dpi_aware();

            let session_id = std::env::var("SESSION_ID").ok();

            // If no SESSION_ID, try to spawn the legacy service helper
            if session_id.is_none() {
                Self::spawn_input_helper();
            }

            Self {
                pipe: Mutex::new(PipeState::Disconnected { session_id }),
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            let enigo = enigo::Enigo::new(&enigo::Settings::default())
                .expect("Failed to create Enigo");
            Self {
                enigo: Mutex::new(enigo),
            }
        }
    }

    /// Spawn the legacy input helper (when not using session manager).
    #[cfg(target_os = "windows")]
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

    #[cfg(target_os = "windows")]
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
        #[cfg(target_os = "windows")]
        {
            self.send_to_pipe(event);
            return;
        }

        #[cfg(not(target_os = "windows"))]
        {
            self.handle_event_enigo(event);
        }
    }

    /// Send event to the appropriate named pipe (session or legacy).
    #[cfg(target_os = "windows")]
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

        // Try to write on existing connection
        match &mut *guard {
            PipeState::Connected(ref mut pipe) | PipeState::Legacy(ref mut pipe) => {
                let len = (json.len() as u32).to_le_bytes();
                if pipe.write_all(&len).is_ok()
                    && pipe.write_all(&json).is_ok()
                    && pipe.flush().is_ok()
                {
                    return;
                }
                // Connection broken — fall through to reconnect
                tracing::warn!("Input pipe broken, reconnecting");
            }
            PipeState::Disconnected { .. } => {}
        }

        // Extract session_id before replacing state
        let session_id = match &*guard {
            PipeState::Disconnected { session_id } => session_id.clone(),
            _ => std::env::var("SESSION_ID").ok(),
        };

        // Try to connect
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
            Err(_e) => {
                // Silently retry next event — avoid log spam
            }
        }

        *guard = PipeState::Disconnected { session_id };
    }

    #[cfg(not(target_os = "windows"))]
    fn handle_event_enigo(&self, event: &InputEvent) {
        use enigo::{Button, Coordinate, Direction, Keyboard, Mouse};

        match event {
            InputEvent::MouseMove { x, y } => {
                let mut enigo = self.enigo.lock().unwrap();
                let _ = enigo.move_mouse(*x, *y, Coordinate::Abs);
            }
            InputEvent::MouseDown { button, x, y } => {
                let mut enigo = self.enigo.lock().unwrap();
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
                let mut enigo = self.enigo.lock().unwrap();
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
                    let mut enigo = self.enigo.lock().unwrap();
                    let _ = enigo.scroll(amount, enigo::Axis::Vertical);
                }
            }
            InputEvent::KeyDown {
                key,
                code: _,
                modifiers: _,
            } => {
                if let Some(k) = map_key(key) {
                    let mut enigo = self.enigo.lock().unwrap();
                    let _ = enigo.key(k, Direction::Press);
                }
            }
            InputEvent::KeyUp {
                key,
                code: _,
                modifiers: _,
            } => {
                if let Some(k) = map_key(key) {
                    let mut enigo = self.enigo.lock().unwrap();
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
