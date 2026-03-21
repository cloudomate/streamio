//! Virtual display management via the IddCx driver.
//!
//! Wraps the `display-ctl` CLI for creating/destroying virtual displays
//! and queries `EnumDisplayMonitors` for display rectangles.

use anyhow::{anyhow, Result};
use std::ffi::c_void;
use tracing::{info, warn};

type BOOL = i32;
type HDC = *mut c_void;
type HMONITOR = *mut c_void;
type LPARAM = isize;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct RECT {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

type MONITORENUMPROC =
    unsafe extern "system" fn(HMONITOR, HDC, *mut RECT, LPARAM) -> BOOL;

#[link(name = "user32")]
extern "system" {
    fn EnumDisplayMonitors(
        hdc: HDC,
        lprc_clip: *const RECT,
        lpfn_enum: MONITORENUMPROC,
        data: LPARAM,
    ) -> BOOL;
}

/// Information about a display in the virtual desktop.
#[derive(Debug, Clone)]
pub struct DisplayRect {
    pub index: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Enumerate all monitors and return their rectangles.
pub fn enumerate_displays() -> Vec<DisplayRect> {
    use std::sync::Mutex;
    static RECTS: Mutex<Vec<RECT>> = Mutex::new(Vec::new());

    unsafe extern "system" fn callback(
        _hmon: HMONITOR,
        _hdc: HDC,
        rc: *mut RECT,
        _data: LPARAM,
    ) -> BOOL {
        if let Ok(mut rects) = RECTS.lock() {
            rects.push(*rc);
        }
        1
    }

    {
        let mut rects = RECTS.lock().unwrap();
        rects.clear();
    }

    unsafe {
        EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), callback, 0);
    }

    let rects = RECTS.lock().unwrap();
    rects
        .iter()
        .enumerate()
        .map(|(i, r)| DisplayRect {
            index: i as u32,
            x: r.left,
            y: r.top,
            width: (r.right - r.left) as u32,
            height: (r.bottom - r.top) as u32,
        })
        .collect()
}

/// Get the rectangle of a specific display by index.
pub fn get_display_rect(display_index: u32) -> Option<DisplayRect> {
    enumerate_displays()
        .into_iter()
        .find(|d| d.index == display_index)
}

/// Create a virtual display via display-ctl.
/// Returns the display ID assigned by the driver.
pub fn create_display(
    display_ctl_path: &str,
    width: u32,
    height: u32,
    refresh_hz: u32,
) -> Result<u32> {
    let output = std::process::Command::new(display_ctl_path)
        .args(["create", &width.to_string(), &height.to_string(), &refresh_hz.to_string()])
        .output()
        .map_err(|e| anyhow!("Failed to run display-ctl: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        return Err(anyhow!(
            "display-ctl create failed (exit {}): {}",
            output.status,
            stderr
        ));
    }

    // Parse "Created virtual display #N (...)"
    let id = stdout
        .split('#')
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| anyhow!("Failed to parse display ID from: {}", stdout))?;

    info!("Created virtual display #{} ({}x{}@{}Hz)", id, width, height, refresh_hz);
    Ok(id)
}

/// Destroy a virtual display via display-ctl.
pub fn destroy_display(display_ctl_path: &str, display_id: u32) -> Result<()> {
    let output = std::process::Command::new(display_ctl_path)
        .args(["destroy", &display_id.to_string()])
        .output()
        .map_err(|e| anyhow!("Failed to run display-ctl: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("display-ctl destroy failed: {}", stderr));
    }

    info!("Destroyed virtual display #{}", display_id);
    Ok(())
}

/// Extend the desktop to show new virtual displays.
/// Must run in the interactive session (Session 1) via scheduled task.
pub fn extend_desktop() -> Result<()> {
    // Create a scheduled task to run DisplaySwitch /extend in the interactive session
    let task_xml = r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT30S</ExecutionTimeLimit>
  </Settings>
  <Triggers />
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Actions>
    <Exec>
      <Command>C:\Windows\system32\DisplaySwitch.exe</Command>
      <Arguments>/extend</Arguments>
    </Exec>
  </Actions>
</Task>"#;

    let task_path = r"C:\ProgramData\Streamio\extend-task.xml";
    let _ = std::fs::create_dir_all(r"C:\ProgramData\Streamio");
    std::fs::write(task_path, task_xml)?;

    // Register and run the task
    let register = std::process::Command::new(r"C:\Windows\system32\schtasks.exe")
        .args(["/Create", "/TN", "StreamioExtendDisplay", "/XML", task_path, "/F"])
        .output()?;

    if !register.status.success() {
        warn!(
            "schtasks create: {}",
            String::from_utf8_lossy(&register.stderr)
        );
    }

    let run = std::process::Command::new(r"C:\Windows\system32\schtasks.exe")
        .args(["/Run", "/TN", "StreamioExtendDisplay"])
        .output()?;

    if !run.status.success() {
        return Err(anyhow!(
            "Failed to extend desktop: {}",
            String::from_utf8_lossy(&run.stderr)
        ));
    }

    // Wait for DisplaySwitch to take effect
    std::thread::sleep(std::time::Duration::from_secs(2));
    info!("Desktop extended to include virtual displays");
    Ok(())
}
