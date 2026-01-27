#!/bin/bash
# Build standalone binary - no GStreamer required

set -e

echo "Building Horizon Streamer (Standalone)"
echo "======================================"

# Check Rust
if ! command -v cargo &> /dev/null; then
    echo "Rust not found. Installing..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
fi

echo -e "\nRust version:"
cargo --version

# Backup and switch configs
echo -e "\nSwitching to standalone build configuration..."
cp Cargo.toml Cargo.toml.gstreamer 2>/dev/null || true
cp Cargo.toml.standalone Cargo.toml

# Switch main files
[ -f src/main.rs ] && mv src/main.rs src/main_gstreamer.rs
cp src/main_standalone.rs src/main.rs

echo -e "\nBuilding release binary..."
cargo build --release

if [ $? -eq 0 ]; then
    SIZE=$(ls -lh target/release/horizon-streamer | awk '{print $5}')
    echo -e "\n\033[32mBuild successful!\033[0m"
    echo -e "\033[36mBinary: target/release/horizon-streamer\033[0m"
    echo -e "\033[36mSize: $SIZE\033[0m"

    echo -e "\n\033[33mTo run:\033[0m"
    echo "  ./target/release/horizon-streamer"
    echo "  Then open http://localhost:8123"
else
    echo -e "\n\033[31mBuild failed!\033[0m"
fi

# Restore original config
echo -e "\nRestoring original configuration..."
cp Cargo.toml.gstreamer Cargo.toml 2>/dev/null || true
[ -f src/main_gstreamer.rs ] && mv src/main_gstreamer.rs src/main.rs
