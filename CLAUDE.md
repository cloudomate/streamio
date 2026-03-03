# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Streamio — a Rust-based VDI/remote desktop streaming application. Captures screens and streams them to browsers via WebRTC using GStreamer. Supports bidirectional audio and remote mouse/keyboard input injection. Ships as a self-contained bundle with GStreamer libs included.

The `vdi-gateway` branch adds a full gateway service with OAuth2/OIDC authentication, per-user desktop assignment, session sharing, and an admin panel.

## Repository Structure (Cargo Workspace)

```
Cargo.toml          # workspace root [members = backend, gateway, types]
types/              # shared types crate (SessionClaims, InputEvent, SignalingMessage, etc.)
backend/            # renamed from root src/ — screen capture + WebRTC streaming process
gateway/            # new — auth gateway, routing, admin panel
client/
  screen.html       # browser streaming UI (embedded by backend and gateway at compile time)
  admin.html        # admin panel UI (embedded by gateway at compile time)
gateway/migrations/ # PostgreSQL schema (001_init.sql)
docker-compose.yml  # local dev: gateway + backend-1 + postgres + redis
```

## Build Commands

```bash
# Build entire workspace
cargo build --release --workspace

# Build individual crate
cargo build -p streamio
cargo build -p streamio-gateway

# Type-check without building
cargo check --workspace

# Run backend only (dev mode, no auth)
cargo run -p streamio

# Run with debug logging
RUST_LOG=debug cargo run -p streamio-gateway

# Create self-contained distribution bundle (backend only)
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
- `DISPLAY_INDEX` — macOS display to capture (default: 0)
- `ENABLE_AUDIO` — Set to `1` to enable audio capture
- `RUST_LOG` — Log level (default: `info`)
- `BACKEND_TOKEN_SECRET` — Shared with gateway JWT secret; if empty, auth is bypassed (dev mode)
- `GATEWAY_ORIGIN` — Restrict CORS to this origin; if unset, `CorsLayer::permissive()` (dev mode)
- `GATEWAY_URL` — Gateway base URL for self-registration on startup
- `BACKEND_ID` — UUID identifying this backend instance in the registry

### Gateway (`streamio-gateway`)
- `GATEWAY_PORT` — HTTP server port (default: 8080)
- `GATEWAY_ORIGIN` — Own public URL (for CORS and cookie domain)
- `JWT_SECRET` — Shared secret for signing internal JWTs (required)
- `OIDC_ISSUER_URL` — OIDC provider discovery URL (e.g. `https://accounts.google.com`)
- `OIDC_CLIENT_ID` / `OIDC_CLIENT_SECRET` / `OIDC_REDIRECT_URI`
- `DATABASE_URL` — PostgreSQL connection string
- `REDIS_URL` — Redis connection string (default: `redis://127.0.0.1/`)
- `ADMIN_SUBS` — Comma-separated OIDC subject IDs with admin role

## Architecture

### Components

```
Browser → gateway:8080 → (JWT proxy) → backend:9001 (user A)
                       → (JWT proxy) → backend:9002 (user B)
```

- **Gateway** authenticates users via OIDC PKCE flow, issues internal JWTs, proxies WebSocket to the user's assigned backend, and hosts the admin panel.
- **Backend** verifies `X-Session-Token` header on `/ws` upgrade, starts a GStreamer screen-capture pipeline per connection, and reports health via `/healthz`.
- **PostgreSQL** stores backend registry and user→backend assignments.
- **Redis** holds PKCE verifiers + nonces (10-minute TTL) during login flow.

### Gateway Source Layout

- `gateway/src/auth.rs` — OIDC login/callback/logout; stores PKCE verifier+nonce in Redis; issues JWT cookie `sid`
- `gateway/src/session.rs` — JWT issue/verify with `SessionClaims` (sub, email, role, backend_id, exp)
- `gateway/src/middleware.rs` — `RequireSession` and `RequireAdmin` axum extractors
- `gateway/src/registry.rs` — Backend pool (PostgreSQL), health polling every 30s, `get_or_assign()`
- `gateway/src/proxy.rs` — WebSocket splice: client ↔ `tokio_tungstenite` ↔ backend
- `gateway/src/admin.rs` — Admin REST API + serves `client/admin.html`
- `gateway/src/main.rs` — Router, DB/Redis init, runs migration `gateway/migrations/001_init.sql`

### Backend Modifications (vs original)

- `backend/src/screen_server.rs` — Added `verify_token()` middleware on `/ws`; `/healthz` endpoint tracking active session count; CORS restricted to `GATEWAY_ORIGIN`
- `backend/src/screen_capture.rs` — Uses `streamio_types::SignalingMessage` (removed local definition)
- `backend/src/input.rs` — Uses `streamio_types::InputEvent` (removed local definition)

### Key Patterns

- **Bundled GStreamer**: At startup, checks for `lib/gstreamer-1.0/` next to the executable. If found, sets `GST_PLUGIN_PATH`, `GST_PLUGIN_SYSTEM_PATH=""`, and `GST_PLUGIN_SCANNER` before `gstreamer::init()`.
- **WebSocket signaling**: Clients connect via `/ws`, exchange SDP offer/answer and ICE candidates as JSON. Input events share the same WebSocket.
- **HTML embedding**: `client/screen.html` and `client/admin.html` are embedded at compile time via `include_str!()`.
- **Platform-conditional pipelines**: Screen capture source and hardware encoder use `#[cfg(target_os)]` — different GStreamer element names per platform.
- **Plugin validation**: On startup, `main.rs` verifies required GStreamer plugins (webrtc, nice, dtls, srtp, rtp, videoconvertscale) are available.
- **Dev mode (no auth)**: When `BACKEND_TOKEN_SECRET` is empty, the backend skips token verification. Gateway uses `CorsLayer::permissive()` when `GATEWAY_ORIGIN` is unset.

### Distribution

`./bundle.sh` creates a `dist/` directory containing the binary, `lib/` (GStreamer core libs + transitive deps), `lib/gstreamer-1.0/` (plugins), and `libexec/gst-plugin-scanner`.

On macOS, dylib paths are rewritten to `@executable_path/lib/...` via `install_name_tool`. On Linux, rpaths are set via `patchelf` (requires `sudo apt install patchelf` before running `bundle.sh`). On Windows, DLLs go next to the exe.

Note: Distributing with the bundled x264 software encoder triggers GPL-2.0 obligations — see `COPYING`.

### Dependencies

**Backend**: GStreamer 0.23 bindings, Axum 0.7, Enigo 0.2, Tokio 1, `jsonwebtoken 9`, `streamio-types`

**Gateway**: Axum 0.7, `openidconnect 3` (OIDC PKCE), `jsonwebtoken 9`, `sqlx 0.8` + PostgreSQL, `redis 0.27`, `tokio-tungstenite 0.24` (WS proxy), `reqwest 0.12`

**Shared (`streamio-types`)**: `serde`, `uuid`

For development (backend only): `brew install gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly pkgconf` (macOS).

## Phase Roadmap

- **Phase 1** (this branch): Auth + single-user MVP — OIDC login, JWT session, backend token verification
- **Phase 2**: Per-user desktop assignment — backend registry in PostgreSQL, health polling, `get_or_assign()`
- **Phase 3**: Session sharing — GStreamer `tee` element + multiple `webrtcbin` peers per pipeline
- **Phase 4**: Admin panel full features + Kubernetes dynamic provisioning (`kube` crate)
