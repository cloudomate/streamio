#!/bin/bash
# Cross-compile streamio (backend agent) for Windows using Docker.
# Produces a self-contained dist-windows/ with the exe + all GStreamer DLLs.

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
    local dist_dir="${REPO_ROOT}/dist-windows-${arch}"

    echo ""
    echo "=== Building Windows ${arch} (${target}) ==="

    # Build toolchain image (cached after first run)
    docker build \
        --file "${REPO_ROOT}/docker/${dockerfile}" \
        --tag "${image}" \
        "${REPO_ROOT}"

    # Build + bundle inside the container
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

            # 1. Compile
            # 1. Compile
            cargo build --release --target ${target} -p streamio

            # 2. Bundle: copy exe + all required GStreamer DLLs
            DIST=/workspace/dist-windows-${arch}
            rm -rf \"\$DIST\" && mkdir -p \"\$DIST/lib/gstreamer-1.0\"

            cp target/${target}/release/streamio.exe \"\$DIST/\"

            GST_BIN=\"\${GST_ROOT}/bin\"
            GST_PLUGINS=\"\${GST_ROOT}/lib/gstreamer-1.0\"

            echo 'Resolving DLL dependencies...'
            # Iteratively copy DLLs referenced by the exe and already-copied DLLs
            ALL_BINS=(\"\$DIST/streamio.exe\")
            for round in \$(seq 1 8); do
                NEW=()
                for bin in \"\${ALL_BINS[@]}\"; do
                    for dll in \$(x86_64-w64-mingw32-objdump -p \"\$bin\" 2>/dev/null \
                                  | grep 'DLL Name:' | awk '{print \$3}'); do
                        case \"\$dll\" in
                            KERNEL32*|USER32*|GDI32*|ADVAPI32*|SHELL32*|ole32*|OLEAUT32*|\
                            WS2_32*|ntdll*|msvcrt*|VCRUNTIME*|ucrtbase*|api-ms-*|ext-ms-*|\
                            bcrypt*|MSWSOCK*|secur32*|IPHLPAPI*|USERENV*|dbghelp*|IMM32*|\
                            SETUPAPI*|CFGMGR32*|dwmapi*|d3d11*|d3d12*|dxgi*|DNSAPI*|\
                            VERSION*|WINMM*|COMCTL32*|COMDLG32*|WTSAPI32*|PSAPI*|RPCRT4*|\
                            Normaliz*)
                                continue ;;
                        esac
                        dest=\"\$DIST/\$dll\"
                        [ -f \"\$dest\" ] && continue
                        src=\"\$GST_BIN/\$dll\"
                        if [ -f \"\$src\" ]; then
                            cp \"\$src\" \"\$dest\"
                            NEW+=(\"\$dest\")
                            echo \"  + \$dll\"
                        fi
                    done
                done
                [ \${#NEW[@]} -eq 0 ] && break
                ALL_BINS=(\"\${NEW[@]}\")
            done

            # Copy required GStreamer plugins
            for plugin in \
                libgstcoreelements libgstapp libgstvideoconvertscale \
                libgstvideoparsersbad libgsttypefindfunctions \
                libgstrtp libgstrtpmanager libgstwebrtc libgstnice \
                libgstdtls libgstsrtp libgstsctp libgstsdpelem \
                libgstx264 libgstopus libgstaudioconvert libgstaudioresample \
                gstd3d11 gstwasapi2 gstmediafoundation; do
                found=\$(find \"\$GST_PLUGINS\" -name \"\${plugin}.dll\" -type f 2>/dev/null | head -1)
                [ -n \"\$found\" ] && cp \"\$found\" \"\$DIST/lib/gstreamer-1.0/\" && echo \"  plugin: \$(basename \$found)\"
            done

            echo ''
            echo 'Bundle size:' \$(du -sh \"\$DIST\" | cut -f1)
        "

    local exe="${dist_dir}/streamio.exe"
    if [ -f "$exe" ]; then
        echo ""
        echo "  => Bundle: ${dist_dir}/"
        ls "${dist_dir}/" | head -10
        # Create archive (tar.gz — zip may not be available on all build hosts)
        (cd "${REPO_ROOT}" && tar -czf "streamio-windows-${arch}.tar.gz" "dist-windows-${arch}/")
        echo "  => Archive: streamio-windows-${arch}.tar.gz ($(du -sh "streamio-windows-${arch}.tar.gz" | cut -f1))"
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
    echo ""
    echo "Note: No GStreamer ARM64 Windows packages exist."
    echo "      Producing x86_64 binary (runs on ARM64 Windows via emulation)."
    build_target "arm64" \
        "x86_64-pc-windows-gnu" \
        "streamio-win-arm64" \
        "Dockerfile.windows-arm64"
fi

echo ""
echo "=== Windows build complete ==="
echo "Bundles:"
$BUILD_X86   && echo "  streamio-windows-x86_64.tar.gz  — extract and run streamio.exe"
$BUILD_ARM64 && echo "  streamio-windows-arm64.tar.gz   — x86_64 binary, runs on ARM64 Windows via emulation"
