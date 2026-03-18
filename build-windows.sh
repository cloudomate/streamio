#!/bin/bash
# Cross-compile streamio (backend agent) for Windows using Docker.
# Produces:
#   target/x86_64-pc-windows-gnu/release/streamio.exe
#   target/aarch64-pc-windows-gnullvm/release/streamio.exe

set -e

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
TARGET_DIR="${REPO_ROOT}/target"

# Which targets to build (default: both)
BUILD_X86=true
BUILD_ARM64=true
for arg in "$@"; do
    case "$arg" in
        --x86-only)  BUILD_ARM64=false ;;
        --arm64-only) BUILD_X86=false ;;
    esac
done

build_target() {
    local arch="$1"       # x86_64 or arm64
    local target="$2"     # Rust target triple
    local image="$3"      # Docker image tag
    local dockerfile="$4" # Path to Dockerfile

    echo ""
    echo "=== Building Windows ${arch} (${target}) ==="

    # Build toolchain image (cached after first run)
    docker build \
        --file "${REPO_ROOT}/docker/${dockerfile}" \
        --tag "${image}" \
        "${REPO_ROOT}"

    # Run cargo build inside the container, mounting source + target cache
    docker run --rm \
        -v "${REPO_ROOT}:/workspace" \
        -v "${TARGET_DIR}:/workspace/target" \
        --workdir /workspace \
        "${image}" \
        /bin/bash -c "
            set -e
            . /etc/gst-env.sh
            export PKG_CONFIG_ALLOW_CROSS=1
            export PKG_CONFIG_PATH
            cargo build --release --target ${target} -p streamio
        "

    local exe="${TARGET_DIR}/${target}/release/streamio.exe"
    if [ -f "$exe" ]; then
        echo "  => $(du -sh "$exe" | cut -f1)  $exe"
    else
        echo "ERROR: binary not found at $exe"
        exit 1
    fi
}

if $BUILD_X86; then
    build_target "x86_64" \
        "x86_64-pc-windows-gnu" \
        "streamio-win-x86_64" \
        "Dockerfile.windows-x86_64"
fi

if $BUILD_ARM64; then
    # GStreamer has no official Windows ARM64 packages. Windows ARM64 devices
    # run x86_64 binaries natively via hardware emulation, so we produce an
    # x86_64 binary and label it for ARM64 deployment.
    echo ""
    echo "Note: No GStreamer ARM64 Windows packages exist."
    echo "      Producing x86_64 binary (runs on ARM64 Windows via emulation)."
    build_target "arm64-compat" \
        "x86_64-pc-windows-gnu" \
        "streamio-win-arm64" \
        "Dockerfile.windows-arm64"
fi

echo ""
echo "=== Windows build complete ==="
echo "Binaries:"
$BUILD_X86   && echo "  target/x86_64-pc-windows-gnu/release/streamio.exe  (native x86_64)"
$BUILD_ARM64 && echo "  target/x86_64-pc-windows-gnu/release/streamio.exe  (x86_64, runs on ARM64 Windows via emulation)"
echo ""
echo "Note: these are bare executables. To create a self-contained bundle"
echo "with GStreamer DLLs, run bundle.sh on a Windows machine or GitHub Actions."
