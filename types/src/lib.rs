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

// ── VM provisioning ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OsType {
    Windows11,
    Ubuntu,
    Alpine,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmType {
    Kubevirt,
    External,
}

/// Admin API request body for provisioning a new VM per user.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProvisionRequest {
    pub user_sub: String,
    pub os_type: OsType,
    /// Name of the base image PVC to clone, e.g. "ubuntu-22.04-base"
    pub base_pvc: String,
    /// e.g. "60Gi"
    pub disk_size: String,
    /// e.g. "4Gi"
    pub memory: String,
    pub cpu_cores: u32,
    pub label: Option<String>,
}

/// VM metadata stored alongside a backend record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmInfo {
    pub vm_type: VmType,
    pub vm_name: String,
    pub vm_ns: String,
    pub os_type: OsType,
    pub disk_pvc: String,
    /// Power state: "stopped" | "starting" | "running" | "stopping" | "provisioning"
    pub state: String,
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
