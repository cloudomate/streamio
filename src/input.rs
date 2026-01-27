//! Input injection for remote control using enigo
//!
//! Cross-platform mouse and keyboard input simulation.

use enigo::{Enigo, Keyboard, Mouse, Settings, Coordinate, Button, Direction};
use serde::Deserialize;
use std::sync::Mutex;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputEvent {
    MouseDown { button: u8, x: i32, y: i32 },
    MouseUp { button: u8, x: i32, y: i32 },
    MouseMove { x: i32, y: i32 },
    Scroll { dx: f64, dy: f64 },
    KeyDown { key: String, code: String, modifiers: Modifiers },
    KeyUp { key: String, code: String, modifiers: Modifiers },
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Modifiers {
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub meta: bool,
}

/// Input controller using enigo
pub struct InputController {
    enigo: Mutex<Enigo>,
}

impl InputController {
    pub fn new() -> Self {
        let enigo = Enigo::new(&Settings::default()).expect("Failed to create Enigo");
        Self {
            enigo: Mutex::new(enigo),
        }
    }

    pub fn handle_event(&self, event: &InputEvent) {
        let mut enigo = self.enigo.lock().unwrap();

        match event {
            InputEvent::MouseMove { x, y } => {
                let _ = enigo.move_mouse(*x, *y, Coordinate::Abs);
            }
            InputEvent::MouseDown { button, x, y } => {
                tracing::info!("Mouse down at ({}, {})", x, y);
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
                tracing::info!("Mouse up at ({}, {})", x, y);
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
                    let _ = enigo.scroll(amount, enigo::Axis::Vertical);
                }
            }
            InputEvent::KeyDown { key, code: _, modifiers } => {
                tracing::info!("Key down: {}", key);

                // For single printable characters without modifiers, use text()
                if key.len() == 1 && !modifiers.ctrl && !modifiers.alt && !modifiers.meta {
                    let _ = enigo.text(key);
                } else if let Some(k) = map_key(key) {
                    // Handle modifier keys
                    if modifiers.meta {
                        let _ = enigo.key(enigo::Key::Meta, Direction::Press);
                    }
                    if modifiers.ctrl {
                        let _ = enigo.key(enigo::Key::Control, Direction::Press);
                    }
                    if modifiers.alt {
                        let _ = enigo.key(enigo::Key::Alt, Direction::Press);
                    }
                    if modifiers.shift {
                        let _ = enigo.key(enigo::Key::Shift, Direction::Press);
                    }

                    let _ = enigo.key(k, Direction::Click);

                    // Release modifiers
                    if modifiers.shift {
                        let _ = enigo.key(enigo::Key::Shift, Direction::Release);
                    }
                    if modifiers.alt {
                        let _ = enigo.key(enigo::Key::Alt, Direction::Release);
                    }
                    if modifiers.ctrl {
                        let _ = enigo.key(enigo::Key::Control, Direction::Release);
                    }
                    if modifiers.meta {
                        let _ = enigo.key(enigo::Key::Meta, Direction::Release);
                    }
                }
            }
            InputEvent::KeyUp { key: _, code: _, modifiers: _ } => {
                // Key up is handled in KeyDown with Click
            }
        }
    }
}

fn map_key(key: &str) -> Option<enigo::Key> {
    match key {
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
        // Single character keys
        s if s.len() == 1 => {
            let c = s.chars().next().unwrap();
            Some(enigo::Key::Unicode(c))
        }
        _ => None,
    }
}
