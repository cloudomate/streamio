# Horizon Streamer

Remote 3D geological horizon visualization using Rust, wgpu (Vulkan/Metal), and GStreamer WebRTC.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Server (GPU VM)                                            │
│                                                             │
│  ┌──────────────┐    ┌─────────────────┐    ┌───────────┐  │
│  │    wgpu      │    │    GStreamer    │    │  WebRTC   │  │
│  │  3D Render   │───▶│  H.264 Encoder  │───▶│  Server   │──┼──▶ Browser
│  │  (offscreen) │    │  (HW/SW)        │    │           │  │
│  └──────────────┘    └─────────────────┘    └───────────┘  │
│        ▲                                          │        │
│        │              Input Events                │        │
│        └──────────────────────────────────────────┘        │
└─────────────────────────────────────────────────────────────┘
```

## Features

- **wgpu rendering**: Cross-platform GPU rendering (Vulkan on Linux, Metal on macOS)
- **Hardware encoding**: Automatically uses NVENC, VA-API, or VideoToolbox when available
- **WebRTC streaming**: Low-latency (<100ms) video streaming to browsers
- **Interactive controls**: Real-time camera manipulation (rotate, zoom, pan)

## Prerequisites

### macOS

```bash
# Install Homebrew if not already installed
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

# Install dependencies
brew install gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly pkgconf

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### Ubuntu/Debian

```bash
# Install GStreamer and development files
sudo apt update
sudo apt install -y \
    libgstreamer1.0-dev \
    libgstreamer-plugins-base1.0-dev \
    gstreamer1.0-plugins-base \
    gstreamer1.0-plugins-good \
    gstreamer1.0-plugins-bad \
    gstreamer1.0-plugins-ugly \
    gstreamer1.0-nice \
    libgstrtspserver-1.0-dev \
    libglib2.0-dev \
    pkg-config

# For NVIDIA GPU encoding (optional)
sudo apt install gstreamer1.0-plugins-bad

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## Building

```bash
# Clone and build
cd horizon-streamer
cargo build --release
```

## Running

```bash
# Start the server
cargo run --release

# Or with debug logging
RUST_LOG=debug cargo run
```

Then open http://localhost:8080 in a browser.

## Controls

| Input | Action |
|-------|--------|
| Left-drag | Rotate camera |
| Scroll wheel | Zoom in/out |
| Middle-drag | Pan camera |
| R key | Reset view |

## Configuration

Edit constants in `src/main.rs`:

```rust
const WIDTH: u32 = 1280;   // Render resolution
const HEIGHT: u32 = 720;
const FPS: u32 = 30;       // Target frame rate
const PORT: u16 = 8080;    // HTTP server port
```

## VM Deployment

### Recommended VM Specs

| Provider | Instance Type | GPU | Monthly Cost |
|----------|---------------|-----|--------------|
| AWS | g4dn.xlarge | T4 | ~$380 (on-demand) |
| GCP | n1-standard-4 + T4 | T4 | ~$350 |
| Azure | NC4as_T4_v3 | T4 | ~$400 |
| Hetzner | GPU Server | RTX 4000 | ~€180 |

### Docker Deployment

```dockerfile
FROM nvidia/cuda:12.0-runtime-ubuntu22.04

RUN apt-get update && apt-get install -y \
    libgstreamer1.0-0 \
    gstreamer1.0-plugins-base \
    gstreamer1.0-plugins-good \
    gstreamer1.0-plugins-bad \
    gstreamer1.0-nice

COPY target/release/horizon-streamer /app/
WORKDIR /app
EXPOSE 8080
CMD ["./horizon-streamer"]
```

## Project Structure

```
horizon-streamer/
├── Cargo.toml
├── src/
│   ├── main.rs           # Entry point
│   ├── renderer/
│   │   ├── mod.rs        # wgpu 3D renderer
│   │   └── shader.wgsl   # GPU shaders
│   ├── streamer/
│   │   └── mod.rs        # GStreamer WebRTC pipeline
│   └── server/
│       └── mod.rs        # HTTP/WebSocket server
└── client/
    └── index.html        # Browser client
```

## Loading Real Horizon Data

Replace the sample horizon in `src/renderer/mod.rs`:

```rust
fn create_sample_horizon() -> (Vec<Vertex>, Vec<u32>, f32, f32) {
    // Load from SEGY, ZMAP, OpenVDS, etc.
    // Return vertices, indices, and depth range
}
```

## License

MIT
