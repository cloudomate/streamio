//! OS-level account management for VDI users.
//!
//! Creates isolated local accounts per VDI user so each user gets
//! their own profile directory, file isolation, and process identity.
//!
//! - **Windows**: `net.exe user` commands
//! - **Linux**: `useradd`/`userdel` + `chpasswd`
//! - **macOS**: `sysadminctl` / `dscl`

use anyhow::{anyhow, Result};
use tracing::{info, warn};

/// Info about a local OS account created for a VDI user.
#[derive(Debug, Clone)]
pub struct OsAccount {
    pub username: String,
    pub password: String,
}

// ── Shared helpers (all platforms) ──────────────────────────────────────────

/// Derive a local username from an OIDC subject ID.
/// Format: `streamio_<first 8 chars of hex hash>`
fn derive_username(user_id: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
    for b in user_id.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3); // FNV-1a prime
    }
    format!("streamio_{:08x}", hash as u32)
}

/// Generate a random password for the account.
fn generate_password() -> String {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mixed = seed ^ (std::process::id() as u128);
    // 14 chars: "S3!" prefix + 8 hex + "xZ" suffix = meets complexity
    format!("S3!{:08x}xZ", mixed as u32)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Windows — net.exe user
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(windows)]
mod windows_impl {
    use super::*;

    pub fn create_or_get_account(user_id: &str) -> Result<OsAccount> {
        let username = derive_username(user_id);
        let password = generate_password();

        let check = std::process::Command::new(r"C:\Windows\system32\net.exe")
            .args(["user", &username])
            .output()?;

        if check.status.success() {
            info!("Account {} already exists, resetting password", username);
            let reset = std::process::Command::new(r"C:\Windows\system32\net.exe")
                .args(["user", &username, &password])
                .output()?;
            if !reset.status.success() {
                return Err(anyhow!(
                    "Failed to reset password for {}: {}",
                    username,
                    String::from_utf8_lossy(&reset.stderr)
                ));
            }
        } else {
            info!("Creating local account: {}", username);
            let create = std::process::Command::new(r"C:\Windows\system32\net.exe")
                .args(["user", &username, &password, "/add"])
                .output()?;

            if !create.status.success() {
                let stdout = String::from_utf8_lossy(&create.stdout);
                let stderr = String::from_utf8_lossy(&create.stderr);
                return Err(anyhow!(
                    "Failed to create account {}: stdout=[{}] stderr=[{}]",
                    username,
                    stdout.trim(),
                    stderr.trim()
                ));
            }

            // Set password to never expire
            let _ = std::process::Command::new(r"C:\Windows\system32\wbem\WMIC.exe")
                .args([
                    "useraccount",
                    "where",
                    &format!("name='{}'", username),
                    "set",
                    "PasswordExpires=false",
                ])
                .output();

            info!("Created local account: {}", username);
        }

        Ok(OsAccount { username, password })
    }

    pub fn delete_account(username: &str) -> Result<()> {
        info!("Deleting account: {}", username);

        let output = std::process::Command::new(r"C:\Windows\system32\net.exe")
            .args(["user", username, "/delete"])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("not found") {
                warn!("Account {} not found (already deleted?)", username);
                return Ok(());
            }
            return Err(anyhow!("Failed to delete account {}: {}", username, stderr));
        }

        let profile_path = format!(r"C:\Users\{}", username);
        if std::path::Path::new(&profile_path).exists() {
            info!("Removing profile directory: {}", profile_path);
            let _ = std::fs::remove_dir_all(&profile_path);
        }

        info!("Deleted account: {}", username);
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Linux — useradd / userdel / chpasswd
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(target_os = "linux")]
mod linux_impl {
    use super::*;

    pub fn create_or_get_account(user_id: &str) -> Result<OsAccount> {
        let username = derive_username(user_id);
        let password = generate_password();

        // Check if user exists
        let check = std::process::Command::new("id")
            .arg(&username)
            .output()?;

        if check.status.success() {
            info!("Account {} already exists, resetting password", username);
        } else {
            info!("Creating local account: {}", username);
            let create = std::process::Command::new("useradd")
                .args([
                    "-m",          // create home directory
                    "-s", "/bin/bash",
                    "-G", "audio,video", // groups needed for display/audio
                    &username,
                ])
                .output()?;

            if !create.status.success() {
                let stderr = String::from_utf8_lossy(&create.stderr);
                return Err(anyhow!(
                    "Failed to create account {}: {}",
                    username,
                    stderr.trim()
                ));
            }
            info!("Created local account: {}", username);
        }

        // Set password via chpasswd (works for both new and existing users)
        let chpasswd = std::process::Command::new("chpasswd")
            .stdin(std::process::Stdio::piped())
            .spawn();

        if let Ok(mut child) = chpasswd {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                let _ = writeln!(stdin, "{}:{}", username, password);
            }
            let _ = child.wait();
        }

        Ok(OsAccount { username, password })
    }

    pub fn delete_account(username: &str) -> Result<()> {
        info!("Deleting account: {}", username);

        let output = std::process::Command::new("userdel")
            .args(["-r", username]) // -r removes home directory
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("does not exist") {
                warn!("Account {} not found (already deleted?)", username);
                return Ok(());
            }
            return Err(anyhow!("Failed to delete account {}: {}", username, stderr));
        }

        info!("Deleted account: {}", username);
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// macOS — sysadminctl / dscl
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(target_os = "macos")]
mod macos_impl {
    use super::*;

    pub fn create_or_get_account(user_id: &str) -> Result<OsAccount> {
        // On macOS, creating users requires root (dscl).
        // If not running as root, just use the current user for dev/testing.
        let euid = unsafe { libc::geteuid() };
        if euid != 0 {
            let current_user = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
            info!("Not running as root — using current user '{}' for session", current_user);
            return Ok(OsAccount {
                username: current_user,
                password: String::new(),
            });
        }

        let username = derive_username(user_id);
        let password = generate_password();

        // Check if user exists via dscl
        let check = std::process::Command::new("dscl")
            .args([".", "-read", &format!("/Users/{}", username)])
            .output()?;

        if check.status.success() {
            info!("Account {} already exists, resetting password", username);
            let reset = std::process::Command::new("dscl")
                .args([
                    ".",
                    "-passwd",
                    &format!("/Users/{}", username),
                    &password,
                ])
                .output()?;
            if !reset.status.success() {
                warn!("Failed to reset password for {} via dscl", username);
            }
        } else {
            info!("Creating local account: {}", username);

            // Find next available UID (start at 600 for service accounts)
            let uid = find_next_uid()?;

            // Create user via dscl
            let user_path = format!("/Users/{}", username);
            let real_name = format!("Streamio VDI {}", username);
            let uid_str = uid.to_string();
            let home_dir = format!("/Users/{}", username);
            let commands: Vec<Vec<&str>> = vec![
                vec![".", "-create", &user_path],
                vec![".", "-create", &user_path, "UserShell", "/bin/bash"],
                vec![".", "-create", &user_path, "RealName", &real_name],
                vec![".", "-create", &user_path, "UniqueID", &uid_str],
                vec![".", "-create", &user_path, "PrimaryGroupID", "20"],
                vec![".", "-create", &user_path, "NFSHomeDirectory", &home_dir],
                vec![".", "-passwd", &user_path, &password],
            ];

            for args in &commands {
                let output = std::process::Command::new("dscl")
                    .args(args)
                    .output()?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(anyhow!(
                        "dscl {} failed: {}",
                        args.join(" "),
                        stderr.trim()
                    ));
                }
            }

            // Create home directory
            let _ = std::process::Command::new("createhomedir")
                .args(["-c", "-u", &username])
                .output();

            info!("Created local account: {} (uid={})", username, uid);
        }

        Ok(OsAccount { username, password })
    }

    fn find_next_uid() -> Result<u32> {
        let output = std::process::Command::new("dscl")
            .args([".", "-list", "/Users", "UniqueID"])
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let max_uid = stdout
            .lines()
            .filter_map(|line| line.split_whitespace().last())
            .filter_map(|s| s.parse::<u32>().ok())
            .filter(|&uid| uid >= 600 && uid < 65534)
            .max()
            .unwrap_or(599);
        Ok(max_uid + 1)
    }

    pub fn delete_account(username: &str) -> Result<()> {
        info!("Deleting account: {}", username);

        let output = std::process::Command::new("dscl")
            .args([".", "-delete", &format!("/Users/{}", username)])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to delete account {} via dscl: {}", username, stderr);
        }

        let profile_path = format!("/Users/{}", username);
        if std::path::Path::new(&profile_path).exists() {
            info!("Removing profile directory: {}", profile_path);
            let _ = std::fs::remove_dir_all(&profile_path);
        }

        info!("Deleted account: {}", username);
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════════

pub fn create_or_get_account(user_id: &str) -> Result<OsAccount> {
    #[cfg(windows)]
    { windows_impl::create_or_get_account(user_id) }
    #[cfg(target_os = "linux")]
    { linux_impl::create_or_get_account(user_id) }
    #[cfg(target_os = "macos")]
    { macos_impl::create_or_get_account(user_id) }
}

#[allow(dead_code)]
pub fn delete_account(username: &str) -> Result<()> {
    #[cfg(windows)]
    { windows_impl::delete_account(username) }
    #[cfg(target_os = "linux")]
    { linux_impl::delete_account(username) }
    #[cfg(target_os = "macos")]
    { macos_impl::delete_account(username) }
}
