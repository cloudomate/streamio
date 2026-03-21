# Streamio

Multi-session VDI without RDS licensing. Multiple users connect via browser to a single Windows host — each gets an isolated virtual display, dedicated input routing, and a separate user account. No Remote Desktop Services, no CALs, no plugins.

## What It Does

Streamio turns a single Windows machine into a multi-user remote desktop server. Each user gets:

- **Their own virtual display** — created on demand via a custom IddCx driver
- **Their own screen capture pipeline** — hardware-accelerated H.264 encoding via GStreamer + WebRTC
- **Isolated input** — mouse and keyboard routed only to their display, no cross-session interference
- **A separate local Windows account** — file and profile isolation
- **Browser-only access** — zero-install client, works in any modern browser

```
                        ┌──────────────────────────────────────────────────┐
                        │             Windows Host (Single Machine)         │
                        │                                                  │
  Browser (User A) ───► │  Backend A  (port 9001, virtual display 1)       │
  Browser (User B) ───► │  Backend B  (port 9002, virtual display 2)       │
  Browser (User C) ───► │  Backend C  (port 9003, virtual display 3)       │
                        │       ▲           ▲           ▲                  │
                        │       └───────────┼───────────┘                  │
                        │                   │                              │
                        │          Session Manager                         │
                        │     (creates displays, launches backends,        │
                        │      routes input, manages accounts)             │
                        │                   │                              │
                        │    ┌──────────────┼──────────────┐               │
                        │    │              │              │               │
                        │  IddCx Driver  VHID Driver   display-ctl        │
                        │  (virtual       (keyboard     (display           │
                        │   monitors)      injection)    management)       │
                        └──────────────────────────────────────────────────┘
                                            ▲
                                            │
                              Gateway (OIDC auth, WebSocket proxy)
                                            ▲
                                            │
                                   Users via browser
```

### Streamio vs RDS

| | RDS | Streamio |
|---|---|---|
| Separate displays per user | Yes | Yes (IddCx virtual displays) |
| Input isolation | Yes | Yes (per-session SendInput + VHID) |
| File/profile isolation | Yes | Yes (separate local accounts) |
| Browser-only client | RD Web Client | Yes (WebRTC, zero install) |
| GPU-accelerated encoding | RemoteFX (deprecated) | Yes (H.264 via GStreamer) |
| NAT traversal | Needs RD Gateway | Built-in (WebRTC ICE) |
| **RDS CAL licensing** | **Required** | **Not needed** |

---

## Components

### Backend (`streamio`)

Per-user process that captures one virtual display and streams it to the browser.

- GStreamer pipeline: `d3d11screencapturesrc` → H.264 encoder → `webrtcbin`
- Hardware encoder chain: AMF → NVENC → QuickSync → x264 fallback
- Bidirectional audio via Opus
- Receives input events over WebSocket, forwards to session manager via named pipe
- Platform support: Windows (primary), macOS (AVFoundation), Linux (X11/PipeWire)

### Session Manager (`streamio-session-manager`)

Windows service running as SYSTEM. Orchestrates all per-user resources.

- **Display management** — creates/destroys virtual displays via `display-ctl` + IddCx driver
- **Account management** — creates local Windows accounts per VDI user
- **Backend launcher** — spawns backend processes via `schtasks` in the interactive desktop session
- **Input routing** — per-session named pipes, translates coordinates to display region, injects via SendInput (mouse) and VHID (keyboard)
- **REST API** — for gateway or direct use (`POST/DELETE/GET /api/sessions`)

### Gateway (`streamio-gateway`)

Authentication and routing layer. Can run on the same host or remotely.

- OIDC authentication (Google, Keycloak, Azure AD, Okta, etc.)
- Per-user desktop assignment with automatic backend provisioning
- WebSocket proxy: browser ↔ gateway ↔ backend
- Admin panel (web UI + REST API) for managing backends, users, sessions
- Optional KubeVirt VM provisioner for cloud deployments

### Drivers

- **VHID driver** (`driver/vhid/`) — KMDF driver using Windows Virtual HID Framework. Provides a virtual HID device for keyboard injection that works even on the lock screen.
- **Virtual display driver** (`driver/display/`) — IddCx UMDF2 indirect display driver. Creates virtual monitors on demand — each appears as a real display in Windows.
- **display-ctl** (`display-ctl/`) — CLI tool to manage virtual displays: `display-ctl create 1920 1080 60`, `display-ctl remove <index>`, `display-ctl list`.

### Shared Types (`streamio-types`)

Common types used across crates: `InputEvent`, `SignalingMessage`, `SessionClaims`, `BackendInfo`.

---

## Quick Start

### Standalone (single user, no auth)

```bash
# macOS or Linux — just run the backend directly
./streamio
# Open http://localhost:8123
```

### Multi-Session VDI (Windows host)

**Prerequisites on the Windows host:**
- VHID driver installed (`pnputil /add-driver streamio-vhid.inf /install`)
- Virtual display driver installed (`pnputil /add-driver streamio-display.inf /install`)
- Test signing enabled (`bcdedit /set testsigning on`)
- GStreamer MSVC x86_64 installed to `C:\gstreamer\1.0\msvc_x86_64\`
- Executables in `C:\Program Files\Streamio\`: `streamio.exe`, `streamio-session-manager.exe`, `display-ctl.exe`

**Start the session manager** (from the interactive desktop, not SSH):
```cmd
"C:\Program Files\Streamio\streamio-session-manager.exe"
```

**Create a session:**
```bash
curl -X POST http://<host>:9100/api/sessions \
  -H "Content-Type: application/json" \
  -d '{"user_id": "alice", "width": 1920, "height": 1080}'
```

Response:
```json
{
  "session_id": "a1b2c3d4-...",
  "backend_port": 9001,
  "display_index": 1,
  "display_rect": [1920, 0, 1920, 1080]
}
```

**Connect:** Open `http://<host>:9001` in your browser.

**List sessions:**
```bash
curl http://<host>:9100/api/sessions
```

**Destroy a session:**
```bash
curl -X DELETE http://<host>:9100/api/sessions/<session_id>
```

### Docker Compose (gateway + backend + Postgres + Redis)

For development or single-user gateway mode:

1. Copy `.env.example` to `.env` and fill in OIDC credentials:
```bash
JWT_SECRET=a-long-random-secret-at-least-32-chars
OIDC_ISSUER_URL=https://accounts.google.com
OIDC_CLIENT_ID=your-client-id.apps.googleusercontent.com
OIDC_CLIENT_SECRET=your-client-secret
OIDC_REDIRECT_URI=http://localhost:8080/auth/callback
GATEWAY_ORIGIN=http://localhost:8080
ADMIN_SUBS=google-subject-id-of-admin-user
```

2. Start:
```bash
docker compose up
```

3. Open http://localhost:8080 — redirects to OIDC provider login.

---

## Building from Source

### Prerequisites

**Rust:**
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

**macOS:**
```bash
brew install gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly pkgconf
```

**Ubuntu/Debian:**
```bash
sudo apt install -y \
    libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
    gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly \
    gstreamer1.0-nice libglib2.0-dev pkg-config
```

**Windows:**
- Install [GStreamer MSVC x86_64](https://gstreamer.freedesktop.org/download/) to `C:\gstreamer\1.0\msvc_x86_64\`
- Install [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with C++ workload
- Install [Windows Driver Kit (WDK)](https://learn.microsoft.com/en-us/windows-hardware/drivers/download-the-wdk) for building drivers

### Build

```bash
# All crates
cargo build --release --workspace

# Individual crates
cargo build --release -p streamio                    # backend
cargo build --release -p streamio-gateway            # gateway
cargo build --release -p streamio-session-manager    # session manager
cargo build --release -p display-ctl                 # virtual display CLI
```

### Build Drivers (Windows only)

Drivers require WDK and must be code-signed (or test-signed with `bcdedit /set testsigning on`).

```cmd
cd driver\vhid
build.bat

cd driver\display
build.bat
```

### Create a Self-Contained Bundle (macOS/Linux)

```bash
./bundle.sh   # requires patchelf on Linux: sudo apt install patchelf
```

Produces a `dist/` directory with the binary and all GStreamer shared libraries bundled — no system dependencies required at runtime.

---

## Configuration Reference

### Backend (`streamio`)

| Variable | Default | Description |
|----------|---------|-------------|
| `PORT` | `8123` | HTTP server port |
| `FPS` | `30` | Screen capture framerate |
| `DISPLAY_INDEX` | `0` | Virtual display index to capture |
| `SESSION_ID` | _(empty)_ | Session ID for input pipe name (set by session manager) |
| `ENABLE_AUDIO` | `0` | Set to `1` to enable audio capture/playback |
| `BACKEND_TOKEN_SECRET` | _(empty)_ | JWT secret shared with gateway. Empty = no auth (dev mode) |
| `GATEWAY_URL` | _(empty)_ | Gateway URL for self-registration |
| `BACKEND_ID` | _(empty)_ | UUID identifying this instance |
| `GATEWAY_ORIGIN` | _(empty)_ | CORS allowed origin. Empty = all origins (dev mode) |
| `STREAMIO_LOG_FILE` | _(empty)_ | Write logs to this file (in addition to stderr) |

### Session Manager (`streamio-session-manager`)

| Variable | Default | Description |
|----------|---------|-------------|
| `SESSION_MANAGER_PORT` | `9100` | REST API port |
| `BACKEND_PATH` | `C:\Program Files\Streamio\streamio.exe` | Path to backend executable |
| `BACKEND_TOKEN_SECRET` | _(empty)_ | Token secret passed to launched backends |
| `GATEWAY_URL` | _(empty)_ | Gateway URL passed to launched backends |

### Gateway (`streamio-gateway`)

| Variable | Default | Required | Description |
|----------|---------|----------|-------------|
| `GATEWAY_PORT` | `8080` | No | HTTP listen port |
| `GATEWAY_ORIGIN` | `http://localhost:8080` | No | Public URL (CORS + cookie domain) |
| `JWT_SECRET` | — | **Yes** | JWT signing secret (must match `BACKEND_TOKEN_SECRET`) |
| `DATABASE_URL` | — | **Yes** | PostgreSQL connection string |
| `REDIS_URL` | `redis://127.0.0.1/` | No | Redis connection string |
| `OIDC_ISSUER_URL` | — | **Yes** | OIDC discovery URL |
| `OIDC_CLIENT_ID` | — | **Yes** | OAuth2 client ID |
| `OIDC_CLIENT_SECRET` | — | **Yes** | OAuth2 client secret |
| `OIDC_REDIRECT_URI` | — | **Yes** | OAuth2 callback URL |
| `ADMIN_SUBS` | _(empty)_ | No | Comma-separated OIDC `sub` values for admin access |
| `KUBEVIRT_ENABLED` | `false` | No | Enable KubeVirt VM provisioning |
| `KUBEVIRT_NAMESPACE` | `vdi` | No | Kubernetes namespace for VMs |
| `DEFAULT_BASE_PVC` | — | No | Base PVC for DataVolume cloning (required for auto-provisioning) |

---

## OIDC Setup

The gateway uses **Authorization Code flow with PKCE**. Works with any standard OIDC provider.

### Google

1. Go to [Google Cloud Console](https://console.cloud.google.com/) → APIs & Services → Credentials
2. Create OAuth 2.0 Client ID (Web application)
3. Add callback URL: `https://your-gateway.example.com/auth/callback`
4. Set `OIDC_ISSUER_URL=https://accounts.google.com`

### Keycloak

```
OIDC_ISSUER_URL=https://keycloak.example.com/realms/your-realm
OIDC_CLIENT_ID=streamio
```

### Finding your admin `sub`

After logging in, decode the `sid` cookie (base64-decode the middle JWT segment). The `sub` field is the value for `ADMIN_SUBS`.

---

## Admin Panel

Navigate to `/admin` (requires admin role). Also available as REST API:

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin/api/backends` | List backends with health status |
| `POST` | `/admin/api/backends/provision` | Provision a KubeVirt VM |
| `GET` | `/admin/api/users` | List user → backend assignments |
| `POST` | `/admin/api/assignments` | Assign user to backend |
| `DELETE` | `/admin/api/assignments/:sub` | Remove assignment |
| `POST` | `/admin/api/vms/:id/start` | Start a VM |
| `POST` | `/admin/api/vms/:id/stop` | Stop a VM |
| `DELETE` | `/admin/api/vms/:id` | Delete VM and storage |
| `GET` | `/admin/api/vms/:id/state` | Query VM power state |
| `POST` | `/admin/api/sessions/:id/shadow` | Shadow a user's session |

---

## KubeVirt Deployment

For cloud deployments where each user gets a full VM instead of a shared host.

### Setup

```bash
# 1. Create RBAC
kubectl apply -f deploy/kubevirt-rbac.yaml

# 2. Import base image
kubectl apply -f deploy/ubuntu-base-datavolume.yaml
kubectl get datavolume -n vdi -w  # wait for Succeeded

# 3. Deploy gateway with KubeVirt enabled
# Add to your deployment: KUBEVIRT_ENABLED=true, DEFAULT_BASE_PVC=ubuntu-22.04-base
```

### Auto-provisioning flow

1. User logs in for the first time → gateway creates DataVolume (clone of base image) + VirtualMachine
2. VM boots → cloud-init runs → streamio agent starts → calls `POST /internal/register`
3. Gateway polls until backend is healthy → user gets their stream
4. On subsequent logins, if VM was stopped, gateway wakes it automatically

---

## Project Structure

```
streamio/
├── Cargo.toml                  # workspace: backend, gateway, types, service, display-ctl, session-manager
├── backend/                    # screen capture + WebRTC streaming (one per user)
│   └── src/
│       ├── main.rs             # GStreamer init, plugin validation
│       ├── screen_capture.rs   # GStreamer pipeline, DXGI handover
│       ├── screen_server.rs    # Axum HTTP/WS server, pipeline lifecycle
│       └── input.rs            # Input forwarding (enigo + named pipe)
├── gateway/                    # OIDC auth, WebSocket proxy, admin panel
│   ├── src/
│   │   ├── auth.rs             # OIDC login/callback/logout
│   │   ├── session.rs          # JWT issue/verify
│   │   ├── middleware.rs       # RequireSession, RequireAdmin extractors
│   │   ├── registry.rs         # Backend pool, health polling, assignment
│   │   ├── proxy.rs            # WebSocket splice (browser ↔ backend)
│   │   ├── admin.rs            # Admin REST API + UI
│   │   ├── provisioner.rs      # KubeVirt VM provisioner
│   │   └── main.rs             # Router, DB/Redis init
│   └── migrations/             # PostgreSQL schema
├── session-manager/            # Windows service — multi-session orchestrator
│   └── src/
│       ├── main.rs             # Service entry, HTTP API, session lifecycle
│       ├── display.rs          # Virtual display create/destroy
│       ├── accounts.rs         # Local Windows account management
│       ├── launcher.rs         # Backend process launcher (schtasks)
│       ├── input_router.rs     # Per-session input: SendInput mouse, VHID keyboard
│       ├── window_mgr.rs       # Window confinement (stub)
│       └── api.rs              # REST API handlers
├── types/                      # Shared types (InputEvent, SignalingMessage, etc.)
├── display-ctl/                # CLI for virtual display management
├── service/                    # Legacy input helper (being replaced by session-manager)
├── driver/
│   ├── vhid/                   # KMDF VHF virtual HID driver
│   └── display/                # IddCx UMDF2 virtual display driver
├── client/
│   ├── screen.html             # Browser streaming UI
│   └── admin.html              # Admin panel UI
├── deploy/                     # Kubernetes manifests
├── docker-compose.yml          # Local dev environment
└── bundle.sh                   # macOS/Linux self-contained bundle script
```

---

## License

The Streamio source code is licensed under the **Apache License 2.0**. See [LICENSE](LICENSE) for details.

The pre-built binary bundles include GStreamer plugins and third-party libraries under their own licenses (LGPL-2.1+, GPL-2.0, BSD). In particular, the inclusion of x264 (GPL-2.0) means the bundled distribution as a whole is subject to the terms of the **GNU General Public License v2.0**. See [COPYING](COPYING) for the full GPL-2.0 text.
