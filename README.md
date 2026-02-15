# Horizon Streamer

Stream your desktop to any browser. Low-latency screen capture with remote keyboard and mouse control over WebRTC — a lightweight, self-contained VDI solution.

## How It Works

Horizon Streamer captures your screen using platform-native APIs, encodes it with hardware-accelerated H.264, and delivers it to browsers via WebRTC. Remote input (mouse, keyboard, scroll) flows back over the same connection, giving you full control of the host machine from any modern browser.

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
| macOS (Apple Silicon) | `horizon-streamer-macos-arm64.tar.gz` |
| Linux (x86_64) | `horizon-streamer-linux-x86_64.tar.gz` |

```bash
# Extract and run
tar xzf horizon-streamer-*.tar.gz
./horizon-streamer
```

Then open **http://localhost:8123** in your browser.

## Features

- **Low-latency streaming** — WebRTC with hardware H.264 encoding for sub-100ms latency
- **Remote desktop control** — Full mouse and keyboard input from the browser
- **Hardware encoder fallback chain** — VideoToolbox → NVENC → VAAPI → QuickSync → x264
- **Bidirectional audio** — System audio to browser, browser microphone to host
- **Platform-native capture** — AVFoundation (macOS), X11/PipeWire (Linux), DirectX (Windows)
- **Zero-install client** — Just a browser, no plugins or extensions
- **Self-contained binary** — Single folder with all dependencies bundled
- **Signed and notarized** — macOS builds are code-signed and Apple-notarized

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `PORT` | `8123` | HTTP server port |
| `FPS` | `30` | Capture framerate |
| `DISPLAY_INDEX` | `0` | macOS display index (0 = main) |
| `ENABLE_AUDIO` | `0` | Set to `1` to enable audio capture |
| `RUST_LOG` | `info` | Log level (`debug`, `info`, `warn`, `error`) |

```bash
PORT=9000 FPS=60 ENABLE_AUDIO=1 ./horizon-streamer
```

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

### Build and Run

```bash
cargo build --release
cargo run --release
```

### Create a Self-Contained Bundle

```bash
./bundle.sh
```

Produces a `dist/` directory with the binary and all GStreamer shared libraries. Copy the entire folder to any machine of the same OS/architecture — no GStreamer installation required.

On Linux, `patchelf` is needed: `sudo apt install patchelf`

## License

The Horizon Streamer source code is licensed under the **Apache License 2.0**. See [LICENSE](LICENSE) for details.

The pre-built binary bundles include GStreamer plugins and third-party libraries under their own licenses (LGPL-2.1+, GPL-2.0, BSD). In particular, the inclusion of x264 (GPL-2.0) means the bundled distribution as a whole is subject to the terms of the **GNU General Public License v2.0**. See [COPYING](COPYING) for the full GPL-2.0 text.
