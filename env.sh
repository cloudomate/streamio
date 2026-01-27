#!/bin/bash
# Source this file to set up PATH for Homebrew, Rust, Go
# Usage: source env.sh

# Homebrew (macOS ARM - M1/M2/M3)
[ -f /opt/homebrew/bin/brew ] && eval "$(/opt/homebrew/bin/brew shellenv)"

# Homebrew (macOS Intel)
[ -f /usr/local/bin/brew ] && eval "$(/usr/local/bin/brew shellenv)"

# Homebrew (Linux)
[ -f /home/linuxbrew/.linuxbrew/bin/brew ] && eval "$(/home/linuxbrew/.linuxbrew/bin/brew shellenv)"

# Rust
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

# Go
[ -d "$HOME/go/bin" ] && export PATH="$HOME/go/bin:$PATH"
[ -d "/usr/local/go/bin" ] && export PATH="/usr/local/go/bin:$PATH"

# GStreamer plugins (Homebrew)
[ -d "/opt/homebrew/lib/gstreamer-1.0" ] && export GST_PLUGIN_PATH="/opt/homebrew/lib/gstreamer-1.0"
[ -d "/usr/local/lib/gstreamer-1.0" ] && export GST_PLUGIN_PATH="/usr/local/lib/gstreamer-1.0"
