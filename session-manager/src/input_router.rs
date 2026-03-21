//! Per-session input routing via named pipes → VHID driver.
//!
//! Each backend connects to its own named pipe: `\\.\pipe\streamio-input-<session_id>`
//! The input router translates coordinates to the session's display region
//! and injects via the VHID kernel driver. Each session has isolated button/key state.

use crate::SessionManagerState;
use std::ffi::c_void;
use std::sync::Arc;
use tracing::{error, info, warn};

type HANDLE = *mut c_void;
const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;

// ── IOCTL codes (must match driver/vhid/streamio-vhid.h) ──────────────

const FILE_DEVICE_VHID: u32 = 0x8000;
const METHOD_BUFFERED: u32 = 0;
const FILE_ANY_ACCESS: u32 = 0;

const fn ctl_code(device: u32, function: u32, method: u32, access: u32) -> u32 {
    (device << 16) | (access << 14) | (function << 2) | method
}

const IOCTL_VHID_SUBMIT_KEYBOARD: u32 =
    ctl_code(FILE_DEVICE_VHID, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS);
const IOCTL_VHID_SUBMIT_MOUSE: u32 =
    ctl_code(FILE_DEVICE_VHID, 0x801, METHOD_BUFFERED, FILE_ANY_ACCESS);

// ── HID report structs ─────────────────────────────────────────────────

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct VhidKeyboardReport {
    report_id: u8,
    modifiers: u8,
    reserved: u8,
    keys: [u8; 6],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct VhidMouseReport {
    report_id: u8,
    buttons: u8,
    x: i16,
    y: i16,
    wheel: i8,
}

// ── Win32 FFI ──────────────────────────────────────────────────────────

#[repr(C)]
struct SECURITY_ATTRIBUTES {
    length: u32,
    security_descriptor: *mut c_void,
    inherit_handle: i32,
}

#[repr(C)]
struct SECURITY_DESCRIPTOR {
    revision: u8,
    sbz1: u8,
    control: u16,
    owner: *mut c_void,
    group: *mut c_void,
    sacl: *mut c_void,
    dacl: *mut c_void,
}

#[repr(C)]
struct SP_DEVICE_INTERFACE_DATA {
    cb_size: u32,
    interface_class_guid: [u8; 16],
    flags: u32,
    reserved: usize,
}

#[repr(C)]
struct SP_DEVICE_INTERFACE_DETAIL_DATA_W {
    cb_size: u32,
    device_path: [u16; 1],
}

type HDEVINFO = *mut c_void;
const DIGCF_PRESENT: u32 = 0x02;
const DIGCF_DEVICEINTERFACE: u32 = 0x10;
const GENERIC_READ: u32 = 0x80000000;
const GENERIC_WRITE: u32 = 0x40000000;
const OPEN_EXISTING: u32 = 3;

#[link(name = "advapi32")]
extern "system" {
    fn InitializeSecurityDescriptor(sd: *mut SECURITY_DESCRIPTOR, revision: u32) -> i32;
    fn SetSecurityDescriptorDacl(
        sd: *mut SECURITY_DESCRIPTOR,
        present: i32,
        dacl: *mut c_void,
        defaulted: i32,
    ) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateNamedPipeA(
        name: *const u8,
        open_mode: u32,
        pipe_mode: u32,
        max_instances: u32,
        out_buf: u32,
        in_buf: u32,
        default_timeout: u32,
        security: *mut SECURITY_ATTRIBUTES,
    ) -> HANDLE;
    fn ConnectNamedPipe(pipe: HANDLE, overlapped: *mut c_void) -> i32;
    fn DisconnectNamedPipe(pipe: HANDLE) -> i32;
    fn ReadFile(
        file: HANDLE,
        buffer: *mut u8,
        to_read: u32,
        read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *mut c_void,
        disposition: u32,
        flags: u32,
        template: HANDLE,
    ) -> HANDLE;
    fn DeviceIoControl(
        device: HANDLE,
        control_code: u32,
        in_buffer: *const c_void,
        in_size: u32,
        out_buffer: *mut c_void,
        out_size: u32,
        bytes_returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CloseHandle(handle: HANDLE) -> i32;
    fn GetLastError() -> u32;
}

#[repr(C)]
struct MOUSEINPUT {
    dx: i32,
    dy: i32,
    mouse_data: u32,
    flags: u32,
    time: u32,
    extra_info: usize,
}

/// Windows INPUT structure for SendInput — x64 layout:
/// 4 bytes type + 4 bytes padding + 32 bytes union = 40 bytes
#[repr(C)]
struct INPUT {
    input_type: u32,
    _padding: u32,
    mi: MOUSEINPUT,
}

const INPUT_MOUSE: u32 = 0;
const MOUSEEVENTF_MOVE: u32 = 0x0001;
const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
const MOUSEEVENTF_LEFTUP: u32 = 0x0004;
const MOUSEEVENTF_RIGHTDOWN: u32 = 0x0008;
const MOUSEEVENTF_RIGHTUP: u32 = 0x0010;
const MOUSEEVENTF_MIDDLEDOWN: u32 = 0x0020;
const MOUSEEVENTF_MIDDLEUP: u32 = 0x0040;
const MOUSEEVENTF_WHEEL: u32 = 0x0800;
const MOUSEEVENTF_ABSOLUTE: u32 = 0x8000;
const MOUSEEVENTF_VIRTUALDESK: u32 = 0x4000;

#[link(name = "user32")]
extern "system" {
    fn GetSystemMetrics(index: i32) -> i32;
    fn SendInput(count: u32, inputs: *const INPUT, size: i32) -> u32;
}

#[link(name = "setupapi")]
extern "system" {
    fn SetupDiGetClassDevsW(
        class_guid: *const [u8; 16],
        enumerator: *const u16,
        hwnd_parent: HANDLE,
        flags: u32,
    ) -> HDEVINFO;
    fn SetupDiEnumDeviceInterfaces(
        dev_info: HDEVINFO,
        dev_info_data: *mut c_void,
        interface_class_guid: *const [u8; 16],
        member_index: u32,
        device_interface_data: *mut SP_DEVICE_INTERFACE_DATA,
    ) -> i32;
    fn SetupDiGetDeviceInterfaceDetailW(
        dev_info: HDEVINFO,
        device_interface_data: *mut SP_DEVICE_INTERFACE_DATA,
        detail_data: *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W,
        detail_data_size: u32,
        required_size: *mut u32,
        device_info_data: *mut c_void,
    ) -> i32;
    fn SetupDiDestroyDeviceInfoList(dev_info: HDEVINFO) -> i32;
}

const SM_CXVIRTUALSCREEN: i32 = 78;
const SM_CYVIRTUALSCREEN: i32 = 79;
const SM_XVIRTUALSCREEN: i32 = 76;
const SM_YVIRTUALSCREEN: i32 = 77;

// ── Per-session input state ─────────────────────────────────────────────

struct SessionInputState {
    modifiers: u8,
    pressed_keys: Vec<u8>,
    buttons: u8,
    /// Display region: (x, y, width, height) in virtual desktop
    display_rect: (i32, i32, u32, u32),
    event_count: u32,
}

impl SessionInputState {
    fn new(display_rect: (i32, i32, u32, u32)) -> Self {
        Self {
            modifiers: 0,
            pressed_keys: Vec::new(),
            buttons: 0,
            display_rect,
            event_count: 0,
        }
    }

    fn reset(&mut self) {
        self.buttons = 0;
        self.modifiers = 0;
        self.pressed_keys.clear();
        self.event_count = 0;
    }
}

/// Shared input router state accessible from the API layer.
pub struct InputRouterState {
    vhf_handle: HANDLE,
    /// Security attributes kept alive for pipe creation
    _sd: Box<SECURITY_DESCRIPTOR>,
    sa: Box<SECURITY_ATTRIBUTES>,
}

// SAFETY: HANDLE is just a pointer, and we only use it from one thread at a time
// via the session pipe threads which serialize access.
unsafe impl Send for InputRouterState {}
unsafe impl Sync for InputRouterState {}

impl InputRouterState {
    /// Create a named pipe for a session and spawn a handler thread.
    pub fn spawn_session_pipe(
        self: &Arc<Self>,
        session_id: String,
        display_rect: (i32, i32, u32, u32),
    ) {
        let router = self.clone();
        std::thread::spawn(move || {
            run_session_pipe(&router, &session_id, display_rect);
        });
    }
}

// ── USB HID scancode mapping (same as service) ─────────────────────────

fn map_hid(key: &str) -> Option<u8> {
    match key {
        "a" | "A" => Some(0x04),
        "b" | "B" => Some(0x05),
        "c" | "C" => Some(0x06),
        "d" | "D" => Some(0x07),
        "e" | "E" => Some(0x08),
        "f" | "F" => Some(0x09),
        "g" | "G" => Some(0x0A),
        "h" | "H" => Some(0x0B),
        "i" | "I" => Some(0x0C),
        "j" | "J" => Some(0x0D),
        "k" | "K" => Some(0x0E),
        "l" | "L" => Some(0x0F),
        "m" | "M" => Some(0x10),
        "n" | "N" => Some(0x11),
        "o" | "O" => Some(0x12),
        "p" | "P" => Some(0x13),
        "q" | "Q" => Some(0x14),
        "r" | "R" => Some(0x15),
        "s" | "S" => Some(0x16),
        "t" | "T" => Some(0x17),
        "u" | "U" => Some(0x18),
        "v" | "V" => Some(0x19),
        "w" | "W" => Some(0x1A),
        "x" | "X" => Some(0x1B),
        "y" | "Y" => Some(0x1C),
        "z" | "Z" => Some(0x1D),
        "1" | "!" => Some(0x1E),
        "2" | "@" => Some(0x1F),
        "3" | "#" => Some(0x20),
        "4" | "$" => Some(0x21),
        "5" | "%" => Some(0x22),
        "6" | "^" => Some(0x23),
        "7" | "&" => Some(0x24),
        "8" | "*" => Some(0x25),
        "9" | "(" => Some(0x26),
        "0" | ")" => Some(0x27),
        "Enter" => Some(0x28),
        "Escape" => Some(0x29),
        "Backspace" => Some(0x2A),
        "Tab" => Some(0x2B),
        " " => Some(0x2C),
        "-" | "_" => Some(0x2D),
        "=" | "+" => Some(0x2E),
        "[" | "{" => Some(0x2F),
        "]" | "}" => Some(0x30),
        "\\" | "|" => Some(0x31),
        ";" | ":" => Some(0x33),
        "'" | "\"" => Some(0x34),
        "`" | "~" => Some(0x35),
        "," | "<" => Some(0x36),
        "." | ">" => Some(0x37),
        "/" | "?" => Some(0x38),
        "CapsLock" => Some(0x39),
        "F1" => Some(0x3A),
        "F2" => Some(0x3B),
        "F3" => Some(0x3C),
        "F4" => Some(0x3D),
        "F5" => Some(0x3E),
        "F6" => Some(0x3F),
        "F7" => Some(0x40),
        "F8" => Some(0x41),
        "F9" => Some(0x42),
        "F10" => Some(0x43),
        "F11" => Some(0x44),
        "F12" => Some(0x45),
        "PrintScreen" => Some(0x46),
        "ScrollLock" => Some(0x47),
        "Pause" => Some(0x48),
        "Insert" => Some(0x49),
        "Home" => Some(0x4A),
        "PageUp" => Some(0x4B),
        "Delete" => Some(0x4C),
        "End" => Some(0x4D),
        "PageDown" => Some(0x4E),
        "ArrowRight" => Some(0x4F),
        "ArrowLeft" => Some(0x50),
        "ArrowDown" => Some(0x51),
        "ArrowUp" => Some(0x52),
        "NumLock" => Some(0x53),
        _ => None,
    }
}

fn modifier_bit(key: &str) -> Option<u8> {
    match key {
        "Control" => Some(0x01),
        "Shift" => Some(0x02),
        "Alt" => Some(0x04),
        "Meta" => Some(0x08),
        _ => None,
    }
}

// ── VHID device open ───────────────────────────────────────────────────

fn open_vhf_device() -> Option<HANDLE> {
    // GUID_DEVINTERFACE_STREAMIO_VHID = {A8B3F5E1-7D2C-4E9A-B6F0-1A3C5D8E2F4B}
    let guid: [u8; 16] = [
        0xE1, 0xF5, 0xB3, 0xA8, 0x2C, 0x7D, 0x9A, 0x4E, 0xB6, 0xF0, 0x1A, 0x3C, 0x5D, 0x8E,
        0x2F, 0x4B,
    ];

    unsafe {
        let dev_info = SetupDiGetClassDevsW(
            &guid,
            std::ptr::null(),
            std::ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        );
        if dev_info == INVALID_HANDLE_VALUE || dev_info.is_null() {
            error!("SetupDiGetClassDevsW failed: {}", GetLastError());
            return None;
        }

        let mut iface_data: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
        iface_data.cb_size = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;

        if SetupDiEnumDeviceInterfaces(
            dev_info,
            std::ptr::null_mut(),
            &guid,
            0,
            &mut iface_data,
        ) == 0
        {
            error!(
                "VHF device not found (SetupDiEnumDeviceInterfaces: {})",
                GetLastError()
            );
            SetupDiDestroyDeviceInfoList(dev_info);
            return None;
        }

        let mut required_size = 0u32;
        SetupDiGetDeviceInterfaceDetailW(
            dev_info,
            &mut iface_data,
            std::ptr::null_mut(),
            0,
            &mut required_size,
            std::ptr::null_mut(),
        );

        let mut detail_buf = vec![0u8; required_size as usize];
        let detail = detail_buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
        (*detail).cb_size = 8; // 64-bit

        if SetupDiGetDeviceInterfaceDetailW(
            dev_info,
            &mut iface_data,
            detail,
            required_size,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) == 0
        {
            error!(
                "SetupDiGetDeviceInterfaceDetailW failed: {}",
                GetLastError()
            );
            SetupDiDestroyDeviceInfoList(dev_info);
            return None;
        }

        let path_ptr = &(*detail).device_path as *const u16;
        let path_len = (required_size as usize - 4) / 2;
        let path_slice = std::slice::from_raw_parts(path_ptr, path_len);
        let path_end = path_slice.iter().position(|&c| c == 0).unwrap_or(path_len);
        let path = String::from_utf16_lossy(&path_slice[..path_end]);
        info!("VHF device path: {}", path);

        SetupDiDestroyDeviceInfoList(dev_info);

        let mut path_wide: Vec<u16> = path.encode_utf16().collect();
        path_wide.push(0);

        let handle = CreateFileW(
            path_wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );

        if handle == INVALID_HANDLE_VALUE {
            error!("Failed to open VHF device: {}", GetLastError());
            return None;
        }

        info!("VHF device opened successfully");
        Some(handle)
    }
}

// ── VHID report submission ─────────────────────────────────────────────

fn send_keyboard_report(vhf: HANDLE, state: &SessionInputState) {
    let mut report = VhidKeyboardReport {
        report_id: 1,
        modifiers: state.modifiers,
        reserved: 0,
        keys: [0; 6],
    };
    for (i, &k) in state.pressed_keys.iter().take(6).enumerate() {
        report.keys[i] = k;
    }

    unsafe {
        let mut returned = 0u32;
        let ok = DeviceIoControl(
            vhf,
            IOCTL_VHID_SUBMIT_KEYBOARD,
            &report as *const _ as *const c_void,
            std::mem::size_of::<VhidKeyboardReport>() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        );
        if ok == 0 {
            error!("VHID keyboard ioctl failed: {}", GetLastError());
        }
    }
}

fn send_mouse_report(vhf: HANDLE, buttons: u8, x: i16, y: i16, wheel: i8) -> bool {
    let report = VhidMouseReport {
        report_id: 2,
        buttons,
        x,
        y,
        wheel,
    };

    unsafe {
        let mut returned = 0u32;
        let ok = DeviceIoControl(
            vhf,
            IOCTL_VHID_SUBMIT_MOUSE,
            &report as *const _ as *const c_void,
            std::mem::size_of::<VhidMouseReport>() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        );
        if ok == 0 {
            error!("VHID mouse ioctl failed: {}", GetLastError());
            return false;
        }
        true
    }
}

/// Convert pixel coordinates (already offset to display region) to HID absolute (0-32767).
fn to_hid_abs(x: i32, y: i32) -> (i16, i16) {
    unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);
        let hx = ((x - vx) * 32767 / vw).clamp(0, 32767) as i16;
        let hy = ((y - vy) * 32767 / vh).clamp(0, 32767) as i16;
        (hx, hy)
    }
}

/// Send mouse input via SendInput (proven to work, unlike VHID mouse).
/// Coordinates are pixel positions in the virtual desktop.
fn send_mouse_sendinput(x: i32, y: i32, flags: u32, mouse_data: u32) {
    unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);

        // SendInput absolute coordinates: 0-65535 mapped to virtual desktop
        let dx = ((x - vx) * 65535 / vw).clamp(0, 65535);
        let dy = ((y - vy) * 65535 / vh).clamp(0, 65535);

        let input = INPUT {
            input_type: INPUT_MOUSE,
            _padding: 0,
            mi: MOUSEINPUT {
                dx,
                dy,
                mouse_data,
                flags: flags | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                extra_info: 0,
            },
        };

        let sent = SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
        if sent == 0 {
            error!("SendInput failed: {}", GetLastError());
        }
    }
}

/// Translate input event coordinates to the session's display region.
fn translate_event(
    event: streamio_types::InputEvent,
    rect: (i32, i32, u32, u32),
) -> streamio_types::InputEvent {
    let (dx, dy, _dw, _dh) = rect;
    match event {
        streamio_types::InputEvent::MouseMove { x, y } => {
            streamio_types::InputEvent::MouseMove {
                x: x + dx,
                y: y + dy,
            }
        }
        streamio_types::InputEvent::MouseDown { button, x, y } => {
            streamio_types::InputEvent::MouseDown {
                button,
                x: x + dx,
                y: y + dy,
            }
        }
        streamio_types::InputEvent::MouseUp { button, x, y } => {
            streamio_types::InputEvent::MouseUp {
                button,
                x: x + dx,
                y: y + dy,
            }
        }
        other => other,
    }
}

/// Handle a single input event for a session.
fn handle_event(vhf: HANDLE, state: &mut SessionInputState, event: streamio_types::InputEvent) {
    // Translate coordinates to display region
    let event = translate_event(event, state.display_rect);

    // Log first few events and ALL clicks for debugging
    state.event_count += 1;
    let is_click = matches!(&event,
        streamio_types::InputEvent::MouseDown { .. } |
        streamio_types::InputEvent::MouseUp { .. } |
        streamio_types::InputEvent::KeyDown { .. } |
        streamio_types::InputEvent::KeyUp { .. }
    );
    if state.event_count <= 5 || is_click {
        match &event {
            streamio_types::InputEvent::MouseMove { x, y } => {
                info!("Input #{}: Move ({},{})", state.event_count, x, y);
            }
            other => {
                info!("Input #{}: {:?}", state.event_count, other);
            }
        }
    }

    match &event {
        streamio_types::InputEvent::MouseMove { x, y } => {
            send_mouse_sendinput(*x, *y, MOUSEEVENTF_MOVE, 0);
        }
        streamio_types::InputEvent::MouseDown { button, x, y } => {
            let btn_flag = match button {
                0 => MOUSEEVENTF_LEFTDOWN,
                1 => MOUSEEVENTF_MIDDLEDOWN,
                2 => MOUSEEVENTF_RIGHTDOWN,
                _ => MOUSEEVENTF_LEFTDOWN,
            };
            send_mouse_sendinput(*x, *y, MOUSEEVENTF_MOVE | btn_flag, 0);
        }
        streamio_types::InputEvent::MouseUp { button, x, y } => {
            let btn_flag = match button {
                0 => MOUSEEVENTF_LEFTUP,
                1 => MOUSEEVENTF_MIDDLEUP,
                2 => MOUSEEVENTF_RIGHTUP,
                _ => MOUSEEVENTF_LEFTUP,
            };
            send_mouse_sendinput(*x, *y, MOUSEEVENTF_MOVE | btn_flag, 0);
        }
        streamio_types::InputEvent::Scroll { dx: _, dy } => {
            // WHEEL_DELTA = 120, scale the float dy
            let wheel_amount = (-*dy * 120.0).clamp(-12000.0, 12000.0) as i32;
            if wheel_amount != 0 {
                send_mouse_sendinput(0, 0, MOUSEEVENTF_WHEEL, wheel_amount as u32);
            }
        }
        streamio_types::InputEvent::KeyDown { key, .. } => {
            if let Some(mod_bit) = modifier_bit(key) {
                state.modifiers |= mod_bit;
                send_keyboard_report(vhf, state);
            } else if let Some(hid_code) = map_hid(key) {
                if !state.pressed_keys.contains(&hid_code) {
                    state.pressed_keys.push(hid_code);
                }
                send_keyboard_report(vhf, state);
            }
        }
        streamio_types::InputEvent::KeyUp { key, .. } => {
            if let Some(mod_bit) = modifier_bit(key) {
                state.modifiers &= !mod_bit;
                send_keyboard_report(vhf, state);
            } else if let Some(hid_code) = map_hid(key) {
                state.pressed_keys.retain(|&k| k != hid_code);
                send_keyboard_report(vhf, state);
            }
        }
    }
}

// ── Per-session pipe server ────────────────────────────────────────────

/// Run a named pipe server for a single session. Blocks until session ends.
fn run_session_pipe(
    router: &InputRouterState,
    session_id: &str,
    display_rect: (i32, i32, u32, u32),
) {
    let pipe_name = format!("\\\\.\\pipe\\streamio-input-{}\0", session_id);
    info!(
        "Starting input pipe for session {} (display rect: {:?})",
        session_id, display_rect
    );

    let mut session_state = SessionInputState::new(display_rect);

    loop {
        unsafe {
            let pipe = CreateNamedPipeA(
                pipe_name.as_ptr(),
                0x00000003, // PIPE_ACCESS_DUPLEX
                0,          // byte mode
                10,
                4096,
                4096,
                1000,
                // Cast sa pointer — we need a mutable pointer but the SA is stable
                &*router.sa as *const SECURITY_ATTRIBUTES as *mut SECURITY_ATTRIBUTES,
            );
            if pipe == INVALID_HANDLE_VALUE {
                error!(
                    "CreateNamedPipe failed for session {}: {}",
                    session_id,
                    GetLastError()
                );
                std::thread::sleep(std::time::Duration::from_secs(1));
                continue;
            }

            info!("Session {} pipe waiting for backend...", session_id);
            if ConnectNamedPipe(pipe, std::ptr::null_mut()) == 0 {
                let err = GetLastError();
                if err != 535 {
                    // ERROR_PIPE_CONNECTED
                    CloseHandle(pipe);
                    continue;
                }
            }

            info!("Session {} backend connected to input pipe", session_id);
            session_state.reset();
            // Release all via VHID
            send_mouse_report(router.vhf_handle, 0, 0, 0, 0);
            send_keyboard_report(router.vhf_handle, &session_state);

            let mut len_buf = [0u8; 4];
            let mut read = 0u32;
            let mut msg_count = 0u32;

            loop {
                if ReadFile(
                    pipe,
                    len_buf.as_mut_ptr(),
                    4,
                    &mut read,
                    std::ptr::null_mut(),
                ) == 0
                    || read != 4
                {
                    break;
                }
                let msg_len = u32::from_le_bytes(len_buf) as usize;
                if msg_len > 65536 {
                    break;
                }
                let mut msg_buf = vec![0u8; msg_len];
                if ReadFile(
                    pipe,
                    msg_buf.as_mut_ptr(),
                    msg_len as u32,
                    &mut read,
                    std::ptr::null_mut(),
                ) == 0
                    || read as usize != msg_len
                {
                    break;
                }
                match serde_json::from_slice::<streamio_types::InputEvent>(&msg_buf) {
                    Ok(event) => {
                        handle_event(router.vhf_handle, &mut session_state, event);
                    }
                    Err(e) => {
                        warn!("Session {} JSON parse error: {}", session_id, e);
                    }
                }
                msg_count += 1;
            }

            // Release all keys/buttons on disconnect
            session_state.reset();
            send_mouse_report(router.vhf_handle, 0, 0, 0, 0);
            send_keyboard_report(router.vhf_handle, &session_state);

            DisconnectNamedPipe(pipe);
            CloseHandle(pipe);
            info!(
                "Session {} backend disconnected ({} messages)",
                session_id, msg_count
            );
        }
    }
}

// ── Initialization ─────────────────────────────────────────────────────

/// Start the input router. Opens the VHID device and returns shared state.
pub async fn start(_sm_state: Arc<SessionManagerState>) -> Arc<InputRouterState> {
    let vhf_handle = open_vhf_device().expect("VHID driver not available — install streamio-vhid.sys first");

    // Create NULL DACL security attributes for pipes
    let mut sd: SECURITY_DESCRIPTOR = unsafe { std::mem::zeroed() };
    unsafe {
        InitializeSecurityDescriptor(&mut sd, 1);
        SetSecurityDescriptorDacl(&mut sd, 1, std::ptr::null_mut(), 0);
    }
    let sd = Box::new(sd);

    let sa = Box::new(SECURITY_ATTRIBUTES {
        length: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        security_descriptor: &*sd as *const SECURITY_DESCRIPTOR as *mut SECURITY_DESCRIPTOR
            as *mut c_void,
        inherit_handle: 0,
    });

    info!("Input router initialized with VHID device");

    Arc::new(InputRouterState {
        vhf_handle,
        _sd: sd,
        sa,
    })
}
