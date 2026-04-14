#!/usr/bin/env bash
#
# Cross-compile CPython for wasm32-wasi, producing:
#   - libpython3.XX.a     (static library for linking into the component)
#   - python_stdlib/       (stdlib modules for runtime use)
#
# This script is intentionally separate from the Pyodide build. Pyodide
# embeds its own CPython fork with Emscripten; this builds stock CPython
# with wasi-sdk for the WASI P2 component target.
#
# Environment:
#   SKIP_PYTHON_WASI=1   Skip entirely (for devs without wasi-sdk interest)
#   PYTHON_WASI_CLEAN=1  Force a full rebuild
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=versions.env
source "$SCRIPT_DIR/versions.env"

BUILD_DIR="$SCRIPT_DIR/build"
WASI_SDK_DIR="$SCRIPT_DIR/wasi-sdk"
CPYTHON_SRC="$BUILD_DIR/cpython-src"
PYODIDE_CPYTHON_FALLBACK_GLOB="$REPO_ROOT/tools/pyodide/pyodide-src/cpython/build/Python-${CPYTHON_MAJOR_MINOR}.*"
HOST_BUILD="$BUILD_DIR/host-build"
WASI_BUILD="$BUILD_DIR/wasi-build"
OUTPUT_DIR="$SCRIPT_DIR/output"
WASI_TARGET_TRIPLE="wasm32-wasip2"
HOST_PYTHON_UNIX="$HOST_BUILD/python"
HOST_PYTHON_EXE="$HOST_BUILD/python.exe"

resolve_host_python() {
    if [ -f "$HOST_PYTHON_UNIX" ] && [ -x "$HOST_PYTHON_UNIX" ]; then
        printf '%s\n' "$HOST_PYTHON_UNIX"
        return
    fi
    if [ -f "$HOST_PYTHON_EXE" ] && [ -x "$HOST_PYTHON_EXE" ]; then
        printf '%s\n' "$HOST_PYTHON_EXE"
        return
    fi
    printf '%s\n' "$HOST_PYTHON_UNIX"
}

resolve_cpython_source() {
    if [ -f "$CPYTHON_SRC/configure" ]; then
        printf '%s\n' "$CPYTHON_SRC"
        return
    fi

    local fallback
    fallback=$(echo $PYODIDE_CPYTHON_FALLBACK_GLOB)
    if [ -f "$fallback/configure" ]; then
        printf '%s\n' "$fallback"
        return
    fi

    printf '%s\n' "$CPYTHON_SRC"
}

stage_local_cpython_source() {
    local source_dir="$1"
    if [ "$source_dir" = "$CPYTHON_SRC" ]; then
        return
    fi

    echo "Staging local CPython source into $CPYTHON_SRC"
    rm -rf "$CPYTHON_SRC"
    mkdir -p "$CPYTHON_SRC"
    rsync -a \
        --exclude='.git/' \
        "$source_dir/" "$CPYTHON_SRC/"
    (cd "$CPYTHON_SRC" && make distclean >/dev/null 2>&1 || true)
}

HOST_PYTHON="$(resolve_host_python)"

if [ "${SKIP_PYTHON_WASI:-0}" = "1" ]; then
    echo "SKIP_PYTHON_WASI=1 — skipping"
    exit 0
fi

if [ "${PYTHON_WASI_CLEAN:-0}" = "1" ] && [ -d "$BUILD_DIR" ]; then
    echo "PYTHON_WASI_CLEAN=1 — removing build dir"
    rm -rf "$BUILD_DIR" "$OUTPUT_DIR"
fi

echo "=== CPython ${CPYTHON_VERSION} WASI build ==="
echo "wasi-sdk:  ${WASI_SDK_VERSION}"
echo "Platform:  $(uname -ms)"

# ── Download wasi-sdk ─────────────────────────────────────────
install_wasi_sdk() {
    if [ -x "$WASI_SDK_DIR/bin/clang" ]; then
        echo "wasi-sdk already present at $WASI_SDK_DIR"
        return
    fi

    local os arch tarball url
    os="$(uname -s | tr '[:upper:]' '[:lower:]')"
    arch="$(uname -m)"

    case "${os}-${arch}" in
        linux-x86_64)  tarball="wasi-sdk-${WASI_SDK_VERSION}.0-x86_64-linux.tar.gz" ;;
        linux-aarch64) tarball="wasi-sdk-${WASI_SDK_VERSION}.0-arm64-linux.tar.gz" ;;
        darwin-arm64)  tarball="wasi-sdk-${WASI_SDK_VERSION}.0-arm64-macos.tar.gz" ;;
        darwin-x86_64) tarball="wasi-sdk-${WASI_SDK_VERSION}.0-x86_64-macos.tar.gz" ;;
        *)
            echo "ERROR: unsupported platform ${os}-${arch}"
            exit 1
            ;;
    esac

    url="https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-${WASI_SDK_VERSION}/${tarball}"
    echo "Downloading wasi-sdk from ${url}..."
    mkdir -p "$WASI_SDK_DIR"
    curl -fsSL "$url" | tar xz --strip-components=1 -C "$WASI_SDK_DIR"

    if [ ! -x "$WASI_SDK_DIR/bin/clang" ]; then
        echo "ERROR: wasi-sdk clang not found after download"
        exit 1
    fi
    echo "wasi-sdk installed: $("$WASI_SDK_DIR/bin/clang" --version | head -1)"
}

install_wasi_sdk

export WASI_SDK_PATH="$WASI_SDK_DIR"
export WASI_SYSROOT="$WASI_SDK_DIR/share/wasi-sysroot"
export PKG_CONFIG_PATH=""
export PKG_CONFIG_LIBDIR="$WASI_SYSROOT/lib/pkgconfig:$WASI_SYSROOT/share/pkgconfig"
export PKG_CONFIG_SYSROOT_DIR="$WASI_SYSROOT"

# ── Download CPython source ──────────���────────────────────────
LOCAL_CPYTHON_SRC="$(resolve_cpython_source)"
if [ "$LOCAL_CPYTHON_SRC" != "$CPYTHON_SRC" ]; then
    echo "Using existing local CPython source at $LOCAL_CPYTHON_SRC"
    stage_local_cpython_source "$LOCAL_CPYTHON_SRC"
fi

if [ ! -f "$CPYTHON_SRC/configure" ]; then
    echo "Downloading CPython ${CPYTHON_VERSION} source..."
    mkdir -p "$CPYTHON_SRC"
    curl -fsSL "https://www.python.org/ftp/python/${CPYTHON_VERSION}/Python-${CPYTHON_VERSION}.tar.xz" \
        | tar xJ --strip-components=1 -C "$CPYTHON_SRC"
    echo "CPython source ready."
else
    echo "CPython source already present."
fi

# ── Build host Python (needed for cross-compilation) ──────────
# CPython's cross-compilation needs a host Python of the exact same
# version to generate frozen modules, run setup scripts, etc.
if [ ! -f "$HOST_PYTHON" ] || [ ! -x "$HOST_PYTHON" ]; then
    echo "Building host Python ${CPYTHON_VERSION}..."
    mkdir -p "$HOST_BUILD"
    cd "$HOST_BUILD"
    "$CPYTHON_SRC/configure" \
        --prefix="$HOST_BUILD/install" \
        --disable-test-modules \
        --without-ensurepip \
        2>&1 | tail -5
    make -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu)" 2>&1 | tail -5
    HOST_PYTHON="$(resolve_host_python)"
    echo "Host Python built: $($HOST_PYTHON --version)"
else
    echo "Host Python already built: $($HOST_PYTHON --version)"
fi

# ── Cross-compile CPython for wasm32-wasi ─────────────────────
# We use CPython's built-in Tools/wasm/wasi.py if available (3.13+),
# falling back to manual configure for older versions.
if [ ! -f "$WASI_BUILD/libpython${CPYTHON_MAJOR_MINOR}.a" ]; then
    echo "Cross-compiling CPython for wasm32-wasi..."
    mkdir -p "$WASI_BUILD"
    cd "$WASI_BUILD"

    CONFIG_SITE="$CPYTHON_SRC/Tools/wasm/config.site-wasm32-wasi" \
    CC="$WASI_SDK_DIR/bin/clang" \
    CPP="$WASI_SDK_DIR/bin/clang-cpp" \
    AR="$WASI_SDK_DIR/bin/llvm-ar" \
    RANLIB="$WASI_SDK_DIR/bin/llvm-ranlib" \
    DYNLOADFILE="dynload_stub.o" \
    ac_cv_func_dlopen=no \
    ac_cv_func_memfd_create=no \
    ac_cv_header_dlfcn_h=no \
    ac_cv_lib_dl_dlopen=no \
    CFLAGS="--target=$WASI_TARGET_TRIPLE --sysroot=$WASI_SYSROOT -D_WASI_EMULATED_SIGNAL -D_WASI_EMULATED_PROCESS_CLOCKS -D_WASI_EMULATED_MMAN -D_WASI_EMULATED_GETPID -DRTLD_LAZY=0 -DRTLD_NOW=0 -DRTLD_GLOBAL=0 -DRTLD_LOCAL=0 -DRTLD_NODELETE=0 -DRTLD_NOLOAD=0 -matomics -mbulk-memory" \
    LDFLAGS="--target=$WASI_TARGET_TRIPLE --sysroot=$WASI_SYSROOT -lwasi-emulated-signal -lwasi-emulated-process-clocks -lwasi-emulated-mman -lwasi-emulated-getpid" \
    "$CPYTHON_SRC/configure" \
        --host="$WASI_TARGET_TRIPLE" \
        --build="$(cc -dumpmachine 2>/dev/null || echo "$(uname -m)-$(uname -s | tr '[:upper:]' '[:lower:]')")" \
        --with-build-python="$HOST_PYTHON" \
        --prefix=/usr/local \
        --disable-shared \
        --disable-ipv6 \
        --disable-test-modules \
        --without-ensurepip \
        --without-pymalloc \
        --with-suffix=".wasm" \
        ac_cv_file__dev_ptmx=no \
        ac_cv_file__dev_ptc=no \
        2>&1 | tail -10

    # Build the static library (not the full executable).
    # -j1 for WASI cross to avoid flaky parallel issues in setup scripts.
    make -j1 libpython${CPYTHON_MAJOR_MINOR}.a 2>&1 | tail -20

    # Keep the archive safe for component model linking even if a stale
    # configure cache ever sneaks dynamic-loading support back in.
    "$WASI_SDK_DIR/bin/llvm-ar" d \
        "$WASI_BUILD/libpython${CPYTHON_MAJOR_MINOR}.a" \
        Python/dynload_shlib.o 2>/dev/null || true

    if [ ! -f "$WASI_BUILD/libpython${CPYTHON_MAJOR_MINOR}.a" ]; then
        echo "ERROR: libpython${CPYTHON_MAJOR_MINOR}.a not produced"
        echo "Build log tail:"
        tail -50 "$WASI_BUILD/config.log" 2>/dev/null || true
        exit 1
    fi
    echo "Static library built: $(ls -lh "$WASI_BUILD/libpython${CPYTHON_MAJOR_MINOR}.a")"
else
    echo "Static library already present."
fi

# ── Assemble output ────��──────────────────────────────────────
echo "Assembling output..."
mkdir -p "$OUTPUT_DIR"

cp "$WASI_BUILD/libpython${CPYTHON_MAJOR_MINOR}.a" "$OUTPUT_DIR/"
echo "  Copied libpython${CPYTHON_MAJOR_MINOR}.a"

cp "$WASI_BUILD/Modules/_decimal/libmpdec/libmpdec.a" "$OUTPUT_DIR/"
echo "  Copied libmpdec.a"

cp "$WASI_BUILD/Modules/_hacl/libHacl_Hash_SHA2.a" "$OUTPUT_DIR/"
echo "  Copied libHacl_Hash_SHA2.a"

cp "$WASI_BUILD/Modules/expat/libexpat.a" "$OUTPUT_DIR/"
echo "  Copied libexpat.a"

cp "$WASI_BUILD/Modules/_hacl/Hacl_Hash_MD5.o" "$OUTPUT_DIR/"
cp "$WASI_BUILD/Modules/_hacl/Hacl_Hash_SHA1.o" "$OUTPUT_DIR/"
cp "$WASI_BUILD/Modules/_hacl/Hacl_Hash_SHA2.o" "$OUTPUT_DIR/"
cp "$WASI_BUILD/Modules/_hacl/Hacl_Hash_SHA3.o" "$OUTPUT_DIR/"
echo "  Copied HACL hashlib objects"

# Copy the pyconfig.h and other headers needed for linking.
mkdir -p "$OUTPUT_DIR/include/python${CPYTHON_MAJOR_MINOR}"
cp "$WASI_BUILD/pyconfig.h" "$OUTPUT_DIR/include/python${CPYTHON_MAJOR_MINOR}/"
# Also copy the source-tree headers.
cp "$CPYTHON_SRC/Include"/*.h "$OUTPUT_DIR/include/python${CPYTHON_MAJOR_MINOR}/"
if [ -d "$CPYTHON_SRC/Include/cpython" ]; then
    mkdir -p "$OUTPUT_DIR/include/python${CPYTHON_MAJOR_MINOR}/cpython"
    cp "$CPYTHON_SRC/Include/cpython"/*.h "$OUTPUT_DIR/include/python${CPYTHON_MAJOR_MINOR}/cpython/"
fi
if [ -d "$CPYTHON_SRC/Include/internal" ]; then
    mkdir -p "$OUTPUT_DIR/include/python${CPYTHON_MAJOR_MINOR}/internal"
    cp "$CPYTHON_SRC/Include/internal"/*.h "$OUTPUT_DIR/include/python${CPYTHON_MAJOR_MINOR}/internal/"
fi
echo "  Copied headers"

# Collect the stdlib — only the pure-Python modules that the wrapper
# script needs (sys, io, traceback, base64). Ship the full Lib/ so
# Python's import machinery finds its bootstrap modules.
if [ -d "$CPYTHON_SRC/Lib" ]; then
    mkdir -p "$OUTPUT_DIR/lib/python${CPYTHON_MAJOR_MINOR}"
    # Copy Lib/ but exclude test directories to save space.
    rsync -a --exclude='test/' --exclude='tests/' --exclude='__pycache__/' \
        "$CPYTHON_SRC/Lib/" "$OUTPUT_DIR/lib/python${CPYTHON_MAJOR_MINOR}/"
    echo "  Copied stdlib ($(du -sh "$OUTPUT_DIR/lib/python${CPYTHON_MAJOR_MINOR}" | cut -f1))"
fi

# Frozen modules archive — if the build produced a _freeze_importlib
# directory, the bytecodes are compiled into the static lib already.
echo ""
echo "=== CPython WASI build complete ==="
echo "Output directory: $OUTPUT_DIR"
ls -lh "$OUTPUT_DIR/libpython${CPYTHON_MAJOR_MINOR}.a"
