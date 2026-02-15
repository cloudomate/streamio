# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Horizon Streamer — a Rust-based VDI/remote desktop streaming application. Captures screens and streams them to browsers via WebRTC using GStreamer. Supports bidirectional audio and remote mouse/keyboard input injection. Designed to ship as a self-contained bundle with GStreamer libs included.

## Build Commands

```bash
# Build
cargo build --release

# Run (default port 8123)
cargo run --release

# Run with debug logging
RUST_LOG=debug cargo run

# Type-check without building
cargo check

# Create self-contained distribution bundle (binary + GStreamer libs)
./bundle.sh

# Set up shell environment (GStreamer plugin paths, Rust, Homebrew)
source env.sh
```

There are no tests in this project currently.

## Environment Variables

- `PORT` — HTTP server port (default: 8123)
- `FPS` — Capture framerate (default: 30)
- `DISPLAY_INDEX` — macOS display to capture (default: 0)
- `ENABLE_AUDIO` — Set to `1` to enable audio capture
- `RUST_LOG` — Log level (default: `info`)

## Architecture

### Source Layout

- `src/main.rs` — Entry point. Includes `setup_bundled_gstreamer()` which auto-detects bundled GStreamer libs next to the executable (falls through to system GStreamer in dev mode). Builds platform-specific capture pipelines with hardware encoder fallback chain (VideoToolbox → NVENC → VAAPI → QuickSync → x264).
- `src/screen_capture.rs` — Screen capture + WebRTC pipeline construction. Platform-specific capture sources: `avfvideosrc` (macOS), `ximagesrc`/`pipewiresrc` (Linux), `d3d11screencapturesrc` (Windows). Also handles audio pipelines (Opus encoding) and incoming browser microphone.
- `src/screen_server.rs` — Axum HTTP/WebSocket server. Serves `client/screen.html` via `include_str!()`. Creates a `ScreenStreamer` per WebSocket connection and routes signaling + input events.
- `src/input.rs` — Cross-platform keyboard/mouse injection via Enigo. Runs on a dedicated `spawn_blocking` thread because Enigo is not `Send`.
- `client/screen.html` — Browser client (vanilla JS). WebRTC video playback, mouse/keyboard capture with coordinate transformation, audio controls, stats display.

### Key Patterns

- **Bundled GStreamer**: At startup, checks for `lib/gstreamer-1.0/` next to the executable. If found, sets `GST_PLUGIN_PATH`, `GST_PLUGIN_SYSTEM_PATH=""`, and `GST_PLUGIN_SCANNER` before `gstreamer::init()`. This enables self-contained distribution via `./bundle.sh`.
- **WebSocket signaling**: Clients connect via `/ws`, exchange SDP offer/answer and ICE candidates as JSON. Input events (mouse, keyboard, scroll) share the same WebSocket.
- **HTML embedding**: `client/screen.html` is embedded at compile time via `include_str!()`.
- **Platform-conditional pipelines**: Screen capture source and hardware encoder use `#[cfg(target_os)]` compile-time selection — different GStreamer element names per platform.

### Distribution

`./bundle.sh` creates a `dist/` directory containing:
- The binary
- `lib/` — GStreamer core libs + transitive deps (glib, openssl, opus, x264, etc.)
- `lib/gstreamer-1.0/` — Required GStreamer plugins
- `libexec/gst-plugin-scanner`

On macOS, dylib paths are rewritten to `@executable_path/lib/...` via `install_name_tool`. On Linux, rpaths are set via `patchelf`. On Windows, DLLs go next to the exe (Windows searches the exe directory automatically).

### Dependencies

GStreamer 0.23 bindings (gstreamer, gstreamer-app, gstreamer-webrtc, gstreamer-sdp, gstreamer-video), Axum 0.7 (web server), Enigo 0.2 (input injection), Tokio 1 (async).

For development (not needed for bundled distribution): `brew install gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly pkgconf` (macOS).
