//! Input injection for remote control using enigo
//!
//! Cross-platform mouse and keyboard input simulation.
//! On Windows, when a lock-screen service is available, input commands
//! are forwarded to it via named pipe so they reach the Winlogon desktop.

use enigo::{Button, Coordinate, Direction, Enigo, Keyboard, Mouse, Settings};
use std::sync::Mutex;
use streamio_types::InputEvent;

/// Input controller using enigo
pub struct InputController {
    enigo: Mutex<Enigo>,
}

impl InputController {
    pub fn new() -> Self {
        #[cfg(target_os = "windows")]
        Self::set_dpi_aware();

        #[cfg(target_os = "windows")]
        Self::spawn_input_helper();

        let enigo = Enigo::new(&Settings::default()).expect("Failed to create Enigo");

        Self {
            enigo: Mutex::new(enigo),
        }
    }

    /// Spawn the uiAccess input helper if it exists next to the exe or in Program Files.
    #[cfg(target_os = "windows")]
    fn spawn_input_helper() {
        use std::os::windows::process::CommandExt;
        use std::process::Command;

        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let candidates = [
            // Next to this exe
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("streamio-service.exe"))),
            // Program Files install location
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
                            "Spawned input helper: {} (pid={})",
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
        tracing::info!("Input helper not found — lock screen input will not be available");
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
            let result = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            if result != 0 {
                tracing::info!("Set DPI awareness to per-monitor-v2");
            } else {
                tracing::warn!("Failed to set DPI awareness (may already be set)");
            }
        }
    }

    pub fn handle_event(&self, event: &InputEvent) {
        // On Windows, use VHF driver exclusively (works on lock screen too).
        // Enigo causes duplicate input since both paths inject simultaneously.
        #[cfg(target_os = "windows")]
        {
            let _ = lock_service::try_send(event);
            return;
        }

        #[cfg(not(target_os = "windows"))]
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
            InputEvent::KeyDown { key, code: _, modifiers: _ } => {
                if let Some(k) = map_key(key) {
                    let mut enigo = self.enigo.lock().unwrap();
                    let _ = enigo.key(k, Direction::Press);
                }
            }
            InputEvent::KeyUp { key, code: _, modifiers: _ } => {
                if let Some(k) = map_key(key) {
                    let mut enigo = self.enigo.lock().unwrap();
                    let _ = enigo.key(k, Direction::Release);
                }
            }
        }
    }
}

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

/// Named-pipe client for the lock-screen input service.
/// The service runs as SYSTEM and can inject input on the Winlogon desktop.
/// Pipe name: \\.\pipe\streamio-input
#[cfg(target_os = "windows")]
mod lock_service {
    use streamio_types::InputEvent;
    use std::io::Write;
    use std::sync::Mutex;

    static PIPE: Mutex<Option<std::fs::File>> = Mutex::new(None);
    static BACKOFF_UNTIL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    /// Try to send an input event to the lock-screen service.
    /// Returns true if sent successfully, false if pipe not available.
    pub fn try_send(event: &InputEvent) -> bool {
        use std::sync::atomic::Ordering;

        // Backoff check (avoid reconnect spam)
        if now_secs() < BACKOFF_UNTIL.load(Ordering::Relaxed) {
            return false;
        }

        let json = match serde_json::to_vec(event) {
            Ok(j) => j,
            Err(_) => return false,
        };

        let mut guard = match PIPE.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };

        // Try to write on existing connection
        if let Some(ref mut pipe) = *guard {
            let len = (json.len() as u32).to_le_bytes();
            if pipe.write_all(&len).is_ok()
                && pipe.write_all(&json).is_ok()
                && pipe.flush().is_ok()
            {
                return true;
            }
            // Connection broken, drop it
            tracing::warn!("Pipe connection broken, reconnecting");
            *guard = None;
        }

        // Try to open a new connection
        match std::fs::OpenOptions::new()
            .write(true)
            .open(r"\\.\pipe\streamio-input")
        {
            Ok(mut pipe) => {
                let len = (json.len() as u32).to_le_bytes();
                if pipe.write_all(&len).is_ok()
                    && pipe.write_all(&json).is_ok()
                    && pipe.flush().is_ok()
                {
                    static CONNECT_LOG: std::sync::atomic::AtomicBool =
                        std::sync::atomic::AtomicBool::new(false);
                    if !CONNECT_LOG.swap(true, Ordering::Relaxed) {
                        tracing::info!("Connected to input service pipe");
                    }
                    *guard = Some(pipe);
                    return true;
                }
                tracing::warn!("Pipe write failed after connect");
                false
            }
            Err(e) => {
                static ERR_LOG: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                if !ERR_LOG.swap(true, Ordering::Relaxed) {
                    tracing::warn!("Input service pipe not available: {}", e);
                }
                // Back off for 5 seconds
                BACKOFF_UNTIL.store(now_secs() + 5, Ordering::Relaxed);
                false
            }
        }
    }
}
