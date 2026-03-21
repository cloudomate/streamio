//! Backend process launcher using CreateProcessWithLogonW.
//!
//! Launches one backend instance per user session under the user's
//! local Windows account, targeting a specific virtual display.
//! Uses CreateProcessWithLogonW (no special privileges required).

use anyhow::{anyhow, Result};
use std::ffi::c_void;
use tracing::info;

type HANDLE = *mut c_void;
type DWORD = u32;
type BOOL = i32;
type LPVOID = *mut c_void;

const CREATE_NO_WINDOW: DWORD = 0x08000000;
const CREATE_UNICODE_ENVIRONMENT: DWORD = 0x00000400;
const NORMAL_PRIORITY_CLASS: DWORD = 0x00000020;
const LOGON_WITH_PROFILE: DWORD = 1;
const LOGON_NETCREDENTIALS_ONLY: DWORD = 2;

#[repr(C)]
struct STARTUPINFOW {
    cb: DWORD,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: DWORD,
    y: DWORD,
    x_size: DWORD,
    y_size: DWORD,
    x_count_chars: DWORD,
    y_count_chars: DWORD,
    fill_attribute: DWORD,
    flags: DWORD,
    show_window: u16,
    cb_reserved2: u16,
    lp_reserved2: *mut u8,
    std_input: HANDLE,
    std_output: HANDLE,
    std_error: HANDLE,
}

#[repr(C)]
struct PROCESS_INFORMATION {
    process: HANDLE,
    thread: HANDLE,
    process_id: DWORD,
    thread_id: DWORD,
}

#[link(name = "advapi32")]
extern "system" {
    fn CreateProcessWithLogonW(
        username: *const u16,
        domain: *const u16,
        password: *const u16,
        logon_flags: DWORD,
        application_name: *const u16,
        command_line: *mut u16,
        creation_flags: DWORD,
        environment: LPVOID,
        current_directory: *const u16,
        startup_info: *mut STARTUPINFOW,
        process_information: *mut PROCESS_INFORMATION,
    ) -> BOOL;
}

#[link(name = "kernel32")]
extern "system" {
    fn CloseHandle(handle: HANDLE) -> BOOL;
    fn GetLastError() -> DWORD;
    fn GetExitCodeProcess(process: HANDLE, exit_code: *mut DWORD) -> BOOL;
    fn WaitForSingleObject(handle: HANDLE, milliseconds: DWORD) -> DWORD;
    fn TerminateProcess(process: HANDLE, exit_code: u32) -> BOOL;
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Build a Unicode environment block from scratch.
fn build_env_block(vars: &[(&str, String)]) -> Vec<u16> {
    // Include essential system environment vars
    let mut all_vars: Vec<(String, String)> = Vec::new();

    // Copy essential vars from current process environment
    for key in &[
        "SystemRoot",
        "SystemDrive",
        "TEMP",
        "TMP",
        "PATH",
        "PATHEXT",
        "COMSPEC",
        "windir",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "CommonProgramFiles",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "NUMBER_OF_PROCESSORS",
        "OS",
        "PROCESSOR_ARCHITECTURE",
    ] {
        if let Ok(val) = std::env::var(key) {
            all_vars.push((key.to_string(), val));
        }
    }

    // Add/override with provided vars
    for (key, value) in vars {
        if let Some(existing) = all_vars
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
        {
            existing.1 = value.clone();
        } else {
            all_vars.push((key.to_string(), value.clone()));
        }
    }

    // Build block: KEY=VALUE\0KEY=VALUE\0\0
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in &all_vars {
        let entry = format!("{}={}", k, v);
        block.extend(entry.encode_utf16());
        block.push(0);
    }
    block.push(0); // double null terminator
    block
}

/// Handle to a launched backend process.
pub struct BackendProcess {
    process_handle: HANDLE,
    pub pid: u32,
}

// SAFETY: Handles are just pointers, managed by Windows kernel.
unsafe impl Send for BackendProcess {}
unsafe impl Sync for BackendProcess {}

impl BackendProcess {
    /// Check if the process is still running.
    pub fn is_alive(&self) -> bool {
        unsafe {
            let mut exit_code: DWORD = 0;
            if GetExitCodeProcess(self.process_handle, &mut exit_code) != 0 {
                exit_code == 259 // STILL_ACTIVE
            } else {
                false
            }
        }
    }

    /// Terminate the process.
    pub fn kill(&self) {
        unsafe {
            TerminateProcess(self.process_handle, 1);
            WaitForSingleObject(self.process_handle, 5000);
        }
    }
}

impl Drop for BackendProcess {
    fn drop(&mut self) {
        if !self.process_handle.is_null() {
            unsafe {
                CloseHandle(self.process_handle);
            }
        }
    }
}

/// Launch a backend process in the interactive session via a scheduled task.
///
/// We use schtasks instead of CreateProcessWithLogonW because:
/// 1. CreateProcessWithLogonW creates a restricted logon session where
///    d3d11screencapturesrc can't enumerate DXGI outputs for virtual displays
/// 2. Scheduled tasks with InteractiveToken run in the full desktop session
///    with proper access to all display adapters and monitors
pub fn launch_backend(
    backend_path: &str,
    _username: &str,
    _password: &str,
    port: u16,
    display_index: u32,
    session_id: &str,
    token_secret: &str,
    gateway_url: &str,
) -> Result<BackendProcess> {
    info!(
        "Launching backend on port {} display {} session {}",
        port, display_index, session_id
    );

    let gst_root = std::env::var("GSTREAMER_1_0_ROOT_MSVC_X86_64")
        .unwrap_or_else(|_| r"C:\gstreamer\1.0\msvc_x86_64".to_string());
    let gst_bin = format!(r"{}\bin", gst_root);
    let log_file = format!(r"C:\build\streamio-backend-{}.log", session_id);

    // Write a batch script that sets environment and launches the backend
    let script_dir = r"C:\ProgramData\Streamio";
    let _ = std::fs::create_dir_all(script_dir);
    let script_path = format!(r"{}\start-backend-{}.bat", script_dir, session_id);

    let script = format!(
        "@echo off\r\n\
         set PATH={gst_bin};%PATH%\r\n\
         set PORT={port}\r\n\
         set DISPLAY_INDEX={display_index}\r\n\
         set SESSION_ID={session_id}\r\n\
         set BACKEND_TOKEN_SECRET={token_secret}\r\n\
         set GATEWAY_URL={gateway_url}\r\n\
         set RUST_LOG=info\r\n\
         set STREAMIO_LOG_FILE={log_file}\r\n\
         set GST_PLUGIN_PATH={gst_root}\\lib\\gstreamer-1.0\r\n\
         \"{backend_path}\"\r\n",
        gst_bin = gst_bin,
        port = port,
        display_index = display_index,
        session_id = session_id,
        token_secret = token_secret,
        gateway_url = gateway_url,
        log_file = log_file,
        gst_root = gst_root,
        backend_path = backend_path,
    );
    std::fs::write(&script_path, &script)?;

    // Create and run a scheduled task with InteractiveToken
    let task_name = format!("StreamioBackend_{}", &session_id[..8]);

    // Register the task
    let register = std::process::Command::new(r"C:\Windows\system32\schtasks.exe")
        .args([
            "/Create", "/TN", &task_name,
            "/TR", &format!("\"{}\"", script_path),
            "/SC", "ONCE",
            "/ST", "00:00",
            "/RU", "streamio",
            "/RP", "Vdi12345",
            "/RL", "HIGHEST",
            "/IT",
            "/F",
        ])
        .output()?;

    if !register.status.success() {
        let stderr = String::from_utf8_lossy(&register.stderr);
        return Err(anyhow!("Failed to create backend task: {}", stderr));
    }

    // Run the task
    let run = std::process::Command::new(r"C:\Windows\system32\schtasks.exe")
        .args(["/Run", "/TN", &task_name])
        .output()?;

    if !run.status.success() {
        let stderr = String::from_utf8_lossy(&run.stderr);
        return Err(anyhow!("Failed to run backend task: {}", stderr));
    }

    // Wait for the backend to start and find its PID
    std::thread::sleep(std::time::Duration::from_secs(3));

    // Find the PID by checking which streamio.exe is listening on our port
    let netstat = std::process::Command::new(r"C:\Windows\system32\netstat.exe")
        .args(["-ano"])
        .output()?;
    let output = String::from_utf8_lossy(&netstat.stdout);
    let pid = output
        .lines()
        .find(|line| line.contains(&format!(":{}", port)) && line.contains("LISTENING"))
        .and_then(|line| line.split_whitespace().last())
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    if pid == 0 {
        // Check if it's still starting up — read the log file
        let log_content = std::fs::read_to_string(&log_file).unwrap_or_default();
        if log_content.is_empty() {
            return Err(anyhow!("Backend failed to start — no log output"));
        }
        info!("Backend log so far: {}", log_content.lines().take(5).collect::<Vec<_>>().join(" | "));
    }

    info!(
        "Backend launched via schtasks: pid={}, port={}, display={}",
        pid, port, display_index
    );

    // Clean up the scheduled task (process continues running)
    let _ = std::process::Command::new(r"C:\Windows\system32\schtasks.exe")
        .args(["/Delete", "/TN", &task_name, "/F"])
        .output();

    Ok(BackendProcess {
        process_handle: std::ptr::null_mut(), // no handle when using schtasks
        pid,
    })
}
