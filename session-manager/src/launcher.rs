//! Backend process launcher.
//!
//! Launches one backend instance per user session under the user's
//! local OS account, targeting a specific virtual display.
//!
//! - **Windows**: `schtasks.exe` with InteractiveToken (DXGI requires full desktop session)
//! - **Linux**: `sudo -u {user}` with DISPLAY=:{N}
//! - **macOS**: `sudo -u {user}` with env vars

use anyhow::{anyhow, Result};
use tracing::info;

/// Handle to a launched backend process.
pub struct BackendProcess {
    #[cfg(windows)]
    process_handle: *mut std::ffi::c_void,
    #[cfg(not(windows))]
    child: Option<std::process::Child>,
    pub pid: u32,
}

// SAFETY: Handles/pids are managed by the OS kernel.
unsafe impl Send for BackendProcess {}
unsafe impl Sync for BackendProcess {}

impl BackendProcess {
    /// Check if the process is still running.
    pub fn is_alive(&self) -> bool {
        #[cfg(windows)]
        {
            windows_impl::is_alive(self.process_handle)
        }
        #[cfg(not(windows))]
        {
            // Check if process is still running via kill -0
            let output = std::process::Command::new("kill")
                .args(["-0", &self.pid.to_string()])
                .output();
            output.map(|o| o.status.success()).unwrap_or(false)
        }
    }

    /// Terminate the process.
    pub fn kill(&self) {
        #[cfg(windows)]
        {
            windows_impl::kill_process(self.process_handle);
        }
        #[cfg(not(windows))]
        {
            let _ = std::process::Command::new("kill")
                .args(["-9", &self.pid.to_string()])
                .output();
        }
    }
}

impl Drop for BackendProcess {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            windows_impl::close_handle(self.process_handle);
        }
        // On Unix, the child process continues running after we drop the handle
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Windows — schtasks with InteractiveToken
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use std::ffi::c_void;

    type HANDLE = *mut c_void;
    type DWORD = u32;

    #[link(name = "kernel32")]
    extern "system" {
        fn CloseHandle(handle: HANDLE) -> i32;
        fn GetExitCodeProcess(process: HANDLE, exit_code: *mut DWORD) -> i32;
        fn WaitForSingleObject(handle: HANDLE, milliseconds: DWORD) -> DWORD;
        fn TerminateProcess(process: HANDLE, exit_code: u32) -> i32;
    }

    pub fn is_alive(handle: HANDLE) -> bool {
        unsafe {
            let mut exit_code: DWORD = 0;
            if GetExitCodeProcess(handle, &mut exit_code) != 0 {
                exit_code == 259 // STILL_ACTIVE
            } else {
                false
            }
        }
    }

    pub fn kill_process(handle: HANDLE) {
        unsafe {
            TerminateProcess(handle, 1);
            WaitForSingleObject(handle, 5000);
        }
    }

    pub fn close_handle(handle: HANDLE) {
        if !handle.is_null() {
            unsafe {
                CloseHandle(handle);
            }
        }
    }

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
             set ENABLE_AUDIO=1\r\n\
             set RUST_LOG=info\r\n\
             set STREAMIO_LOG_FILE={log_file}\r\n\
             set GST_PLUGIN_PATH={gst_root}\\lib\\gstreamer-1.0\r\n\
             \"{backend_path}\"\r\n",
        );
        std::fs::write(&script_path, &script)?;

        let task_name = format!("StreamioBackend_{}", &session_id[..8]);

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

        let run = std::process::Command::new(r"C:\Windows\system32\schtasks.exe")
            .args(["/Run", "/TN", &task_name])
            .output()?;

        if !run.status.success() {
            let stderr = String::from_utf8_lossy(&run.stderr);
            return Err(anyhow!("Failed to run backend task: {}", stderr));
        }

        std::thread::sleep(std::time::Duration::from_secs(3));

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
            let log_content = std::fs::read_to_string(&log_file).unwrap_or_default();
            if log_content.is_empty() {
                return Err(anyhow!("Backend failed to start — no log output"));
            }
            info!(
                "Backend log so far: {}",
                log_content
                    .lines()
                    .take(5)
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
        }

        info!(
            "Backend launched via schtasks: pid={}, port={}, display={}",
            pid, port, display_index
        );

        let _ = std::process::Command::new(r"C:\Windows\system32\schtasks.exe")
            .args(["/Delete", "/TN", &task_name, "/F"])
            .output();

        Ok(BackendProcess {
            process_handle: std::ptr::null_mut(),
            pid,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Linux — sudo -u {user} with DISPLAY=:{N}
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(target_os = "linux")]
mod linux_impl {
    use super::*;

    pub fn launch_backend(
        backend_path: &str,
        username: &str,
        _password: &str,
        port: u16,
        display_index: u32,
        session_id: &str,
        token_secret: &str,
        gateway_url: &str,
    ) -> Result<BackendProcess> {
        let log_file = format!("/var/log/streamio/backend-{}.log", session_id);

        info!(
            "Launching backend: user={}, port={}, DISPLAY=:{}, session={}",
            username, port, display_index, session_id
        );

        // Allow the user to connect to the Xvfb display
        let _ = std::process::Command::new("xhost")
            .env("DISPLAY", format!(":{}", display_index))
            .arg(format!("+SI:localuser:{}", username))
            .output();

        let child = std::process::Command::new("sudo")
            .args(["-u", username, "--preserve-env=DISPLAY,PORT,SESSION_ID,BACKEND_TOKEN_SECRET,GATEWAY_URL,RUST_LOG,STREAMIO_LOG_FILE,GST_PLUGIN_PATH,GST_PLUGIN_SYSTEM_PATH"])
            .env("DISPLAY", format!(":{}", display_index))
            .env("PORT", port.to_string())
            .env("SESSION_ID", session_id)
            .env("BACKEND_TOKEN_SECRET", token_secret)
            .env("GATEWAY_URL", gateway_url)
            .env("RUST_LOG", "info")
            .env("STREAMIO_LOG_FILE", &log_file)
            .arg(backend_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| anyhow!("Failed to launch backend: {}", e))?;

        let pid = child.id();
        info!(
            "Backend launched: pid={}, port={}, display=:{}, user={}",
            pid, port, display_index, username
        );

        Ok(BackendProcess {
            child: Some(child),
            pid,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// macOS — sudo -u {user}
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(target_os = "macos")]
mod macos_impl {
    use super::*;

    pub fn launch_backend(
        backend_path: &str,
        username: &str,
        _password: &str,
        port: u16,
        display_index: u32,
        session_id: &str,
        token_secret: &str,
        gateway_url: &str,
    ) -> Result<BackendProcess> {
        let log_file = format!("/var/log/streamio/backend-{}.log", session_id);

        info!(
            "Launching backend: user={}, port={}, display={}, session={}",
            username, port, display_index, session_id
        );

        let child = std::process::Command::new("sudo")
            .args(["-u", username])
            .env("PORT", port.to_string())
            .env("DISPLAY_INDEX", display_index.to_string())
            .env("SESSION_ID", session_id)
            .env("BACKEND_TOKEN_SECRET", token_secret)
            .env("GATEWAY_URL", gateway_url)
            .env("RUST_LOG", "info")
            .env("STREAMIO_LOG_FILE", &log_file)
            .arg(backend_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| anyhow!("Failed to launch backend: {}", e))?;

        let pid = child.id();
        info!(
            "Backend launched: pid={}, port={}, display={}, user={}",
            pid, port, display_index, username
        );

        Ok(BackendProcess {
            child: Some(child),
            pid,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════════

pub fn launch_backend(
    backend_path: &str,
    username: &str,
    password: &str,
    port: u16,
    display_index: u32,
    session_id: &str,
    token_secret: &str,
    gateway_url: &str,
) -> Result<BackendProcess> {
    #[cfg(windows)]
    {
        windows_impl::launch_backend(
            backend_path,
            username,
            password,
            port,
            display_index,
            session_id,
            token_secret,
            gateway_url,
        )
    }
    #[cfg(target_os = "linux")]
    {
        linux_impl::launch_backend(
            backend_path,
            username,
            password,
            port,
            display_index,
            session_id,
            token_secret,
            gateway_url,
        )
    }
    #[cfg(target_os = "macos")]
    {
        macos_impl::launch_backend(
            backend_path,
            username,
            password,
            port,
            display_index,
            session_id,
            token_secret,
            gateway_url,
        )
    }
}
