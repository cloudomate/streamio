# Horizon Streamer

VDI-style screen capture and WebRTC streaming. Captures your desktop and streams it to browsers with remote keyboard/mouse control and bidirectional audio.

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│  Server                                                       │
│                                                               │
│  ┌───────────────┐    ┌─────────────────┐    ┌───────────┐   │
│  │  Screen/Audio  │    │   GStreamer      │    │  WebRTC   │   │
│  │  Capture       │───▶│   H.264 + Opus   │───▶│  Server   │──┼──▶ Browser
│  │                │    │   (HW/SW)        │    │           │   │
│  └───────────────┘    └─────────────────┘    └───────────┘   │
│                                                    │          │
│                     Mouse/Keyboard Input            │          │
│                     ◀───────────────────────────────┘          │
└──────────────────────────────────────────────────────────────┘
```

## Features

- **Screen capture**: Platform-native capture (AVFoundation on macOS, X11/PipeWire on Linux, DirectX on Windows)
- **Hardware encoding**: Automatic fallback chain — VideoToolbox → NVENC → VAAPI → QuickSync → x264
- **WebRTC streaming**: Low-latency video + audio to browsers
- **Remote input**: Mouse and keyboard injection from the browser
- **Bidirectional audio**: System audio to browser + browser microphone to local playback
- **Self-contained distribution**: `./bundle.sh` packages the binary with all GStreamer libs

## Quick Start

### Prerequisites (for building)

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

Then open http://localhost:8123 in a browser.

### Create Self-Contained Bundle

```bash
./bundle.sh
```

This creates a `dist/` directory with the binary and all GStreamer shared libraries bundled. The `dist/` folder can be copied to any machine of the same OS/architecture — no GStreamer installation required.

On **Linux**, `patchelf` is needed for the bundling step: `sudo apt install patchelf`

On **Windows**, copy GStreamer DLLs from `C:\gstreamer\1.0\msvc_x86_64\` next to the `.exe`.

## Configuration

Environment variables:

| Variable | Default | Description |
|----------|---------|-------------|
| `PORT` | 8123 | HTTP server port |
| `FPS` | 30 | Capture framerate |
| `DISPLAY_INDEX` | 0 | macOS display to capture |
| `ENABLE_AUDIO` | 0 | Set to `1` for audio capture |
| `RUST_LOG` | info | Log level (debug, info, warn, error) |

## Docker Deployment

```dockerfile
FROM nvidia/cuda:12.0-runtime-ubuntu22.04

RUN apt-get update && apt-get install -y \
    libgstreamer1.0-0 \
    gstreamer1.0-plugins-base \
    gstreamer1.0-plugins-good \
    gstreamer1.0-plugins-bad \
    gstreamer1.0-nice

COPY dist/ /app/
WORKDIR /app
EXPOSE 8123
CMD ["./horizon-streamer"]
```

## License

MIT
