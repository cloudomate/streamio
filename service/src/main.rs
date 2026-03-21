//! Streamio Input Service
//!
//! Listens on \\.\pipe\streamio-input for InputEvent JSON from the backend,
//! and injects input via the Streamio VHF kernel driver (streamio-vhid.sys).
//!
//! The VHF driver creates a virtual keyboard + mouse at the HID level,
//! bypassing UIPI entirely — input works on the lock screen, UAC prompts,
//! and secure desktop.
//!
//! Falls back to SendInput if the VHF driver is not installed.

fn main() {
    #[cfg(windows)]
    {
        // Early crash diagnostics — write to file only (no eprintln,
        // stderr pipe may be closed if parent process died → os error 232)
        std::panic::set_hook(Box::new(|info| {
            let msg = format!("PANIC: {}", info);
            let _ = std::fs::write(r"C:\build\service-panic.log", &msg);
        }));
        let _ = std::fs::write(r"C:\build\service-start.log", "service main() entered\n");

        // Single-instance guard: use a named mutex to prevent duplicates
        #[link(name = "kernel32")]
        extern "system" {
            fn CreateMutexA(sa: *mut std::ffi::c_void, own: i32, name: *const u8) -> *mut std::ffi::c_void;
            fn GetLastError() -> u32;
        }
        let mutex_name = b"Global\\StreamioInputService\0";
        let handle = unsafe {
            CreateMutexA(std::ptr::null_mut(), 1, mutex_name.as_ptr())
        };
        if handle.is_null() || unsafe { GetLastError() } == 183 {
            let _ = std::fs::write(r"C:\build\service-start.log", "Another instance running, exiting.\n");
            std::process::exit(0);
        }

        run_pipe_server();
    }
    #[cfg(not(windows))]
    {
        eprintln!("This helper only runs on Windows.");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn run_pipe_server() {
    type HANDLE = *mut std::ffi::c_void;
    const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;

    // ── IOCTL codes (must match driver/vhid/streamio-vhid.h) ──────────

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

    // ── HID report structs (must match driver) ─────────────────────────

    #[repr(C, packed)]
    #[derive(Clone, Copy)]
    struct VhidKeyboardReport {
        report_id: u8,   // 1
        modifiers: u8,   // modifier bitmap
        reserved: u8,    // 0
        keys: [u8; 6],   // USB HID keycodes
    }

    #[repr(C, packed)]
    #[derive(Clone, Copy)]
    struct VhidMouseReport {
        report_id: u8,   // 2
        buttons: u8,     // bit0=left, bit1=right, bit2=middle
        x: i16,          // 0-32767
        y: i16,          // 0-32767
        wheel: i8,       // -127 to 127
    }

    // ── Win32 FFI ──────────────────────────────────────────────────────

    #[repr(C)]
    struct SECURITY_ATTRIBUTES {
        length: u32,
        security_descriptor: *mut std::ffi::c_void,
        inherit_handle: i32,
    }
    #[repr(C)]
    struct SECURITY_DESCRIPTOR {
        revision: u8, sbz1: u8, control: u16,
        owner: *mut std::ffi::c_void, group: *mut std::ffi::c_void,
        sacl: *mut std::ffi::c_void, dacl: *mut std::ffi::c_void,
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn InitializeSecurityDescriptor(sd: *mut SECURITY_DESCRIPTOR, revision: u32) -> i32;
        fn SetSecurityDescriptorDacl(sd: *mut SECURITY_DESCRIPTOR, present: i32, dacl: *mut std::ffi::c_void, defaulted: i32) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateNamedPipeA(
            name: *const u8, open_mode: u32, pipe_mode: u32,
            max_instances: u32, out_buf: u32, in_buf: u32,
            default_timeout: u32, security: *mut SECURITY_ATTRIBUTES,
        ) -> HANDLE;
        fn ConnectNamedPipe(pipe: HANDLE, overlapped: *mut std::ffi::c_void) -> i32;
        fn DisconnectNamedPipe(pipe: HANDLE) -> i32;
        fn ReadFile(
            file: HANDLE, buffer: *mut u8, to_read: u32,
            read: *mut u32, overlapped: *mut std::ffi::c_void,
        ) -> i32;
        fn CreateFileW(
            name: *const u16, access: u32, share: u32,
            security: *mut std::ffi::c_void, disposition: u32,
            flags: u32, template: HANDLE,
        ) -> HANDLE;
        fn DeviceIoControl(
            device: HANDLE, control_code: u32,
            in_buffer: *const std::ffi::c_void, in_size: u32,
            out_buffer: *mut std::ffi::c_void, out_size: u32,
            bytes_returned: *mut u32, overlapped: *mut std::ffi::c_void,
        ) -> i32;
        fn CloseHandle(handle: HANDLE) -> i32;
        fn GetLastError() -> u32;
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetSystemMetrics(index: i32) -> i32;
    }

    const SM_CXVIRTUALSCREEN: i32 = 78;
    const SM_CYVIRTUALSCREEN: i32 = 79;
    const SM_XVIRTUALSCREEN: i32 = 76;
    const SM_YVIRTUALSCREEN: i32 = 77;

    // SetupDi functions to find device by interface GUID
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
        device_path: [u16; 1], // variable length
    }

    type HDEVINFO = *mut std::ffi::c_void;
    const DIGCF_PRESENT: u32 = 0x02;
    const DIGCF_DEVICEINTERFACE: u32 = 0x10;
    const GENERIC_READ: u32 = 0x80000000;
    const GENERIC_WRITE: u32 = 0x40000000;
    const OPEN_EXISTING: u32 = 3;

    #[link(name = "setupapi")]
    extern "system" {
        fn SetupDiGetClassDevsW(
            class_guid: *const [u8; 16], enumerator: *const u16,
            hwnd_parent: HANDLE, flags: u32,
        ) -> HDEVINFO;
        fn SetupDiEnumDeviceInterfaces(
            dev_info: HDEVINFO, dev_info_data: *mut std::ffi::c_void,
            interface_class_guid: *const [u8; 16], member_index: u32,
            device_interface_data: *mut SP_DEVICE_INTERFACE_DATA,
        ) -> i32;
        fn SetupDiGetDeviceInterfaceDetailW(
            dev_info: HDEVINFO,
            device_interface_data: *mut SP_DEVICE_INTERFACE_DATA,
            detail_data: *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W,
            detail_data_size: u32,
            required_size: *mut u32,
            device_info_data: *mut std::ffi::c_void,
        ) -> i32;
        fn SetupDiDestroyDeviceInfoList(dev_info: HDEVINFO) -> i32;
    }

    // ── Logging ────────────────────────────────────────────────────────

    fn svc_log(msg: &str) {
        // Do NOT use eprintln! — stderr pipe may be closed (os error 232)
        // which causes a panic and crashes the service.
        static LOG: std::sync::OnceLock<std::sync::Mutex<Option<std::fs::File>>> =
            std::sync::OnceLock::new();
        let lock = LOG.get_or_init(|| {
            std::sync::Mutex::new(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(r"C:\build\service.log")
                    .ok(),
            )
        });
        if let Ok(mut g) = lock.lock() {
            if let Some(ref mut f) = *g {
                use std::io::Write;
                let _ = writeln!(f, "{}", msg);
                let _ = f.flush();
            }
        }
    }

    // ── Open VHF device by interface GUID ──────────────────────────────

    fn open_vhf_device() -> Option<HANDLE> {
        // GUID_DEVINTERFACE_STREAMIO_VHID = {A8B3F5E1-7D2C-4E9A-B6F0-1A3C5D8E2F4B}
        let guid: [u8; 16] = [
            0xE1, 0xF5, 0xB3, 0xA8,  // Data1 LE
            0x2C, 0x7D,              // Data2 LE
            0x9A, 0x4E,              // Data3 LE
            0xB6, 0xF0, 0x1A, 0x3C, 0x5D, 0x8E, 0x2F, 0x4B,
        ];

        unsafe {
            let dev_info = SetupDiGetClassDevsW(
                &guid,
                std::ptr::null(),
                std::ptr::null_mut(),
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            );
            if dev_info == INVALID_HANDLE_VALUE || dev_info.is_null() {
                svc_log(&format!("SetupDiGetClassDevsW failed: {}", GetLastError()));
                return None;
            }

            let mut iface_data: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
            iface_data.cb_size = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;

            if SetupDiEnumDeviceInterfaces(
                dev_info, std::ptr::null_mut(), &guid, 0, &mut iface_data,
            ) == 0 {
                svc_log(&format!("SetupDiEnumDeviceInterfaces failed: {}", GetLastError()));
                SetupDiDestroyDeviceInfoList(dev_info);
                return None;
            }

            // Get required size
            let mut required_size = 0u32;
            SetupDiGetDeviceInterfaceDetailW(
                dev_info, &mut iface_data, std::ptr::null_mut(), 0,
                &mut required_size, std::ptr::null_mut(),
            );

            // Allocate and fill detail data
            let mut detail_buf = vec![0u8; required_size as usize];
            let detail = detail_buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            // cbSize must be size of the fixed part (on 64-bit: 8)
            (*detail).cb_size = 8;

            if SetupDiGetDeviceInterfaceDetailW(
                dev_info, &mut iface_data, detail, required_size,
                std::ptr::null_mut(), std::ptr::null_mut(),
            ) == 0 {
                svc_log(&format!("SetupDiGetDeviceInterfaceDetailW failed: {}", GetLastError()));
                SetupDiDestroyDeviceInfoList(dev_info);
                return None;
            }

            // Extract device path
            let path_ptr = &(*detail).device_path as *const u16;
            let path_len = (required_size as usize - 4) / 2; // subtract cbSize, divide by u16
            let path_slice = std::slice::from_raw_parts(path_ptr, path_len);
            let path_end = path_slice.iter().position(|&c| c == 0).unwrap_or(path_len);
            let path = String::from_utf16_lossy(&path_slice[..path_end]);
            svc_log(&format!("VHF device path: {}", path));

            SetupDiDestroyDeviceInfoList(dev_info);

            // Open the device
            let mut path_wide: Vec<u16> = path.encode_utf16().collect();
            path_wide.push(0);

            let handle = CreateFileW(
                path_wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0, // no sharing
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );

            if handle == INVALID_HANDLE_VALUE {
                svc_log(&format!("CreateFileW failed: {}", GetLastError()));
                return None;
            }

            svc_log("VHF device opened successfully");
            Some(handle)
        }
    }

    // ── USB HID scancode mapping ───────────────────────────────────────
    // Maps browser key names to USB HID Usage IDs (Usage Page 0x07)

    fn map_hid(key: &str) -> Option<u8> {
        match key {
            "a" | "A" => Some(0x04), "b" | "B" => Some(0x05),
            "c" | "C" => Some(0x06), "d" | "D" => Some(0x07),
            "e" | "E" => Some(0x08), "f" | "F" => Some(0x09),
            "g" | "G" => Some(0x0A), "h" | "H" => Some(0x0B),
            "i" | "I" => Some(0x0C), "j" | "J" => Some(0x0D),
            "k" | "K" => Some(0x0E), "l" | "L" => Some(0x0F),
            "m" | "M" => Some(0x10), "n" | "N" => Some(0x11),
            "o" | "O" => Some(0x12), "p" | "P" => Some(0x13),
            "q" | "Q" => Some(0x14), "r" | "R" => Some(0x15),
            "s" | "S" => Some(0x16), "t" | "T" => Some(0x17),
            "u" | "U" => Some(0x18), "v" | "V" => Some(0x19),
            "w" | "W" => Some(0x1A), "x" | "X" => Some(0x1B),
            "y" | "Y" => Some(0x1C), "z" | "Z" => Some(0x1D),
            "1" | "!" => Some(0x1E), "2" | "@" => Some(0x1F),
            "3" | "#" => Some(0x20), "4" | "$" => Some(0x21),
            "5" | "%" => Some(0x22), "6" | "^" => Some(0x23),
            "7" | "&" => Some(0x24), "8" | "*" => Some(0x25),
            "9" | "(" => Some(0x26), "0" | ")" => Some(0x27),
            "Enter" => Some(0x28),
            "Escape" => Some(0x29),
            "Backspace" => Some(0x2A),
            "Tab" => Some(0x2B),
            " " => Some(0x2C),
            "-" | "_" => Some(0x2D), "=" | "+" => Some(0x2E),
            "[" | "{" => Some(0x2F), "]" | "}" => Some(0x30),
            "\\" | "|" => Some(0x31),
            ";" | ":" => Some(0x33), "'" | "\"" => Some(0x34),
            "`" | "~" => Some(0x35),
            "," | "<" => Some(0x36), "." | ">" => Some(0x37),
            "/" | "?" => Some(0x38),
            "CapsLock" => Some(0x39),
            "F1" => Some(0x3A), "F2" => Some(0x3B), "F3" => Some(0x3C),
            "F4" => Some(0x3D), "F5" => Some(0x3E), "F6" => Some(0x3F),
            "F7" => Some(0x40), "F8" => Some(0x41), "F9" => Some(0x42),
            "F10" => Some(0x43), "F11" => Some(0x44), "F12" => Some(0x45),
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

    /// Maps browser key name to HID modifier bit (for the modifier byte)
    fn modifier_bit(key: &str) -> Option<u8> {
        match key {
            "Control" => Some(0x01),      // Left Control
            "Shift" => Some(0x02),        // Left Shift
            "Alt" => Some(0x04),          // Left Alt
            "Meta" => Some(0x08),         // Left GUI (Windows key)
            _ => None,
        }
    }

    // ── Input injection via VHF ────────────────────────────────────────

    struct InputState {
        vhf_handle: HANDLE,
        // Track modifier keys and pressed keys for keyboard reports
        modifiers: u8,
        pressed_keys: Vec<u8>, // USB HID keycodes currently held
        // Track mouse button state
        buttons: u8,
    }

    impl InputState {
        fn new(vhf_handle: HANDLE) -> Self {
            Self {
                vhf_handle,
                modifiers: 0,
                pressed_keys: Vec::new(),
                buttons: 0,
            }
        }

        fn reset(&mut self) {
            // Release all buttons and keys when client disconnects/reconnects
            self.buttons = 0;
            self.send_mouse_report(0, 0, 0);
            self.modifiers = 0;
            self.pressed_keys.clear();
            self.send_keyboard_report();
            svc_log("Reset input state (all buttons/keys released)");
        }

        fn send_keyboard_report(&self) {
            let mut report = VhidKeyboardReport {
                report_id: 1,
                modifiers: self.modifiers,
                reserved: 0,
                keys: [0; 6],
            };
            // Fill up to 6 keys
            for (i, &k) in self.pressed_keys.iter().take(6).enumerate() {
                report.keys[i] = k;
            }

            unsafe {
                let mut returned = 0u32;
                let ok = DeviceIoControl(
                    self.vhf_handle,
                    IOCTL_VHID_SUBMIT_KEYBOARD,
                    &report as *const _ as *const std::ffi::c_void,
                    std::mem::size_of::<VhidKeyboardReport>() as u32,
                    std::ptr::null_mut(), 0,
                    &mut returned,
                    std::ptr::null_mut(),
                );
                if ok == 0 {
                    static FAIL_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                    let fc = FAIL_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if fc < 5 || fc % 500 == 0 {
                        svc_log(&format!("Keyboard IOCTL failed: {}", GetLastError()));
                    }
                }
            }
        }

        fn send_mouse_report(&self, x: i16, y: i16, wheel: i8) {
            let report = VhidMouseReport {
                report_id: 2,
                buttons: self.buttons,
                x,
                y,
                wheel,
            };

            unsafe {
                // Debug: log raw bytes
                let bytes = std::slice::from_raw_parts(
                    &report as *const _ as *const u8,
                    std::mem::size_of::<VhidMouseReport>(),
                );
                static DBG_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                let dc = DBG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if dc < 10 {
                    svc_log(&format!(
                        "Mouse report ({} bytes): {:02X?} | btn={} x={} y={} wheel={}",
                        bytes.len(), bytes, self.buttons, x, y, wheel
                    ));
                }

                let mut returned = 0u32;
                let ok = DeviceIoControl(
                    self.vhf_handle,
                    IOCTL_VHID_SUBMIT_MOUSE,
                    &report as *const _ as *const std::ffi::c_void,
                    std::mem::size_of::<VhidMouseReport>() as u32,
                    std::ptr::null_mut(), 0,
                    &mut returned,
                    std::ptr::null_mut(),
                );
                if dc < 10 {
                    svc_log(&format!("Mouse IOCTL result: ok={} returned={} err={}", ok, returned, GetLastError()));
                }
                if ok == 0 {
                    static FAIL_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                    let fc = FAIL_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if fc < 5 || fc % 500 == 0 {
                        svc_log(&format!("Mouse IOCTL failed: {}", GetLastError()));
                    }
                }
            }
        }

        /// Convert pixel coordinates to absolute HID range (0-32767)
        fn to_hid_abs(x: i32, y: i32) -> (i16, i16) {
            unsafe {
                let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
                let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
                let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
                let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);

                static LOG_ONCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                if !LOG_ONCE.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    svc_log(&format!("Virtual screen: {}x{} at ({},{})", vw, vh, vx, vy));
                }

                let hx = ((x - vx) * 32767 / vw).clamp(0, 32767) as i16;
                let hy = ((y - vy) * 32767 / vh).clamp(0, 32767) as i16;
                (hx, hy)
            }
        }

        fn handle_event(&mut self, event: &streamio_types::InputEvent) {
            match event {
                streamio_types::InputEvent::MouseMove { x, y } => {
                    let (hx, hy) = Self::to_hid_abs(*x, *y);
                    self.send_mouse_report(hx, hy, 0);
                }
                streamio_types::InputEvent::MouseDown { button, x, y } => {
                    let (hx, hy) = Self::to_hid_abs(*x, *y);
                    match button {
                        0 => self.buttons |= 0x01, // left
                        1 => self.buttons |= 0x04, // middle
                        2 => self.buttons |= 0x02, // right
                        _ => self.buttons |= 0x01,
                    }
                    svc_log(&format!("CLICK DOWN btn={} buttons=0x{:02X} at ({},{}) -> hid({},{})",
                        button, self.buttons, x, y, hx, hy));
                    self.send_mouse_report(hx, hy, 0);
                }
                streamio_types::InputEvent::MouseUp { button, x, y } => {
                    let (hx, hy) = Self::to_hid_abs(*x, *y);
                    match button {
                        0 => self.buttons &= !0x01,
                        1 => self.buttons &= !0x04,
                        2 => self.buttons &= !0x02,
                        _ => self.buttons &= !0x01,
                    }
                    svc_log(&format!("CLICK UP btn={} buttons=0x{:02X} at ({},{}) -> hid({},{})",
                        button, self.buttons, x, y, hx, hy));
                    self.send_mouse_report(hx, hy, 0);
                }
                streamio_types::InputEvent::Scroll { dx: _, dy } => {
                    let w = (-*dy * 3.0).clamp(-127.0, 127.0) as i8;
                    if w != 0 {
                        // Send scroll with current button state at position 0,0
                        // (wheel is relative, position doesn't matter for scroll-only)
                        self.send_mouse_report(0, 0, w);
                    }
                }
                streamio_types::InputEvent::KeyDown { key, .. } => {
                    if let Some(mod_bit) = modifier_bit(key) {
                        self.modifiers |= mod_bit;
                        self.send_keyboard_report();
                    } else if let Some(hid_code) = map_hid(key) {
                        if !self.pressed_keys.contains(&hid_code) {
                            self.pressed_keys.push(hid_code);
                        }
                        self.send_keyboard_report();
                    }
                }
                streamio_types::InputEvent::KeyUp { key, .. } => {
                    if let Some(mod_bit) = modifier_bit(key) {
                        self.modifiers &= !mod_bit;
                        self.send_keyboard_report();
                    } else if let Some(hid_code) = map_hid(key) {
                        self.pressed_keys.retain(|&k| k != hid_code);
                        self.send_keyboard_report();
                    }
                }
            }
        }
    }

    // ── Pipe server ────────────────────────────────────────────────────

    svc_log("Streamio input service started (VHF mode)");

    // Try to open VHF device
    let vhf_handle = open_vhf_device();
    match &vhf_handle {
        Some(_) => svc_log("Using VHF driver for input injection"),
        None => {
            svc_log("ERROR: VHF driver not available. Install streamio-vhid.sys first.");
            svc_log("Exiting.");
            return;
        }
    }

    let mut state = InputState::new(vhf_handle.unwrap());

    // NULL DACL — allow any process to connect
    let mut sd: SECURITY_DESCRIPTOR = unsafe { std::mem::zeroed() };
    unsafe {
        InitializeSecurityDescriptor(&mut sd, 1);
        SetSecurityDescriptorDacl(&mut sd, 1, std::ptr::null_mut(), 0);
    }
    let mut sa = SECURITY_ATTRIBUTES {
        length: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        security_descriptor: &mut sd as *mut SECURITY_DESCRIPTOR as *mut std::ffi::c_void,
        inherit_handle: 0,
    };

    let pipe_name = b"\\\\.\\pipe\\streamio-input\0";
    svc_log("Listening on \\\\.\\pipe\\streamio-input");

    loop {
        unsafe {
            svc_log("Creating pipe...");
            let pipe = CreateNamedPipeA(
                pipe_name.as_ptr(), 0x00000003, 0, 10, 4096, 4096, 1000, &mut sa,  // PIPE_ACCESS_DUPLEX
            );
            if pipe == INVALID_HANDLE_VALUE {
                svc_log(&format!("CreateNamedPipe failed: {}", GetLastError()));
                std::thread::sleep(std::time::Duration::from_secs(1));
                continue;
            }

            svc_log("Waiting for client...");
            if ConnectNamedPipe(pipe, std::ptr::null_mut()) == 0 {
                let err = GetLastError();
                svc_log(&format!("ConnectNamedPipe returned 0, err={}", err));
                if err != 535 { // ERROR_PIPE_CONNECTED
                    CloseHandle(pipe);
                    continue;
                }
            }

            svc_log("Client connected!");
            state.reset();

            let mut len_buf = [0u8; 4];
            let mut read = 0u32;
            let mut msg_count = 0u32;
            loop {
                if ReadFile(pipe, len_buf.as_mut_ptr(), 4, &mut read, std::ptr::null_mut()) == 0 || read != 4 {
                    svc_log(&format!("ReadFile header: read={}, err={}", read, GetLastError()));
                    break;
                }
                let msg_len = u32::from_le_bytes(len_buf) as usize;
                if msg_len > 65536 { break; }
                let mut msg_buf = vec![0u8; msg_len];
                if ReadFile(pipe, msg_buf.as_mut_ptr(), msg_len as u32, &mut read, std::ptr::null_mut()) == 0
                    || read as usize != msg_len {
                    svc_log(&format!("ReadFile body failed: read={}, err={}", read, GetLastError()));
                    break;
                }
                match serde_json::from_slice::<streamio_types::InputEvent>(&msg_buf) {
                    Ok(event) => {
                        state.handle_event(&event);
                    }
                    Err(e) => {
                        svc_log(&format!("JSON parse error: {}", e));
                    }
                }
                msg_count += 1;
            }
            DisconnectNamedPipe(pipe);
            CloseHandle(pipe);
            svc_log(&format!("Client disconnected (processed {} messages)", msg_count));
        }
    }
}
