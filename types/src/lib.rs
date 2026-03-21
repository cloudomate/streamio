use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Roles ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Observer,
    Admin,
}

// ── Internal JWT claims (gateway → backend) ──────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionClaims {
    /// OIDC subject (unique user ID)
    pub sub: String,
    pub email: String,
    pub role: Role,
    /// Which backend this user is assigned to
    pub backend_id: Option<Uuid>,
    /// Expiry (Unix timestamp seconds)
    pub exp: i64,
}

// ── Backend registry ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendInfo {
    pub id: Uuid,
    /// Base URL reachable from gateway, e.g. "http://192.168.1.10:9001"
    pub url: String,
    pub label: Option<String>,
    pub healthy: bool,
}

/// Sent by backend on startup to self-register with the gateway.
#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub id: Uuid,
    pub url: String,
    pub label: Option<String>,
}

// ── WebRTC signaling ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalingMessage {
    Offer { sdp: String },
    Answer { sdp: String },
    Ice {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_m_line_index: Option<u32>,
    },
}

// ── Input events ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputEvent {
    MouseDown { button: u8, x: i32, y: i32 },
    MouseUp { button: u8, x: i32, y: i32 },
    MouseMove { x: i32, y: i32 },
    Scroll { dx: f64, dy: f64 },
    KeyDown { key: String, code: String, modifiers: Modifiers },
    KeyUp { key: String, code: String, modifiers: Modifiers },
}

// ── Admin API payloads ───────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct AssignRequest {
    pub user_sub: String,
    pub backend_id: Uuid,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ShadowRequest {
    pub user_sub: String,
    pub role: Role,
}

// ── Session Manager types ───────────────────────────────────────────────────

/// Request to create a new VDI session for a user.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionRequest {
    pub user_id: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
}

/// Response from session creation.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionResponse {
    pub session_id: String,
    pub backend_port: u16,
    pub display_index: u32,
    /// OS-level username for the session (e.g., streamio_a1b2c3d4).
    /// Aliased from `windows_user` for backward compatibility.
    #[serde(alias = "windows_user")]
    pub os_user: String,
}

/// Status of an active session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub user_id: String,
    /// OS-level username for the session.
    /// Aliased from `windows_user` for backward compatibility.
    #[serde(alias = "windows_user")]
    pub os_user: String,
    pub display_index: u32,
    pub display_rect: (i32, i32, u32, u32),
    pub backend_port: u16,
    pub backend_pid: Option<u32>,
    pub created_at: u64,
}

/// Host platform type for session manager.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPlatform {
    Windows,
    Linux,
    MacOs,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserAssignment {
    pub user_sub: String,
    pub email: Option<String>,
    pub backend_id: Option<Uuid>,
    pub backend_label: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BackendStatus {
    pub info: BackendInfo,
    pub active_sessions: u32,
}

// ── Host management (multi-machine VDI fleet) ───────────────────────────────

/// A host machine running the session manager agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInfo {
    pub id: Uuid,
    /// Session manager API URL, e.g. "http://192.168.1.10:9100"
    pub url: String,
    pub label: Option<String>,
    pub platform: HostPlatform,
    pub healthy: bool,
    pub max_sessions: u32,
    pub active_sessions: u32,
}

/// Request from session manager agent to register with gateway.
#[derive(Debug, Serialize, Deserialize)]
pub struct HostRegisterRequest {
    pub id: Uuid,
    pub url: String,
    pub label: Option<String>,
    pub platform: HostPlatform,
    pub max_sessions: u32,
}

/// User's view of their assigned VDI(s).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserVdi {
    pub host_id: Uuid,
    pub host_label: Option<String>,
    pub platform: String,
    /// Active session on this host (if any)
    pub session: Option<UserVdiSession>,
}

/// An active VDI session from the user's perspective.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserVdiSession {
    pub session_id: String,
    pub backend_port: u16,
    /// Direct URL to the backend WebRTC stream
    pub stream_url: String,
    pub status: String,
}

/// Admin request to assign a user to a host.
#[derive(Debug, Serialize, Deserialize)]
pub struct UserHostAssignment {
    pub user_sub: String,
    pub host_id: Uuid,
    pub priority: Option<i32>,
}

/// Admin view of a VDI session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VdiSessionInfo {
    pub id: String,
    pub user_sub: String,
    pub user_email: Option<String>,
    pub host_id: Uuid,
    pub host_label: Option<String>,
    pub backend_port: u16,
    pub os_user: Option<String>,
    pub status: String,
    pub created_at: String,
}
