//! Local Windows account management for VDI users.
//!
//! Creates isolated local accounts per VDI user so each user gets
//! their own profile directory, file isolation, and process identity.

use anyhow::{anyhow, Result};
use tracing::{info, warn};

/// Info about a local Windows account created for a VDI user.
#[derive(Debug, Clone)]
pub struct WindowsAccount {
    pub username: String,
    pub password: String,
}

/// Derive a local Windows username from an OIDC subject ID.
/// Format: `streamio_<first 8 chars of hex hash>`
fn derive_username(user_id: &str) -> String {
    // Simple hash — we just need uniqueness, not crypto
    let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
    for b in user_id.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3); // FNV-1a prime
    }
    format!("streamio_{:08x}", hash as u32)
}

/// Generate a random password for the account.
/// Must be <= 14 chars to avoid net.exe interactive prompt.
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

/// Create or get an existing local Windows account for a VDI user.
pub fn create_or_get_account(user_id: &str) -> Result<WindowsAccount> {
    let username = derive_username(user_id);
    let password = generate_password();

    // Check if user already exists
    let check = std::process::Command::new(r"C:\Windows\system32\net.exe")
        .args(["user", &username])
        .output()?;

    if check.status.success() {
        // User exists — reset password so we know it
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
        // Create new user
        info!("Creating local account: {}", username);
        let create = std::process::Command::new(r"C:\Windows\system32\net.exe")
            .args(["user", &username, &password, "/add"])
            .output()?;

        if !create.status.success() {
            let stdout = String::from_utf8_lossy(&create.stdout);
            let stderr = String::from_utf8_lossy(&create.stderr);
            return Err(anyhow!(
                "Failed to create account {}: stdout=[{}] stderr=[{}]",
                username, stdout.trim(), stderr.trim()
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

    Ok(WindowsAccount { username, password })
}

/// Delete a local Windows account and its profile.
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

    // Clean up user profile directory
    let profile_path = format!(r"C:\Users\{}", username);
    if std::path::Path::new(&profile_path).exists() {
        info!("Removing profile directory: {}", profile_path);
        let _ = std::fs::remove_dir_all(&profile_path);
    }

    info!("Deleted account: {}", username);
    Ok(())
}
