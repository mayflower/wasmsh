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
# which emcc rejects.  Keep MAIN_MODULE=2 (explicit exports only) and
# add the standard Pyodide export list so compiled side modules
# (DuckDB, numpy, etc.) can resolve CPython / libc / C++ symbols.
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

# Fix libffi autoreconf on modern Linux.
# libffi's configure.ac uses LT_SYS_SYMBOL_USCORE removed in libtool 2.5+.
# Wrapper patches configure.ac before running real autoreconf.
WRAPPER_DIR="$PYODIDE_SRC/.bin-wrappers"
mkdir -p "$WRAPPER_DIR"
# Find the real autoreconf path before shadowing it.
REAL_AUTORECONF="$(which autoreconf 2>/dev/null || echo /usr/bin/autoreconf)"
cat > "$WRAPPER_DIR/autoreconf" << ENDWRAPPER
#!/bin/sh
if [ -f configure.ac ] && grep -q LT_SYS_SYMBOL_USCORE configure.ac; then
    sed -i '/LT_SYS_SYMBOL_USCORE/d' configure.ac
fi
exec "$REAL_AUTORECONF" "\$@"
ENDWRAPPER
chmod +x "$WRAPPER_DIR/autoreconf"
export PATH="$WRAPPER_DIR:$PATH"
echo "  Installed autoreconf wrapper to patch libffi configure.ac."

# ── Generate comprehensive export list for compiled packages ──
# MAIN_MODULE=2 only exports explicitly listed functions.  Compiled
# side modules (DuckDB, numpy, etc.) need CPython / libc / C++ stdlib
# symbols that MAIN_MODULE=1 would export automatically.  We download
# the standard Pyodide CDN wasm, extract its full export list, merge
# with wasmsh-specific exports, and patch EXPORTED_FUNCTIONS to use
# a @response file.  This avoids MAIN_MODULE=1 which would also
# export Rust symbols containing '$' that break emcc JS glue.
PYODIDE_CDN="https://cdn.jsdelivr.net/pyodide/v${PYODIDE_VERSION}/full"
STANDARD_EXPORTS="$SCRIPT_DIR/standard-exports-${PYODIDE_VERSION}.cache"
EXPORT_RESPONSE="$PYODIDE_SRC/exported-functions.json"

if [ ! -f "$STANDARD_EXPORTS" ]; then
    echo "Downloading standard Pyodide wasm for export list..."
    STANDARD_WASM="$(mktemp)"
    curl -sSL "$PYODIDE_CDN/pyodide.asm.wasm" -o "$STANDARD_WASM"
    python3 "$SCRIPT_DIR/extract-standard-exports.py" "$STANDARD_WASM" > "$STANDARD_EXPORTS"
    rm -f "$STANDARD_WASM"
    echo "  Extracted $(wc -l < "$STANDARD_EXPORTS") standard function exports."
fi

# Build combined JSON export array for Emscripten @response file
python3 -c "
import json
symbols = set()
symbols.add('_main')
for name in ['_wasmsh_probe_version','_wasmsh_probe_write_text',
             '_wasmsh_probe_file_equals','_wasmsh_runtime_new',
             '_wasmsh_runtime_handle_json','_wasmsh_runtime_free',
             '_wasmsh_runtime_free_string']:
    symbols.add(name)
with open('$STANDARD_EXPORTS') as f:
    for line in f:
        sym = line.strip()
        if sym:
            symbols.add(sym)
with open('$EXPORT_RESPONSE', 'w') as f:
    json.dump(sorted(symbols), f)
print(f'  Combined export list: {len(symbols)} symbols.')
"

# Patch Makefile.envs to use the response file instead of inline EXPORTS.
# There are two EXPORTED_FUNCTIONS lines: one in LDFLAGS_BASE ("-s ...")
# and one in MAIN_MODULE_LDFLAGS ("-s..." without space). Patch both so
# the later one doesn't override the first.
if [ -f "$EXPORT_RESPONSE" ] && ! grep -q "exported-functions.json" Makefile.envs; then
    "$SED" -i "s|-s EXPORTED_FUNCTIONS='\$(EXPORTS)'|-s EXPORTED_FUNCTIONS=@$EXPORT_RESPONSE|g" Makefile.envs
    "$SED" -i "s|-sEXPORTED_FUNCTIONS='\$(EXPORTS)'|-sEXPORTED_FUNCTIONS=@$EXPORT_RESPONSE|g" Makefile.envs
    echo "  Patched EXPORTED_FUNCTIONS to use @response file."
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

# NOTE: Deno compat is handled at runtime in node-module.mjs by pre-loading
# pyodide.asm.js via createRequire() before loadPyodide runs. No build-time
# patching of ENVIRONMENT_IS_NODE is needed.

# Copy pyodide.js loader and supporting files if they exist
for f in dist/pyodide.js dist/pyodide.mjs dist/package.json dist/python_stdlib.zip dist/pyodide-lock.json dist/repodata.json; do
    [ -f "$f" ] && cp "$f" "$DIST_DIR/" || true
done

# ── Fetch micropip + packaging + lockfile from Pyodide CDN ─────
# micropip is tagged "always" in standard Pyodide — every sandbox
# should have it available via `import micropip` out of the box.

echo "Fetching pyodide-lock.json from CDN..."
if [ ! -f "$DIST_DIR/pyodide-lock.json" ]; then
    curl -sSL "$PYODIDE_CDN/pyodide-lock.json" -o "$DIST_DIR/pyodide-lock.json"
fi

echo "Fetching micropip + packaging wheels from CDN..."
for whl_name in micropip packaging; do
    # Extract the wheel filename from the lockfile
    whl_file=$(python3 -c "
import json, sys
lock = json.load(open('$DIST_DIR/pyodide-lock.json'))
pkg = lock['packages'].get('$whl_name')
if pkg: print(pkg['file_name'])
else: sys.exit(1)
" 2>/dev/null) || continue
    if [ -n "$whl_file" ] && [ ! -f "$DIST_DIR/$whl_file" ]; then
        echo "  Downloading $whl_file ..."
        curl -sSL "$PYODIDE_CDN/$whl_file" -o "$DIST_DIR/$whl_file"
    fi
done

echo "=== Custom Pyodide build complete ==="
echo "Distribution: $DIST_DIR"
ls -lh "$DIST_DIR/"

if command -v node >/dev/null 2>&1; then
    echo "Packaging runtime assets for npm and Python consumers..."
    node "$SCRIPT_DIR/package-runtime-assets.mjs"
fi
