#!/bin/bash
# Bundle horizon-streamer with all GStreamer dependencies for self-contained distribution.
# Supports macOS (Homebrew) and Linux (apt/system GStreamer).
# The resulting dist/ directory can be copied to any machine of the same OS/arch.

set -e

BINARY_NAME="horizon-streamer"
DIST_DIR="dist"
BINARY="target/release/$BINARY_NAME"

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
if [[ "$(uname)" == "Darwin" ]]; then
    REQUIRED_PLUGINS+=(libgstapplemedia libgstosxaudio)
elif [[ "$(uname)" == "Linux" ]]; then
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
mkdir -p "$DIST_DIR/lib/gstreamer-1.0"
mkdir -p "$DIST_DIR/libexec"

cp "$BINARY" "$DIST_DIR/"

# --- Discover GStreamer paths ---

if [[ "$(uname)" == "Darwin" ]]; then
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
    cp "$GST_SCANNER" "$DIST_DIR/libexec/"
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
ALL_BINS=("$DIST_DIR/$BINARY_NAME")
for f in "$DIST_DIR/lib/gstreamer-1.0/"*; do
    [ -f "$f" ] && ALL_BINS+=("$f")
done
[ -f "$DIST_DIR/libexec/gst-plugin-scanner" ] && ALL_BINS+=("$DIST_DIR/libexec/gst-plugin-scanner")

# Iteratively resolve deps until no new ones are found
COPIED_LIBS=()
MAX_ROUNDS=10
for round in $(seq 1 $MAX_ROUNDS); do
    NEW_DEPS=()

    for bin in "${ALL_BINS[@]}"; do
        if [[ "$(uname)" == "Darwin" ]]; then
            deps=$(otool -L "$bin" 2>/dev/null | tail -n +2 | awk '{print $1}' | grep -v "^/usr/lib" | grep -v "^/System" | grep -v "@")
        else
            deps=$(ldd "$bin" 2>/dev/null | grep "=>" | awk '{print $3}' | grep -v "^/lib" | grep -v "^$")
        fi

        for dep in $deps; do
            base=$(basename "$dep")
            # Skip if already in dist/lib
            if [ -f "$DIST_DIR/lib/$base" ]; then
                continue
            fi
            # Skip if it's the binary itself
            if [ "$base" = "$BINARY_NAME" ]; then
                continue
            fi
            # Copy the dep
            if [ -f "$dep" ]; then
                cp "$dep" "$DIST_DIR/lib/"
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
if [[ "$(uname)" == "Darwin" ]]; then
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

            # Determine the correct @executable_path-relative path
            if [ -f "$DIST_DIR/lib/$base" ]; then
                if [[ "$target" == *"/gstreamer-1.0/"* ]]; then
                    new_path="@loader_path/../$base"
                elif [[ "$target" == *"/libexec/"* ]]; then
                    new_path="@executable_path/lib/$base"
                else
                    new_path="@executable_path/lib/$base"
                fi
            elif [ -f "$DIST_DIR/lib/gstreamer-1.0/$base" ]; then
                new_path="@executable_path/lib/gstreamer-1.0/$base"
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
                install_name_tool -id "@executable_path/lib/$base" "$target" 2>/dev/null || true
            fi
        fi
    done

    # Re-codesign everything (required on Apple Silicon)
    echo "Re-codesigning..."
    for f in "${FIX_FILES[@]}"; do
        codesign --force --sign - "$f" 2>/dev/null || true
    done

elif [[ "$(uname)" == "Linux" ]]; then
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
echo "Contents:"
echo "  $DIST_DIR/$BINARY_NAME"
echo "  $DIST_DIR/lib/              ($(ls "$DIST_DIR/lib/" | wc -l | tr -d ' ') shared libraries)"
echo "  $DIST_DIR/lib/gstreamer-1.0/ ($PLUGIN_COUNT plugins)"
echo "  $DIST_DIR/libexec/          (plugin scanner)"
echo ""
echo "To run: ./$DIST_DIR/$BINARY_NAME"
