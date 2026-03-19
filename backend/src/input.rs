//! Input injection for remote control using enigo
//!
//! Cross-platform mouse and keyboard input simulation.

use enigo::{Button, Coordinate, Direction, Enigo, Keyboard, Mouse, Settings};
use std::sync::Mutex;
use streamio_types::InputEvent;

/// Input controller using enigo
pub struct InputController {
    enigo: Mutex<Enigo>,
}

impl InputController {
    pub fn new() -> Self {
        // On Windows, ensure we are per-monitor DPI aware so that
        // SetCursorPos coordinates match physical pixels (same space
        // as the screen capture).
        #[cfg(target_os = "windows")]
        Self::set_dpi_aware();

        let enigo = Enigo::new(&Settings::default()).expect("Failed to create Enigo");

        Self {
            enigo: Mutex::new(enigo),
        }
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
                // Scroll amount (negative = scroll down, positive = scroll up)
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
        // Modifier keys
        "Shift" => Some(enigo::Key::Shift),
        "Control" => Some(enigo::Key::Control),
        "Alt" => Some(enigo::Key::Alt),
        "Meta" => Some(enigo::Key::Meta),
        // Navigation / editing
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
        // Function keys
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
        // Lock keys
        "CapsLock" => Some(enigo::Key::CapsLock),
        "NumLock" => Some(enigo::Key::Other(0x90)),
        "ScrollLock" => Some(enigo::Key::Other(0x91)),
        // Single character keys
        s if s.len() == 1 => {
            let c = s.chars().next().unwrap();
            Some(enigo::Key::Unicode(c))
        }
        _ => None,
    }
}
