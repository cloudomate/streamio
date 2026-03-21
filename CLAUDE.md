# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Streamio — a Rust-based multi-session VDI system. Multiple users connect via browser to a single Windows host, each getting an isolated virtual display, input routing, and user account — all without RDS/RDP licensing. Streams screens via WebRTC using GStreamer with hardware H.264 encoding. Supports bidirectional audio and remote mouse/keyboard input injection.

The `vdi-gateway` branch is the primary development branch with OIDC auth, per-user desktop assignment, KubeVirt VM provisioning, session manager, virtual display driver, and VHID input driver.

## Repository Structure (Cargo Workspace)

```
Cargo.toml              # workspace root [members = backend, gateway, types, service, display-ctl, session-manager]
types/                  # shared types crate (SessionClaims, InputEvent, SignalingMessage, etc.)
backend/                # screen capture + WebRTC streaming process (one per user)
gateway/                # auth gateway, routing, admin panel, KubeVirt provisioner
session-manager/        # Windows service — orchestrates displays, accounts, backends, input routing
service/                # legacy input helper (streamio-service.exe) — being replaced by session-manager
display-ctl/            # CLI tool to create/remove virtual displays via IddCx driver
driver/
  vhid/                 # KMDF VHF virtual HID driver (keyboard injection)
  display/              # IddCx UMDF2 virtual display driver
client/
  screen.html           # browser streaming UI (embedded by backend at compile time)
  admin.html            # admin panel UI (embedded by gateway at compile time)
gateway/migrations/     # PostgreSQL schema (001_init.sql, 002_vm_columns.sql)
deploy/                 # Kubernetes/KubeVirt RBAC and base image manifests
docker-compose.yml      # local dev: gateway + backend + postgres + redis
```

## Build Commands

```bash
# Build entire workspace
cargo build --release --workspace

# Build individual crates
cargo build -p streamio                    # backend
cargo build -p streamio-gateway            # gateway
cargo build -p streamio-session-manager    # session manager
cargo build -p streamio-service            # legacy input helper
cargo build -p display-ctl                 # virtual display CLI

# Type-check without building
cargo check --workspace

# Run backend only (dev mode, no auth)
cargo run -p streamio

# Run with debug logging
RUST_LOG=debug cargo run -p streamio-gateway

# Create self-contained distribution bundle (backend only, macOS/Linux)
./bundle.sh

# Local dev with all services
docker compose up
```

There are no tests in this project currently.

## Releasing

Releases are automated via GitHub Actions (`.github/workflows/release.yml`). Push a `v*` tag to trigger builds for macOS and Linux, which run `bundle.sh` and upload archives to a GitHub Release. macOS builds require Apple Developer ID certificate secrets to be configured in the repo for code signing and notarization.

## Environment Variables

### Backend (`streamio`)
- `PORT` — HTTP server port (default: 8123)
- `FPS` — Capture framerate (default: 30)
- `DISPLAY_INDEX` — Virtual display index to capture (default: 0)
- `SESSION_ID` — Session identifier, used for per-session input pipe name
- `ENABLE_AUDIO` — Set to `1` to enable audio capture
- `RUST_LOG` — Log level (default: `info`)
- `BACKEND_TOKEN_SECRET` — Shared with gateway JWT secret; if empty, auth is bypassed (dev mode)
- `GATEWAY_ORIGIN` — Restrict CORS to this origin; if unset, `CorsLayer::permissive()` (dev mode)
- `GATEWAY_URL` — Gateway base URL for self-registration on startup
- `BACKEND_ID` — UUID identifying this backend instance in the registry
- `STREAMIO_LOG_FILE` — Path to write file logs (used when launched by session manager)

### Gateway (`streamio-gateway`)
- `GATEWAY_PORT` — HTTP server port (default: 8080)
- `GATEWAY_ORIGIN` — Own public URL (for CORS and cookie domain)
- `JWT_SECRET` — Shared secret for signing internal JWTs (required)
- `OIDC_ISSUER_URL` — OIDC provider discovery URL (e.g. `https://accounts.google.com`)
- `OIDC_CLIENT_ID` / `OIDC_CLIENT_SECRET` / `OIDC_REDIRECT_URI`
- `DATABASE_URL` — PostgreSQL connection string
- `REDIS_URL` — Redis connection string (default: `redis://127.0.0.1/`)
- `ADMIN_SUBS` — Comma-separated OIDC subject IDs with admin role
- `KUBEVIRT_ENABLED` — Set to `true` to enable KubeVirt VM provisioning
- `KUBEVIRT_NAMESPACE` — Kubernetes namespace for VMs (default: `vdi`)
- `KUBEVIRT_GATEWAY_URL` — Gateway URL injected into VMs via cloud-init
- `DEFAULT_BASE_PVC` — Base PVC name for DataVolume cloning
- `DEFAULT_DISK_SIZE` / `DEFAULT_VM_MEMORY` / `DEFAULT_VM_CPU` — VM sizing defaults

### Session Manager (`streamio-session-manager`)
- `SESSION_MANAGER_PORT` — REST API port (default: 9100)
- `BACKEND_PATH` — Path to backend executable (default: `C:\Program Files\Streamio\streamio.exe`)
- `BACKEND_TOKEN_SECRET` — Token secret passed to launched backends
- `GATEWAY_URL` — Gateway URL passed to launched backends

## Architecture

### Multi-Session VDI Architecture

```
Browser A ──► Gateway ──► Backend A (port 9001, display=1) ◄── Session Manager
Browser B ──► Gateway ──► Backend B (port 9002, display=2) ◄── Session Manager
                                                                     │
                                    ┌────────────────────────────────┘
                                    │
                              Session Manager
                              (SYSTEM service)
                                    │
                    ┌───────────────┼───────────────┐
                    │               │               │
              display.rs      launcher.rs    input_router.rs
              (IddCx)        (schtasks)     (SendInput + VHID)
```

- **Gateway** authenticates users via OIDC PKCE flow, issues internal JWTs, proxies WebSocket to the user's assigned backend, and hosts the admin panel. Optionally provisions KubeVirt VMs.
- **Session Manager** (Windows service, SYSTEM) orchestrates per-user resources: creates virtual displays via display-ctl, creates local Windows accounts, launches backend processes via schtasks, and routes input per-session.
- **Backend** verifies `X-Session-Token` header on `/ws` upgrade, starts a GStreamer screen-capture pipeline per connection, and reports health via `/healthz`. One instance per user per display.
- **Input flow**: Browser → WebSocket → Backend → named pipe (`\\.\pipe\streamio-input-<session_id>`) → Session Manager → SendInput (mouse) / VHID DeviceIoControl (keyboard).
- **PostgreSQL** stores backend registry and user→backend assignments.
- **Redis** holds PKCE verifiers + nonces (10-minute TTL) during login flow.

### Gateway Source Layout

- `gateway/src/auth.rs` — OIDC login/callback/logout; stores PKCE verifier+nonce in Redis; issues JWT cookie `sid`
- `gateway/src/session.rs` — JWT issue/verify with `SessionClaims` (sub, email, role, backend_id, exp)
- `gateway/src/middleware.rs` — `RequireSession` and `RequireAdmin` axum extractors
- `gateway/src/registry.rs` — Backend pool (PostgreSQL), health polling every 30s, `get_or_assign()`
- `gateway/src/proxy.rs` — WebSocket splice: client ↔ `tokio_tungstenite` ↔ backend
- `gateway/src/admin.rs` — Admin REST API + serves `client/admin.html`
- `gateway/src/provisioner.rs` — KubeVirt VM provisioner (DataVolume cloning, cloud-init, lifecycle management)
- `gateway/src/main.rs` — Router, DB/Redis init, runs migrations

### Session Manager Source Layout

- `session-manager/src/main.rs` — Service entry, HTTP API server, session lifecycle
- `session-manager/src/display.rs` — Creates/destroys virtual displays via display-ctl CLI
- `session-manager/src/accounts.rs` — Creates/manages local Windows accounts per VDI user
- `session-manager/src/launcher.rs` — Launches backend processes via schtasks (interactive desktop)
- `session-manager/src/window_mgr.rs` — Window confinement via SetWinEventHook (stub)
- `session-manager/src/input_router.rs` — Per-session named pipes, SendInput mouse injection, VHID keyboard injection
- `session-manager/src/api.rs` — REST API handlers for gateway (`POST/DELETE/GET /api/sessions`)

### Backend Source Layout

- `backend/src/main.rs` — GStreamer init, plugin validation, env var parsing
- `backend/src/screen_capture.rs` — GStreamer pipeline (d3d11screencapturesrc → H.264 → webrtcbin), DXGI handover
- `backend/src/screen_server.rs` — Axum HTTP/WS server, pipeline lifecycle, input forwarding
- `backend/src/input.rs` — Input event handling (enigo for direct injection, named pipe for session manager)

### Drivers

- **VHID driver** (`driver/vhid/`): KMDF driver using Windows VHF (Virtual HID Framework). Creates a virtual HID device with keyboard + absolute mouse descriptors. User-mode code sends HID reports via DeviceIoControl. Currently used for keyboard injection only — mouse via SendInput is more reliable.
- **Virtual display driver** (`driver/display/`): IddCx UMDF2 indirect display driver. Creates virtual monitors on demand via `display-ctl create <width> <height> <refresh>`. Each virtual display appears as a separate HMONITOR in the Windows virtual desktop.
- **display-ctl** (`display-ctl/`): Rust CLI tool that communicates with the IddCx driver to create/remove/list virtual displays.

### Key Patterns

- **Bundled GStreamer**: At startup, checks for `lib/gstreamer-1.0/` next to the executable. If found, sets `GST_PLUGIN_PATH`, `GST_PLUGIN_SYSTEM_PATH=""`, and `GST_PLUGIN_SCANNER` before `gstreamer::init()`.
- **WebSocket signaling**: Clients connect via `/ws`, exchange SDP offer/answer and ICE candidates as JSON. Input events share the same WebSocket.
- **HTML embedding**: `client/screen.html` and `client/admin.html` are embedded at compile time via `include_str!()`.
- **Platform-conditional pipelines**: Screen capture source and hardware encoder use `#[cfg(target_os)]` — different GStreamer element names per platform.
- **Plugin validation**: On startup, `main.rs` verifies required GStreamer plugins (webrtc, nice, dtls, srtp, rtp, videoconvertscale) are available.
- **Dev mode (no auth)**: When `BACKEND_TOKEN_SECRET` is empty, the backend skips token verification. Gateway uses `CorsLayer::permissive()` when `GATEWAY_ORIGIN` is unset.
- **DXGI handover**: When a WebSocket disconnects, the backend preserves its GStreamer pipeline in `prev_streamer` so the DXGI output handle stays alive for the next connection. The new pipeline is created while the old one is still running, then the old one is dropped. This is needed because d3d11screencapturesrc's DXGI output for virtual displays disappears once the pipeline that opened it is destroyed.
- **schtasks backend launch**: Session manager launches backends via `schtasks.exe` with `/IT` (interactive token) instead of `CreateProcessWithLogonW`, because the latter creates a restricted logon session where DXGI enumeration of virtual displays fails.
- **Per-session input pipes**: Each backend connects to `\\.\pipe\streamio-input-<session_id>`. The session manager reads events from the pipe, translates coordinates to the session's display region, and injects via SendInput (mouse) or VHID (keyboard).

### Distribution

`./bundle.sh` creates a `dist/` directory containing the binary, `lib/` (GStreamer core libs + transitive deps), `lib/gstreamer-1.0/` (plugins), and `libexec/gst-plugin-scanner`.

On macOS, dylib paths are rewritten to `@executable_path/lib/...` via `install_name_tool`. On Linux, rpaths are set via `patchelf` (requires `sudo apt install patchelf` before running `bundle.sh`). On Windows, DLLs go next to the exe.

Note: Distributing with the bundled x264 software encoder triggers GPL-2.0 obligations — see `COPYING`.

### Dependencies

**Backend**: GStreamer 0.23 bindings, Axum 0.7, Enigo 0.2, Tokio 1, `jsonwebtoken 9`, `socket2`, `streamio-types`

**Gateway**: Axum 0.7, `openidconnect 3` (OIDC PKCE), `jsonwebtoken 9`, `sqlx 0.8` + PostgreSQL, `redis 0.27`, `tokio-tungstenite 0.24` (WS proxy), `reqwest 0.12`, `kube` (KubeVirt)

**Session Manager**: Axum 0.7, Tokio 1, `serde_json`, `uuid`, `anyhow`, `streamio-types`

**Shared (`streamio-types`)**: `serde`, `uuid`

For development (backend only): `brew install gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly pkgconf` (macOS).

## Known Issues / Active Work

- **DXGI output loss on reconnection**: d3d11screencapturesrc can't re-enumerate virtual display HMONITOR after the pipeline that opened it is destroyed. DXGI handover mitigates this but is unreliable after many reconnections. Needs persistent capture pipeline architecture.
- **Mouse clicks not registering**: SendInput succeeds (no errors) but desktop interactions don't respond. Mouse movement works correctly. Under investigation.
- **Orphan virtual displays**: Display driver reset doesn't always clean up all virtual displays, causing 3-monitor topology instead of 2.
- **VHID mouse doesn't work**: VHID DeviceIoControl succeeds but cursor doesn't move. Mouse injection uses SendInput instead. VHID is still used for keyboard only.
- **Keyboard injection**: Uses VHID — not yet tested end-to-end in multi-session setup.

## Phase Roadmap

- **Phase A**: VHID driver — ✅ Done
- **Phase B**: Virtual display driver + per-display capture — ✅ Done
- **Phase C**: Multi-session (session manager, input routing, account management) — 🔶 In progress
- **Phase 1**: Auth gateway + single-user MVP — ✅ Done
- **Phase 2**: Per-user desktop assignment — ✅ Done
- **Phase 3**: Session sharing — GStreamer `tee` element + multiple `webrtcbin` peers per pipeline — Not started
- **Phase 4**: Admin panel full features + KubeVirt dynamic provisioning — ✅ Provisioner done, admin panel partially done
