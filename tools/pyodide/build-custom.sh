#!/usr/bin/env bash
set -euo pipefail

# ── Paths ───────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=versions.env
source "$SCRIPT_DIR/versions.env"
PYODIDE_SRC="$SCRIPT_DIR/pyodide-src"
DIST_DIR="$REPO_ROOT/dist/pyodide-custom"
PROBE_BUILD="$SCRIPT_DIR/probe-build"

export PATH="$HOME/.cargo/bin:$(rustc --print sysroot 2>/dev/null)/bin:$PATH"

echo "=== wasmsh custom Pyodide build ==="
echo "Pyodide version: $PYODIDE_VERSION"
echo "Repo root:       $REPO_ROOT"

# ── Clone Pyodide source ───────────────────────────────────
if [ ! -d "$PYODIDE_SRC" ]; then
    echo "Cloning Pyodide $PYODIDE_VERSION..."
    git clone --depth 1 --branch "$PYODIDE_VERSION" \
        https://github.com/pyodide/pyodide.git "$PYODIDE_SRC"
else
    echo "Pyodide source already present."
fi

# ── Setup Pyodide's emsdk ──────────────────────────────────
cd "$PYODIDE_SRC"

PYODIDE_EMCC_VERSION="$EMSCRIPTEN_VERSION"
echo "Pyodide emscripten version: $PYODIDE_EMCC_VERSION"

if [ ! -f emsdk/emsdk/.complete ]; then
    echo "Setting up Pyodide's emsdk (ccache build skipped — cmake compat)..."
    cd emsdk
    [ -d emsdk ] && rm -rf emsdk
    git clone --depth 1 https://github.com/emscripten-core/emsdk.git
    cd emsdk
    ./emsdk install --build=Release "$PYODIDE_EMCC_VERSION"

    # Apply Pyodide's emscripten patches
    cd upstream/emscripten
    cat ../../../patches/*.patch | patch -p1 --forward || true
    cd ../..

    # Activate WITHOUT ccache (--embedded enables ccache which fails to build)
    ./emsdk activate --build=Release "$PYODIDE_EMCC_VERSION"
    touch .complete
    cd "$PYODIDE_SRC"
fi

# Source Pyodide's emsdk
# shellcheck disable=SC1091
source emsdk/emsdk/emsdk_env.sh 2>/dev/null || true

if ! command -v emcc &>/dev/null; then
    echo "ERROR: emcc not found after emsdk setup."
    exit 1
fi
echo "Using emcc: $(emcc --version | head -1)"

# Pyodide's Makefiles use sed -i (GNU style). macOS sed is different.
if command -v gsed &>/dev/null; then
    export SED=gsed
else
    export SED=sed
fi

# ── Build Rust crates with Pyodide's emsdk ──────────────────
echo "Building Rust crates with Pyodide's emsdk..."
rustup target add wasm32-unknown-emscripten 2>/dev/null || true

# Build the probe crate
CARGO_TARGET_DIR="$PROBE_BUILD" \
cargo build \
    --manifest-path "$REPO_ROOT/crates/wasmsh-pyodide-probe/Cargo.toml" \
    --target wasm32-unknown-emscripten \
    --release

PROBE_LIB="$PROBE_BUILD/wasm32-unknown-emscripten/release/libwasmsh_pyodide_probe.a"
if [ ! -f "$PROBE_LIB" ]; then
    echo "ERROR: Probe lib not found at $PROBE_LIB"
    exit 1
fi
echo "Probe lib: $PROBE_LIB ($(du -h "$PROBE_LIB" | cut -f1))"

# Build the wasmsh-pyodide runtime crate
PYODIDE_BUILD="$SCRIPT_DIR/pyodide-build"
CARGO_TARGET_DIR="$PYODIDE_BUILD" \
cargo build \
    --manifest-path "$REPO_ROOT/crates/wasmsh-pyodide/Cargo.toml" \
    --target wasm32-unknown-emscripten \
    --release

RUNTIME_LIB="$PYODIDE_BUILD/wasm32-unknown-emscripten/release/libwasmsh_pyodide.a"
if [ ! -f "$RUNTIME_LIB" ]; then
    echo "ERROR: Runtime lib not found at $RUNTIME_LIB"
    exit 1
fi
echo "Runtime lib: $RUNTIME_LIB ($(du -h "$RUNTIME_LIB" | cut -f1))"

# ── Patch Pyodide to link the probe ────────────────────────
echo "Patching Pyodide Makefile..."

# Add both staticlibs to the link command
if ! grep -q "wasmsh_pyodide_probe" Makefile; then
    "$SED" -i "s|\$(CXX) -o dist/pyodide.asm.js -lpyodide src/core/main.o|\$(CXX) -o dist/pyodide.asm.js -lpyodide src/core/main.o $PROBE_LIB $RUNTIME_LIB|" Makefile
    echo "  Patched link command to include probe + runtime libs."
else
    echo "  Link command already patched."
fi

# Add ccall to EXPORTED_RUNTIME_METHODS for the MAIN_MODULE build
if ! grep -q "ccall" Makefile.envs; then
    "$SED" -i "s|-sEXPORTED_RUNTIME_METHODS='wasmTable,ERRNO_CODES'|-sEXPORTED_RUNTIME_METHODS='wasmTable,ERRNO_CODES,ccall,cwrap'|" Makefile.envs
    echo "  Added ccall,cwrap to EXPORTED_RUNTIME_METHODS."
else
    echo "  ccall already in EXPORTED_RUNTIME_METHODS."
fi

# MAIN_MODULE=1 exports ALL symbols incl. Rust mangled names with '$'
# which emcc rejects. Switch to MAIN_MODULE=2 (explicit exports only).
# This is fine for a minimal probe distribution (no dynamic pkg loading).
if grep -q "MAIN_MODULE=1" Makefile.envs; then
    "$SED" -i 's|-s MAIN_MODULE=1|-s MAIN_MODULE=2|' Makefile.envs
    echo "  Switched MAIN_MODULE from 1 to 2 (explicit exports)."
fi

# Add probe + runtime symbols to EXPORTED_FUNCTIONS
if ! grep -q "wasmsh_probe_version" Makefile.envs; then
    "$SED" -i 's|EXPORTS=_main|EXPORTS=_main \\\n   ,_wasmsh_probe_version \\\n   ,_wasmsh_probe_write_text \\\n   ,_wasmsh_probe_file_equals \\\n   ,_wasmsh_runtime_new \\\n   ,_wasmsh_runtime_handle_json \\\n   ,_wasmsh_runtime_free \\\n   ,_wasmsh_runtime_free_string|' Makefile.envs
    echo "  Added wasmsh_probe_* and wasmsh_runtime_* to EXPORTS."
fi

# Add runtime methods needed by the FS test harness.
# Must run AFTER the ccall/cwrap patch above. Check the actual line
# (Makefile.envs has stringToNewUTF8 elsewhere for a different target).
if ! grep "ccall,cwrap,stringToNewUTF8" Makefile.envs >/dev/null 2>&1; then
    "$SED" -i "s|ccall,cwrap'|ccall,cwrap,stringToNewUTF8,UTF8ToString,callMain,FS'|" Makefile.envs
    echo "  Added stringToNewUTF8,callMain,FS to EXPORTED_RUNTIME_METHODS."
fi

# Fix libffi autoreconf failure on modern Linux.
# libffi's configure.ac uses LT_SYS_SYMBOL_USCORE which was removed from
# modern libtool. Patch the cpython Makefile to sed out the autoreconf call
# in libffi's build.sh after clone, using the pre-generated configure.
if ! grep -q "wasmsh_patch_libffi" cpython/Makefile; then
    "$SED" -i '/git checkout FETCH_HEAD/a\\t\t&& sed -i "s|autoreconf -fiv|echo wasmsh_patch_libffi: skipping autoreconf|" ./testsuite/emscripten/build.sh' cpython/Makefile
    echo "  Patched cpython/Makefile to skip libffi autoreconf."
fi

# ── Build CPython + core ────────────────────────────────────
echo "Building Pyodide core (this takes a while on first run)..."

# Install Python build deps into a venv
if [ ! -d ".venv" ]; then
    python3 -m venv .venv
    .venv/bin/pip install --quiet pyodide-build
fi
export PATH="$PYODIDE_SRC/.venv/bin:$PATH"

# Build the core module + stdlib + JS loader
make dist/pyodide.asm.js dist/python_stdlib.zip dist/pyodide.js

# ── Assemble the custom distribution ───────────────────────
echo "Assembling custom distribution..."
mkdir -p "$DIST_DIR"

# Copy the core files needed for Node loading
cp dist/pyodide.asm.js "$DIST_DIR/"
cp dist/pyodide.asm.wasm "$DIST_DIR/"

# Copy pyodide.js loader and supporting files if they exist
for f in dist/pyodide.js dist/pyodide.mjs dist/package.json dist/python_stdlib.zip dist/pyodide-lock.json dist/repodata.json; do
    [ -f "$f" ] && cp "$f" "$DIST_DIR/" || true
done

echo "=== Custom Pyodide build complete ==="
echo "Distribution: $DIST_DIR"
ls -lh "$DIST_DIR/"
