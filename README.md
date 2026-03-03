# Streamio

Stream your desktop to any browser. Low-latency screen capture with remote keyboard and mouse control over WebRTC — a lightweight, self-contained VDI solution.

## How It Works

Streamio captures your screen using platform-native APIs, encodes it with hardware-accelerated H.264, and delivers it to browsers via WebRTC. Remote input (mouse, keyboard, scroll) flows back over the same connection, giving you full control of the host machine from any modern browser.

```
 Host Machine                              Browser
┌─────────────────────────────┐       ┌──────────────────┐
│  Screen Capture             │       │                  │
│  (AVFoundation/X11/DirectX) │       │  Video Playback  │
│         │                   │       │                  │
│         ▼                   │  H.264│                  │
│  H.264 Encoder              │──────▶│  Audio Playback  │
│  (VideoToolbox/NVENC/x264)  │  Opus │                  │
│         │                   │       │                  │
│  System Audio ──▶ Opus ─────│──────▶│                  │
│                             │       │  Mouse/Keyboard  │
│  Mouse/Keyboard Injection ◀─│◀──────│  Events          │
│  (Enigo)                    │       │                  │
└─────────────────────────────┘       └──────────────────┘
         WebRTC (peer-to-peer, encrypted)
```

## Download

Pre-built bundles are available on the [Releases](../../releases) page. No dependencies required — GStreamer and all shared libraries are included.

| Platform | Download |
|----------|----------|
| macOS (Apple Silicon) | `streamio-macos-arm64.tar.gz` |
| Linux (x86_64) | `streamio-linux-x86_64.tar.gz` |

```bash
# Extract and run
tar xzf streamio-*.tar.gz
./streamio
```

Then open **http://localhost:8123** in your browser.

---

## Features

### Streaming (backend)

- **Low-latency streaming** — WebRTC with hardware H.264 encoding, typically under 100 ms glass-to-glass
- **Remote desktop control** — Full mouse, keyboard, and scroll input from the browser
- **Hardware encoder fallback chain** — VideoToolbox → NVENC → VAAPI → Intel QuickSync → x264 software
- **Bidirectional audio** — System audio to browser, browser microphone to host
- **Platform-native capture** — AVFoundation (macOS), X11/PipeWire (Linux), DirectX (Windows)
- **Zero-install client** — Requires only a modern browser, no plugins or extensions
- **Self-contained binary** — Single folder with all GStreamer dependencies bundled

### Gateway (enterprise VDI)

- **OIDC authentication** — Integrates with any OAuth2/OIDC provider (Google, Keycloak, Azure AD, Okta, etc.)
- **Per-user desktop assignment** — Each user is routed to their own backend instance
- **Automatic VM provisioning** — On first login, spins up a per-user KubeVirt VM via Kubernetes
- **VM lifecycle management** — Start, stop, and delete VMs from the admin API
- **Session shadowing** — Assign an observer to share a user's session
- **Admin panel** — Web UI for managing backends, user assignments, and sessions
- **Health monitoring** — Background polling marks backends healthy/unhealthy every 30 s
- **Backend self-registration** — Backends call `POST /internal/register` on startup

---

## Architecture

### Standalone mode (single user)

```
Browser → backend:8123
```

Run `streamio` directly. No auth, no gateway.

### Gateway mode (multi-user VDI)

```
Browser → gateway:8080 ──JWT proxy──▶ backend:9001  (user A's desktop)
                         ──JWT proxy──▶ backend:9002  (user B's desktop)
                         ──JWT proxy──▶ backend:9003  (user C's desktop)
```

The gateway handles OIDC login, issues signed JWT sessions, looks up each user's assigned backend, and splices the WebSocket bidirectionally.

### With KubeVirt (dynamic VM provisioning)

```
User logs in (first time)
  Gateway → Kubernetes API → DataVolume (clone base image PVC)
                           → VirtualMachine (running: false)
                           → patch VM running: true
  VM boots → streamio agent starts → POST /internal/register
  Gateway polls DB until healthy → user gets their stream

User logs in (returning, VM was stopped)
  Gateway detects unhealthy backend → patches VM running: true
  Polls until re-registered → stream resumes
```

---

## Quick Start

### Standalone (no auth)

```bash
./streamio
```

Open http://localhost:8123.

### Docker Compose (gateway + backend + Postgres + Redis)

1. Copy `.env.example` to `.env` and fill in your OIDC credentials:

```bash
JWT_SECRET=a-long-random-secret-at-least-32-chars
OIDC_ISSUER_URL=https://accounts.google.com
OIDC_CLIENT_ID=your-client-id.apps.googleusercontent.com
OIDC_CLIENT_SECRET=your-client-secret
OIDC_REDIRECT_URI=http://localhost:8080/auth/callback
GATEWAY_ORIGIN=http://localhost:8080
ADMIN_SUBS=google-subject-id-of-admin-user
```

2. Start the stack:

```bash
docker compose up
```

3. Open http://localhost:8080. You will be redirected to your OIDC provider to log in.

---

## Configuration

### Backend (`streamio`)

| Variable | Default | Description |
|----------|---------|-------------|
| `PORT` | `8123` | HTTP server port |
| `FPS` | `30` | Screen capture framerate |
| `DISPLAY_INDEX` | `0` | macOS display index (0 = primary) |
| `ENABLE_AUDIO` | `0` | Set to `1` to enable audio capture/playback |
| `BACKEND_TOKEN_SECRET` | _(empty)_ | Shared JWT secret with gateway. If empty, token verification is skipped (dev mode) |
| `GATEWAY_URL` | _(empty)_ | Gateway base URL for self-registration on startup, e.g. `http://gateway:8080` |
| `BACKEND_ID` | _(empty)_ | UUID identifying this instance in the backend registry |
| `GATEWAY_ORIGIN` | _(empty)_ | Restrict CORS to this origin. If empty, all origins allowed (dev mode) |
| `RUST_LOG` | `info` | Log level: `error`, `warn`, `info`, `debug`, `trace` |

```bash
# Example: run backend with auth enabled
PORT=9001 BACKEND_TOKEN_SECRET=my-jwt-secret GATEWAY_URL=http://gateway:8080 \
  BACKEND_ID=00000000-0000-0000-0000-000000000001 ./streamio
```

### Gateway (`streamio-gateway`)

| Variable | Default | Required | Description |
|----------|---------|----------|-------------|
| `GATEWAY_PORT` | `8080` | No | HTTP listen port |
| `GATEWAY_ORIGIN` | `http://localhost:8080` | No | Public URL of the gateway (used for CORS and cookie domain) |
| `JWT_SECRET` | — | **Yes** | Secret for signing internal JWTs. Must match `BACKEND_TOKEN_SECRET` on all backends |
| `DATABASE_URL` | — | **Yes** | PostgreSQL connection string, e.g. `postgres://user:pass@host/db` |
| `REDIS_URL` | `redis://127.0.0.1/` | No | Redis connection string |
| `OIDC_ISSUER_URL` | — | **Yes** | OIDC discovery URL, e.g. `https://accounts.google.com` |
| `OIDC_CLIENT_ID` | — | **Yes** | OAuth2 client ID |
| `OIDC_CLIENT_SECRET` | — | **Yes** | OAuth2 client secret |
| `OIDC_REDIRECT_URI` | — | **Yes** | Callback URL registered with your OIDC provider, e.g. `https://vdi.example.com/auth/callback` |
| `ADMIN_SUBS` | _(empty)_ | No | Comma-separated OIDC `sub` claim values that receive the admin role |
| `RUST_LOG` | `info` | No | Log level |

### KubeVirt VM provisioner (optional)

Only relevant when running the gateway inside a Kubernetes cluster with KubeVirt + CDI installed.

| Variable | Default | Description |
|----------|---------|-------------|
| `KUBEVIRT_ENABLED` | `false` | Set to `true` to enable the KubeVirt provisioner |
| `KUBEVIRT_NAMESPACE` | `vdi` | Kubernetes namespace where VMs are created |
| `KUBEVIRT_GATEWAY_URL` | _(uses `GATEWAY_ORIGIN`)_ | URL injected into VM cloud-init so the streamio agent inside the VM can reach the gateway |
| `DEFAULT_BASE_PVC` | — | Name of the base image PVC to clone for new VMs. Required for auto-provisioning on first login |
| `DEFAULT_DISK_SIZE` | `60Gi` | User disk size (PVC clone size) |
| `DEFAULT_VM_MEMORY` | `4Gi` | VM memory request |
| `DEFAULT_VM_CPU` | `2` | VM CPU cores |

---

## OIDC Setup

The gateway uses the **Authorization Code flow with PKCE**. It works with any standard OIDC provider.

### Google

1. Go to [Google Cloud Console](https://console.cloud.google.com/) → APIs & Services → Credentials
2. Create an OAuth 2.0 Client ID (Web application)
3. Add your callback URL to **Authorized redirect URIs**: `https://your-gateway.example.com/auth/callback`
4. Set:
   ```
   OIDC_ISSUER_URL=https://accounts.google.com
   OIDC_CLIENT_ID=<your-client-id>.apps.googleusercontent.com
   OIDC_CLIENT_SECRET=<your-client-secret>
   ```

### Keycloak

```
OIDC_ISSUER_URL=https://keycloak.example.com/realms/your-realm
OIDC_CLIENT_ID=streamio
OIDC_CLIENT_SECRET=<client-secret>
```

### Finding your admin `sub`

After logging in, inspect the JWT cookie named `sid` (base64-decode the middle segment) — the `sub` field is the value to add to `ADMIN_SUBS`.

---

## Admin Panel

Navigate to `/admin` (requires admin role) for the web UI. The same functionality is available via the REST API.

### REST API reference

All endpoints require an authenticated session with admin role (`Authorization` via the `sid` cookie).

#### Backends

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin/api/backends` | List all registered backends with health status |
| `POST` | `/admin/api/backends/provision` | Provision a new KubeVirt VM (see body below) |

**Provision request body:**
```json
{
  "user_sub": "google|1234567890",
  "os_type": "ubuntu",
  "base_pvc": "ubuntu-22.04-base",
  "disk_size": "60Gi",
  "memory": "4Gi",
  "cpu_cores": 2,
  "label": "alice-desktop"
}
```

`os_type` values: `ubuntu`, `windows11`, `alpine`

#### VM Lifecycle

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/admin/api/vms/:id/start` | Power on a stopped VM |
| `POST` | `/admin/api/vms/:id/stop` | Gracefully power off a running VM |
| `DELETE` | `/admin/api/vms/:id` | Delete VM, DataVolume, and PVC |
| `GET` | `/admin/api/vms/:id/state` | Query live power state from KubeVirt |

Power states returned by `/state`: `stopped`, `starting`, `running`, `stopping`, `provisioning`

#### User Assignments

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin/api/users` | List user → backend assignments |
| `POST` | `/admin/api/assignments` | Manually assign a user to a backend |
| `DELETE` | `/admin/api/assignments/:sub` | Remove a user's assignment |

**Assignment request body:**
```json
{ "user_sub": "google|1234567890", "backend_id": "uuid-of-backend" }
```

#### Sessions & Shadowing

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin/api/sessions` | List backends and their active state |
| `POST` | `/admin/api/sessions/:id/shadow` | Route another user to observe a session |

**Shadow request body:**
```json
{ "user_sub": "google|observer-id", "role": "observer" }
```

---

## KubeVirt Deployment

### Prerequisites

- Kubernetes cluster with [KubeVirt](https://kubevirt.io/quickstart_minikube/) installed
- [CDI (Containerized Data Importer)](https://github.com/kubevirt/containerized-data-importer) installed for PVC cloning

### 1. Create RBAC for the gateway

```bash
kubectl apply -f deploy/kubevirt-rbac.yaml
```

This creates a `ServiceAccount`, `ClusterRole`, and `ClusterRoleBinding` in the `vdi` namespace granting the gateway rights to manage VMs and DataVolumes.

### 2. Import a base image

```bash
kubectl apply -f deploy/ubuntu-base-datavolume.yaml
```

This imports Ubuntu 22.04 cloud image into a PVC named `ubuntu-22.04-base`. Wait for it to complete:

```bash
kubectl get datavolume -n vdi -w
# ubuntu-22.04-base   Succeeded
```

You can create additional base images for Windows or other distros by following the same pattern with a different source URL and PVC name.

### 3. Deploy the gateway with KubeVirt enabled

Add to your gateway deployment environment:

```yaml
env:
  - name: KUBEVIRT_ENABLED
    value: "true"
  - name: KUBEVIRT_NAMESPACE
    value: "vdi"
  - name: DEFAULT_BASE_PVC
    value: "ubuntu-22.04-base"
  - name: DEFAULT_DISK_SIZE
    value: "60Gi"
  - name: DEFAULT_VM_MEMORY
    value: "4Gi"
  - name: DEFAULT_VM_CPU
    value: "2"
serviceAccountName: streamio-gateway
```

### 4. How auto-provisioning works

When a user logs in for the first time and no backend assignment exists:

1. Gateway generates a `backend_id` UUID and calls the KubeVirt provisioner
2. A CDI `DataVolume` is created — it clones `DEFAULT_BASE_PVC` into a new per-user PVC named `vdi-<username>-disk`
3. A `VirtualMachine` is created (initially stopped) with cloud-init injecting the streamio agent config
4. The VM is powered on (`spec.running: true`)
5. The VM boots, cloud-init runs, the streamio agent starts and calls `POST /internal/register` with its IP and `BACKEND_ID`
6. The gateway polls the database every 3 s (up to 120 s) until the backend is healthy
7. The user is connected to their stream

On subsequent logins, if the VM was stopped, the gateway wakes it automatically and waits for re-registration.

---

## Building from Source

### Prerequisites

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

**Rust:**
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### Build

```bash
# All crates
cargo build --release --workspace

# Backend only
cargo build --release -p streamio

# Gateway only
cargo build --release -p streamio-gateway
```

### Create a self-contained bundle (backend)

```bash
./bundle.sh   # requires patchelf on Linux: sudo apt install patchelf
```

Produces a `dist/` directory with the binary and all GStreamer shared libraries bundled.

---

## License

The Streamio source code is licensed under the **Apache License 2.0**. See [LICENSE](LICENSE) for details.

The pre-built binary bundles include GStreamer plugins and third-party libraries under their own licenses (LGPL-2.1+, GPL-2.0, BSD). In particular, the inclusion of x264 (GPL-2.0) means the bundled distribution as a whole is subject to the terms of the **GNU General Public License v2.0**. See [COPYING](COPYING) for the full GPL-2.0 text.
