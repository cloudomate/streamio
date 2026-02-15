#!/bin/bash
# Bundle horizon-streamer with all GStreamer dependencies for self-contained distribution.
# Supports macOS (Homebrew), Linux (apt/system GStreamer), and Windows (MSYS2/Git Bash).
# The resulting dist/ directory can be copied to any machine of the same OS/arch.

set -e

BINARY_NAME="horizon-streamer"
DIST_DIR="dist"

OS="$(uname -s)"
case "$OS" in
    MINGW*|MSYS*|CYGWIN*) OS="Windows" ;;
    Darwin)               OS="Darwin" ;;
    *)                    OS="Linux" ;;
esac

if [[ "$OS" == "Windows" ]]; then
    BINARY="target/release/${BINARY_NAME}.exe"
else
    BINARY="target/release/$BINARY_NAME"
fi

# GStreamer plugins required by screen_capture.rs pipelines
REQUIRED_PLUGINS=(
    # Core pipeline elements
    libgstcoreelements
    libgstapp
    libgstvideoconvertscale
    libgstvideoparsersbad
    libgsttypefindfunctions
    # RTP / WebRTC
    libgstrtp
    libgstrtpmanager
    libgstwebrtc
    libgstnice
    libgstdtls
    libgstsrtp
    libgstsctp
    libgstsdpelem
    # Video encoding
    libgstx264
    # Audio
    libgstopus
    libgstaudioconvert
    libgstaudioresample
)

# Platform-specific plugins
if [[ "$OS" == "Darwin" ]]; then
    REQUIRED_PLUGINS+=(libgstapplemedia libgstosxaudio)
elif [[ "$OS" == "Windows" ]]; then
    REQUIRED_PLUGINS+=(gstd3d11 gstwasapi2 gstd3d12 gstnvcodec gstqsv gstmediafoundation)
elif [[ "$OS" == "Linux" ]]; then
    REQUIRED_PLUGINS+=(libgstpulseaudio libgstximagesrc libgstpipewire libgstv4l2 libgstvideo4linux2 libgstnvcodec libgstvaapi)
fi

echo "=== Building release binary ==="
cargo build --release

if [ ! -f "$BINARY" ]; then
    echo "ERROR: Binary not found at $BINARY"
    exit 1
fi

echo ""
echo "=== Creating distribution bundle ==="
rm -rf "$DIST_DIR"

if [[ "$OS" == "Windows" ]]; then
    # Windows: flat layout — DLLs and plugins all next to the exe
    mkdir -p "$DIST_DIR/lib/gstreamer-1.0"
else
    mkdir -p "$DIST_DIR/lib/gstreamer-1.0"
    mkdir -p "$DIST_DIR/libexec"
fi

cp "$BINARY" "$DIST_DIR/"

# --- Discover GStreamer paths ---

if [[ "$OS" == "Darwin" ]]; then
    # macOS: find GStreamer via otool on the binary
    GST_LIB_DIR=$(otool -L "$BINARY" | grep libgstreamer | awk '{print $1}' | xargs dirname)
    if [ -z "$GST_LIB_DIR" ]; then
        echo "ERROR: Cannot find GStreamer library path from binary"
        exit 1
    fi
    GST_PLUGIN_DIR="$GST_LIB_DIR/gstreamer-1.0"

    # Resolve real path (Homebrew symlinks opt/ -> Cellar/)
    GST_REAL_DIR=$(cd "$GST_LIB_DIR/.." && pwd -P)

    # Find gst-plugin-scanner (check both the real and symlinked paths)
    GST_SCANNER=$(find "$GST_REAL_DIR" -name gst-plugin-scanner -type f 2>/dev/null | head -1)
    if [ -z "$GST_SCANNER" ]; then
        GST_SCANNER=$(find "$(dirname "$GST_LIB_DIR")" -name gst-plugin-scanner -type f 2>/dev/null | head -1)
    fi

elif [[ "$OS" == "Windows" ]]; then
    # Windows: find GStreamer from GSTREAMER_1_0_ROOT or standard install paths
    if [ -n "$GSTREAMER_1_0_ROOT_MSVC_X86_64" ]; then
        GST_ROOT="$GSTREAMER_1_0_ROOT_MSVC_X86_64"
    elif [ -n "$GSTREAMER_1_0_ROOT_X86_64" ]; then
        GST_ROOT="$GSTREAMER_1_0_ROOT_X86_64"
    elif [ -d "/c/gstreamer/1.0/msvc_x86_64" ]; then
        GST_ROOT="/c/gstreamer/1.0/msvc_x86_64"
    elif [ -d "/c/gstreamer/1.0/x86_64" ]; then
        GST_ROOT="/c/gstreamer/1.0/x86_64"
    else
        echo "ERROR: Cannot find GStreamer installation. Set GSTREAMER_1_0_ROOT_MSVC_X86_64 or install to C:\\gstreamer"
        exit 1
    fi
    GST_LIB_DIR="$GST_ROOT/lib"
    GST_BIN_DIR="$GST_ROOT/bin"
    GST_PLUGIN_DIR="$GST_LIB_DIR/gstreamer-1.0"
    GST_SCANNER="$GST_ROOT/libexec/gstreamer-1.0/gst-plugin-scanner.exe"
    [ ! -f "$GST_SCANNER" ] && GST_SCANNER=""

else
    # Linux: check standard paths
    for dir in /usr/lib/x86_64-linux-gnu /usr/lib/aarch64-linux-gnu /usr/lib64 /usr/lib; do
        if [ -d "$dir/gstreamer-1.0" ]; then
            GST_LIB_DIR="$dir"
            GST_PLUGIN_DIR="$dir/gstreamer-1.0"
            break
        fi
    done
    if [ -z "$GST_LIB_DIR" ]; then
        echo "ERROR: Cannot find GStreamer libraries"
        exit 1
    fi

    GST_SCANNER=$(find /usr/libexec /usr/lib -name gst-plugin-scanner -type f 2>/dev/null | head -1)
fi

echo "GStreamer libs:    $GST_LIB_DIR"
echo "GStreamer plugins: $GST_PLUGIN_DIR"
echo "Plugin scanner:    ${GST_SCANNER:-not found}"

# --- Copy plugin scanner ---

if [ -n "$GST_SCANNER" ]; then
    if [[ "$OS" == "Windows" ]]; then
        cp "$GST_SCANNER" "$DIST_DIR/"
    else
        cp "$GST_SCANNER" "$DIST_DIR/libexec/"
    fi
    echo "Copied gst-plugin-scanner"
fi

# --- Copy required plugins ---

echo ""
echo "Copying plugins..."
PLUGIN_COUNT=0
for plugin in "${REQUIRED_PLUGINS[@]}"; do
    # Match plugin name with any extension (follow symlinks with -L)
    found=$(find -L "$GST_PLUGIN_DIR" -name "${plugin}.*" -type f 2>/dev/null | head -1)
    if [ -n "$found" ]; then
        cp -L "$found" "$DIST_DIR/lib/gstreamer-1.0/"
        echo "  + $(basename "$found")"
        PLUGIN_COUNT=$((PLUGIN_COUNT + 1))
    fi
done
echo "Copied $PLUGIN_COUNT plugins"

# --- Recursively discover and copy all shared library dependencies ---

echo ""
echo "Resolving shared library dependencies..."

# Collect all binaries we need to trace
ALL_BINS=("$DIST_DIR/$(basename "$BINARY")")
for f in "$DIST_DIR/lib/gstreamer-1.0/"*; do
    [ -f "$f" ] && ALL_BINS+=("$f")
done
[ -f "$DIST_DIR/libexec/gst-plugin-scanner" ] && ALL_BINS+=("$DIST_DIR/libexec/gst-plugin-scanner")
[ -f "$DIST_DIR/gst-plugin-scanner.exe" ] && ALL_BINS+=("$DIST_DIR/gst-plugin-scanner.exe")

# Iteratively resolve deps until no new ones are found
COPIED_LIBS=()
MAX_ROUNDS=10
for round in $(seq 1 $MAX_ROUNDS); do
    NEW_DEPS=()

    for bin in "${ALL_BINS[@]}"; do
        if [[ "$OS" == "Darwin" ]]; then
            deps=$(otool -L "$bin" 2>/dev/null | tail -n +2 | awk '{print $1}' | grep -v "^/usr/lib" | grep -v "^/System" | grep -v "^@")
        elif [[ "$OS" == "Windows" ]]; then
            # Use objdump to list DLL imports, then resolve from GStreamer bin dir
            deps=""
            for dll in $(objdump -p "$bin" 2>/dev/null | grep "DLL Name:" | awk '{print $3}'); do
                # Skip Windows system DLLs
                case "$dll" in
                    KERNEL32.dll|USER32.dll|GDI32.dll|ADVAPI32.dll|SHELL32.dll|ole32.dll|OLEAUT32.dll|\
                    WS2_32.dll|WSOCK32.dll|CRYPT32.dll|WLDAP32.dll|ntdll.dll|msvcrt.dll|VCRUNTIME*.dll|\
                    ucrtbase.dll|api-ms-*|ext-ms-*|bcrypt.dll|MSWSOCK.dll|secur32.dll|IPHLPAPI.DLL|\
                    USERENV.dll|dbghelp.dll|IMM32.dll|SETUPAPI.dll|CFGMGR32.dll|dwmapi.dll|d3d11.dll|\
                    d3d12.dll|dxgi.dll|d3dcompiler_*.dll|DNSAPI.dll|VERSION.dll|WINMM.dll|COMCTL32.dll|\
                    COMDLG32.dll|WTSAPI32.dll|PSAPI.DLL|RPCRT4.dll|Normaliz.dll|Secur32.dll)
                        continue ;;
                esac
                # Look for DLL in GStreamer bin dir
                if [ -f "$GST_BIN_DIR/$dll" ]; then
                    deps="$deps $GST_BIN_DIR/$dll"
                fi
            done
        else
            deps=$(ldd "$bin" 2>/dev/null | grep "=>" | awk '{print $3}' | grep -v "^$" \
                | grep -Ev '/(libc|libm|libdl|librt|libpthread|libutil|libresolv|libnsl|libcrypt|ld-linux|libgcc_s|libstdc\+\+|linux-vdso)\.so' || true)
        fi

        for dep in $deps; do
            base=$(basename "$dep")
            # Skip if already in dist/lib
            if [ -f "$DIST_DIR/lib/$base" ]; then
                continue
            fi
            # Skip if it's the binary itself
            if [[ "$base" == "${BINARY_NAME}"* ]]; then
                continue
            fi
            # Copy the dep (resolve symlinks to get the real file)
            real_dep=$(realpath "$dep" 2>/dev/null || readlink -f "$dep" 2>/dev/null || echo "$dep")
            if [ -f "$real_dep" ]; then
                cp "$real_dep" "$DIST_DIR/lib/$base"
                COPIED_LIBS+=("$base")
                NEW_DEPS+=("$DIST_DIR/lib/$base")
                echo "  + $base"
            fi
        done
    done

    if [ ${#NEW_DEPS[@]} -eq 0 ]; then
        break
    fi

    # Add newly copied libs to the scan list for next round
    ALL_BINS=("${NEW_DEPS[@]}")
done

echo "Copied ${#COPIED_LIBS[@]} shared libraries"

# --- Fix library paths ---

echo ""
if [[ "$OS" == "Windows" ]]; then
    echo "Windows: moving DLLs next to executable..."
    # Windows finds DLLs in the exe directory — flatten lib/ into dist/
    for f in "$DIST_DIR/lib/"*.dll; do
        [ -f "$f" ] && mv "$f" "$DIST_DIR/"
    done
    for f in "$DIST_DIR/lib/gstreamer-1.0/"*.dll; do
        [ -f "$f" ] && mv "$f" "$DIST_DIR/lib/gstreamer-1.0/"
    done
    # Remove empty lib dir (keep lib/gstreamer-1.0/ — setup_bundled_gstreamer() expects it)
    rmdir "$DIST_DIR/libexec" 2>/dev/null || true

elif [[ "$OS" == "Darwin" ]]; then
    echo "Rewriting dylib paths for macOS..."

    # Collect all dylibs and binaries to fix
    FIX_FILES=("$DIST_DIR/$BINARY_NAME")
    [ -f "$DIST_DIR/libexec/gst-plugin-scanner" ] && FIX_FILES+=("$DIST_DIR/libexec/gst-plugin-scanner")
    for f in "$DIST_DIR/lib/"*.dylib "$DIST_DIR/lib/gstreamer-1.0/"*.dylib; do
        [ -f "$f" ] && FIX_FILES+=("$f")
    done

    for target in "${FIX_FILES[@]}"; do
        # Get all non-system dependencies
        deps=$(otool -L "$target" 2>/dev/null | tail -n +2 | awk '{print $1}' | grep -v "^/usr/lib" | grep -v "^/System" | grep -v "^@")

        for dep in $deps; do
            base=$(basename "$dep")

            # Determine the correct path rewrite.
            # Use @loader_path for dylib-to-dylib references so they resolve
            # correctly regardless of which executable loaded them (main binary
            # vs gst-plugin-scanner in libexec/).
            if [ -f "$DIST_DIR/lib/$base" ]; then
                if [[ "$target" == *"/gstreamer-1.0/"* ]]; then
                    new_path="@loader_path/../$base"
                elif [[ "$target" == *"/libexec/"* ]]; then
                    new_path="@executable_path/../lib/$base"
                elif [[ "$target" == *.dylib ]]; then
                    new_path="@loader_path/$base"
                else
                    new_path="@executable_path/lib/$base"
                fi
            elif [ -f "$DIST_DIR/lib/gstreamer-1.0/$base" ]; then
                if [[ "$target" == *"/libexec/"* ]]; then
                    new_path="@executable_path/../lib/gstreamer-1.0/$base"
                elif [[ "$target" == *.dylib && "$target" != *"/gstreamer-1.0/"* ]]; then
                    new_path="@loader_path/gstreamer-1.0/$base"
                else
                    new_path="@executable_path/lib/gstreamer-1.0/$base"
                fi
            else
                continue
            fi

            install_name_tool -change "$dep" "$new_path" "$target" 2>/dev/null || true
        done

        # Fix the install name (id) for libraries
        if [[ "$target" == *.dylib ]]; then
            base=$(basename "$target")
            if [[ "$target" == *"/gstreamer-1.0/"* ]]; then
                install_name_tool -id "@loader_path/../gstreamer-1.0/$base" "$target" 2>/dev/null || true
            else
                install_name_tool -id "@loader_path/$base" "$target" 2>/dev/null || true
            fi
        fi
    done

    # Re-codesign everything (required on Apple Silicon)
    echo "Re-codesigning..."
    for f in "${FIX_FILES[@]}"; do
        codesign --force --sign - "$f" 2>/dev/null || true
    done

elif [[ "$OS" == "Linux" ]]; then
    echo "Setting rpath for Linux..."

    if ! command -v patchelf &>/dev/null; then
        echo "WARNING: patchelf not found. Install it with: sudo apt install patchelf"
        echo "Skipping rpath fixup — binary will need LD_LIBRARY_PATH set manually."
    else
        patchelf --set-rpath '$ORIGIN/lib' "$DIST_DIR/$BINARY_NAME"
        [ -f "$DIST_DIR/libexec/gst-plugin-scanner" ] && patchelf --set-rpath '$ORIGIN/../lib' "$DIST_DIR/libexec/gst-plugin-scanner"
        for f in "$DIST_DIR/lib/"*.so* "$DIST_DIR/lib/gstreamer-1.0/"*.so*; do
            [ -f "$f" ] && patchelf --set-rpath '$ORIGIN' "$f" 2>/dev/null || true
        done
    fi
fi

# --- Summary ---

echo ""
echo "=== Bundle complete ==="
TOTAL_SIZE=$(du -sh "$DIST_DIR" | awk '{print $1}')
echo "Location: $DIST_DIR/"
echo "Size:     $TOTAL_SIZE"
echo ""
if [[ "$OS" == "Windows" ]]; then
    EXE_NAME="${BINARY_NAME}.exe"
    DLL_COUNT=$(ls "$DIST_DIR/"*.dll 2>/dev/null | wc -l | tr -d ' ')
    echo "Contents:"
    echo "  $DIST_DIR/$EXE_NAME"
    echo "  $DIST_DIR/*.dll             ($DLL_COUNT shared libraries)"
    echo "  $DIST_DIR/lib/gstreamer-1.0/ ($PLUGIN_COUNT plugins)"
    echo ""
    echo "To run: $DIST_DIR\\$EXE_NAME"
else
    echo "Contents:"
    echo "  $DIST_DIR/$BINARY_NAME"
    echo "  $DIST_DIR/lib/              ($(ls "$DIST_DIR/lib/" | wc -l | tr -d ' ') shared libraries)"
    echo "  $DIST_DIR/lib/gstreamer-1.0/ ($PLUGIN_COUNT plugins)"
    echo "  $DIST_DIR/libexec/          (plugin scanner)"
    echo ""
    echo "To run: ./$DIST_DIR/$BINARY_NAME"
fi
