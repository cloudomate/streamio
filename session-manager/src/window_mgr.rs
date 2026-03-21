//! Window confinement manager.
//!
//! Uses SetWinEventHook to monitor window creation and movement,
//! snapping each user's windows back to their assigned display region.
//!
//! TODO: Implement after core session lifecycle is working.
//! For now, this is a stub that compiles but does nothing.

use tracing::info;

/// Start monitoring window events and confining them to display regions.
/// This should be called once on startup.
pub fn start() {
    info!("Window manager: not yet implemented (stub)");
}
