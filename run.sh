#!/bin/bash
# Horizon Streamer - Run Script
# Sets up PATH for Homebrew, Rust, and Go before running

set -e

# Homebrew (macOS ARM)
if [ -f /opt/homebrew/bin/brew ]; then
    eval "$(/opt/homebrew/bin/brew shellenv)"
fi

# Homebrew (macOS Intel)
if [ -f /usr/local/bin/brew ]; then
    eval "$(/usr/local/bin/brew shellenv)"
fi

# Rust
if [ -f "$HOME/.cargo/env" ]; then
    source "$HOME/.cargo/env"
fi

# Go
if [ -d "$HOME/go/bin" ]; then
    export PATH="$HOME/go/bin:$PATH"
fi
if [ -d "/usr/local/go/bin" ]; then
    export PATH="/usr/local/go/bin:$PATH"
fi

# GStreamer plugin path (Homebrew)
if [ -d "/opt/homebrew/lib/gstreamer-1.0" ]; then
    export GST_PLUGIN_PATH="/opt/homebrew/lib/gstreamer-1.0"
fi

# Print environment info
echo "=== Environment ==="
echo "PATH includes:"
which cargo 2>/dev/null && echo "  ✓ Rust/Cargo"
which brew 2>/dev/null && echo "  ✓ Homebrew"
which go 2>/dev/null && echo "  ✓ Go"
which gst-inspect-1.0 2>/dev/null && echo "  ✓ GStreamer"
echo ""

# Build if needed
if [ ! -f ./target/release/horizon-streamer ] || [ "$1" == "--build" ]; then
    echo "=== Building ==="
    cargo build --release
    echo ""
fi

# Parse arguments
PORT="${PORT:-8123}"
MODE="${MODE:-websocket}"

while [[ $# -gt 0 ]]; do
    case $1 in
        --port|-p)
            PORT="$2"
            shift 2
            ;;
        --webrtc)
            MODE="webrtc"
            shift
            ;;
        --build)
            shift
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: ./run.sh [--port PORT] [--webrtc] [--build]"
            exit 1
            ;;
    esac
done

echo "=== Starting Horizon Streamer ==="
echo "Port: $PORT"
echo "Mode: $MODE"
echo "URL:  http://localhost:$PORT"
echo ""

export HORIZON_PORT="$PORT"
export HORIZON_MODE="$MODE"
exec ./target/release/horizon-streamer
