//! Window confinement manager.
//!
//! - **Windows**: Uses SetWinEventHook to monitor window creation and movement,
//!   snapping each user's windows back to their assigned display region.
//! - **Linux/macOS**: Not needed — each user has their own X display / screen.

use crate::SessionManagerState;
use std::sync::Arc;
use tracing::info;

#[cfg(not(windows))]
pub fn start(_state: Arc<SessionManagerState>) {
    info!("Window manager: not needed on this platform (each user has own display server)");
}

#[cfg(windows)]
pub fn start(state: Arc<SessionManagerState>) {
    inner::start(state);
}

#[cfg(windows)]
mod inner {
    use crate::SessionManagerState;
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::sync::{Arc, Mutex, OnceLock};
    use tracing::{debug, error, info, warn};

    type HWND = *mut c_void;
    type HWINEVENTHOOK = *mut c_void;
    type HANDLE = *mut c_void;
    type DWORD = u32;
    type BOOL = i32;
    type LONG = i32;
    type UINT = u32;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RECT {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    struct MSG {
        hwnd: HWND,
        message: UINT,
        wparam: usize,
        lparam: isize,
        time: DWORD,
        pt_x: LONG,
        pt_y: LONG,
    }

    type WINEVENTPROC = unsafe extern "system" fn(
        HWINEVENTHOOK, DWORD, HWND, LONG, LONG, DWORD, DWORD,
    );

    const EVENT_OBJECT_LOCATIONCHANGE: DWORD = 0x800B;
    const EVENT_OBJECT_CREATE: DWORD = 0x8000;
    const WINEVENT_OUTOFCONTEXT: DWORD = 0x0000;
    const OBJID_WINDOW: LONG = 0;
    const GWL_STYLE: i32 = -16;
    const WS_VISIBLE: LONG = 0x10000000;
    const SWP_NOSIZE: UINT = 0x0001;
    const SWP_NOZORDER: UINT = 0x0004;
    const SWP_NOACTIVATE: UINT = 0x0010;
    const PROCESS_QUERY_LIMITED_INFORMATION: DWORD = 0x1000;
    const TOKEN_QUERY: DWORD = 0x0008;
    // TokenUser = 1
    const TOKEN_USER_CLASS: u32 = 1;

    #[link(name = "user32")]
    extern "system" {
        fn SetWinEventHook(
            event_min: DWORD, event_max: DWORD, hmod_win_event_proc: HANDLE,
            pfn_win_event_proc: WINEVENTPROC, id_process: DWORD,
            id_thread: DWORD, dw_flags: DWORD,
        ) -> HWINEVENTHOOK;
        fn GetWindowThreadProcessId(hwnd: HWND, process_id: *mut DWORD) -> DWORD;
        fn GetWindowRect(hwnd: HWND, rect: *mut RECT) -> BOOL;
        fn SetWindowPos(
            hwnd: HWND, hwnd_insert_after: HWND,
            x: i32, y: i32, cx: i32, cy: i32, flags: UINT,
        ) -> BOOL;
        fn GetMessageW(msg: *mut MSG, hwnd: HWND, msg_min: UINT, msg_max: UINT) -> BOOL;
        fn TranslateMessage(msg: *const MSG) -> BOOL;
        fn DispatchMessageW(msg: *const MSG) -> isize;
        fn IsWindowVisible(hwnd: HWND) -> BOOL;
        fn GetWindowLongW(hwnd: HWND, index: i32) -> LONG;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: DWORD, inherit: BOOL, pid: DWORD) -> HANDLE;
        fn CloseHandle(handle: HANDLE) -> BOOL;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(process: HANDLE, access: DWORD, token: *mut HANDLE) -> BOOL;
        fn GetTokenInformation(
            token: HANDLE, token_information_class: u32,
            info: *mut c_void, info_len: DWORD, return_len: *mut DWORD,
        ) -> BOOL;
        fn LookupAccountSidW(
            system: *const u16, sid: *mut c_void,
            name: *mut u16, name_len: *mut DWORD,
            domain: *mut u16, domain_len: *mut DWORD,
            use_type: *mut u32,
        ) -> BOOL;
    }

    // Global state accessible from the WinEvent callback
    static STATE: OnceLock<Arc<SessionManagerState>> = OnceLock::new();
    static PID_CACHE: Mutex<Option<HashMap<u32, Option<String>>>> = Mutex::new(None);

    fn pid_cache() -> std::sync::MutexGuard<'static, Option<HashMap<u32, Option<String>>>> {
        let mut guard = PID_CACHE.lock().unwrap();
        if guard.is_none() {
            *guard = Some(HashMap::new());
        }
        guard
    }

    /// Get the username that owns a process, with caching.
    fn get_process_username(pid: u32) -> Option<String> {
        // Check cache first
        {
            let cache = pid_cache();
            if let Some(cached) = cache.as_ref().unwrap().get(&pid) {
                return cached.clone();
            }
        }

        let username = unsafe { query_process_username(pid) };

        // Cache the result (even None, to avoid repeated lookups)
        {
            let mut cache = pid_cache();
            cache.as_mut().unwrap().insert(pid, username.clone());
        }

        username
    }

    unsafe fn query_process_username(pid: u32) -> Option<String> {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }

        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            CloseHandle(process);
            return None;
        }

        // Get token user (SID)
        let mut return_len: DWORD = 0;
        GetTokenInformation(
            token, TOKEN_USER_CLASS,
            std::ptr::null_mut(), 0, &mut return_len,
        );

        if return_len == 0 {
            CloseHandle(token);
            CloseHandle(process);
            return None;
        }

        let mut token_info = vec![0u8; return_len as usize];
        if GetTokenInformation(
            token, TOKEN_USER_CLASS,
            token_info.as_mut_ptr() as *mut c_void,
            return_len, &mut return_len,
        ) == 0 {
            CloseHandle(token);
            CloseHandle(process);
            return None;
        }

        // TOKEN_USER struct: first field is SID pointer
        let sid_ptr = *(token_info.as_ptr() as *const *mut c_void);

        // Lookup account name from SID
        let mut name_buf = [0u16; 256];
        let mut name_len: DWORD = 256;
        let mut domain_buf = [0u16; 256];
        let mut domain_len: DWORD = 256;
        let mut sid_type: u32 = 0;

        if LookupAccountSidW(
            std::ptr::null(), sid_ptr,
            name_buf.as_mut_ptr(), &mut name_len,
            domain_buf.as_mut_ptr(), &mut domain_len,
            &mut sid_type,
        ) == 0 {
            CloseHandle(token);
            CloseHandle(process);
            return None;
        }

        CloseHandle(token);
        CloseHandle(process);

        let name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
        Some(name)
    }

    /// WinEvent callback — called for window location changes and creation.
    unsafe extern "system" fn win_event_callback(
        _hook: HWINEVENTHOOK,
        event: DWORD,
        hwnd: HWND,
        id_object: LONG,
        _id_child: LONG,
        _event_thread: DWORD,
        _event_time: DWORD,
    ) {
        // Only handle window-level events
        if id_object != OBJID_WINDOW || hwnd.is_null() {
            return;
        }

        // Only handle visible, normal windows
        if IsWindowVisible(hwnd) == 0 {
            return;
        }
        let style = GetWindowLongW(hwnd, GWL_STYLE);
        if style & WS_VISIBLE == 0 {
            return;
        }

        // Get the PID of the window's process
        let mut pid: DWORD = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            return;
        }

        // Get the username for this PID
        let username = match get_process_username(pid) {
            Some(u) => u,
            None => return,
        };

        // Only confine streamio_* user windows
        if !username.starts_with("streamio_") {
            return;
        }

        // Find the session for this username
        let state = match STATE.get() {
            Some(s) => s,
            None => return,
        };

        // Try to read sessions without blocking (best effort)
        let sessions = match state.sessions.try_read() {
            Ok(s) => s,
            Err(_) => return,
        };

        let session = sessions.values().find(|s| s.os_user == username);
        let display_rect = match session {
            Some(s) => s.display_rect,
            None => return,
        };

        let (dx, dy, dw, dh) = display_rect;

        // Get current window position
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return;
        }

        let win_w = rect.right - rect.left;
        let win_h = rect.bottom - rect.top;

        // Check if window is within the assigned display region
        let within =
            rect.left >= dx
            && rect.top >= dy
            && rect.right <= dx + dw as i32
            && rect.bottom <= dy + dh as i32;

        if within {
            return;
        }

        // Snap the window back into the display region
        let new_x = rect.left.max(dx).min(dx + dw as i32 - win_w);
        let new_y = rect.top.max(dy).min(dy + dh as i32 - win_h);

        if new_x != rect.left || new_y != rect.top {
            debug!(
                "Confining window (pid={}, user={}) from ({},{}) to ({},{})",
                pid, username, rect.left, rect.top, new_x, new_y
            );
            SetWindowPos(
                hwnd, std::ptr::null_mut(),
                new_x, new_y, 0, 0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    pub fn start(state: Arc<SessionManagerState>) {
        STATE.set(state).ok();

        std::thread::spawn(|| {
            info!("Window manager starting (SetWinEventHook)");

            unsafe {
                // Hook window location changes
                let hook1 = SetWinEventHook(
                    EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE,
                    std::ptr::null_mut(), win_event_callback,
                    0, 0, WINEVENT_OUTOFCONTEXT,
                );
                if hook1.is_null() {
                    error!("Failed to install LOCATIONCHANGE hook");
                    return;
                }

                // Hook window creation
                let hook2 = SetWinEventHook(
                    EVENT_OBJECT_CREATE, EVENT_OBJECT_CREATE,
                    std::ptr::null_mut(), win_event_callback,
                    0, 0, WINEVENT_OUTOFCONTEXT,
                );
                if hook2.is_null() {
                    warn!("Failed to install CREATE hook (LOCATIONCHANGE still active)");
                }

                info!("Window confinement hooks installed");

                // Run message pump (required for WinEvent hooks)
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        });

        // Periodic cache cleanup thread
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
                let mut cache = pid_cache();
                if let Some(ref mut map) = *cache {
                    let old_size = map.len();
                    map.retain(|pid, _| {
                        // Keep only if process still exists
                        unsafe {
                            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, *pid);
                            if h.is_null() {
                                false
                            } else {
                                CloseHandle(h);
                                true
                            }
                        }
                    });
                    if old_size != map.len() {
                        debug!("PID cache cleaned: {} → {} entries", old_size, map.len());
                    }
                }
            }
        });
    }
}
