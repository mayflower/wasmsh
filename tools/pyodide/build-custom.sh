#!/usr/bin/env bash
set -euo pipefail

# ── Paths ───────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=versions.env
source "$SCRIPT_DIR/versions.env"
PYODIDE_SRC="$SCRIPT_DIR/pyodide-src"
DIST_DIR="$REPO_ROOT/dist/pyodide-custom"

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

# ── Build the wasmsh-pyodide runtime staticlib ──────────────
# We used to also build a separate `wasmsh-pyodide-probe` staticlib and link
# both into the Pyodide wasm. But because both crates pulled in
# `wasmsh-protocol` (and therefore `serde_core`) independently,
# `MAIN_MODULE=1` (which uses `--whole-archive`) hit duplicate symbol errors.
# The probe's three C ABI helpers now live inside `wasmsh-pyodide` itself
# (see `src/probe.rs`), so we only need to build and link one staticlib.
#
# `-C symbol-mangling-version=v0` is required: with `MAIN_MODULE=1` Emscripten
# attempts to expose every linked symbol to the JS side, and the legacy Rust
# mangling produces names containing `$` (e.g.
# `_ZN68_$LT$serde_json..read..StrRead$u20$as$u20$...`) which emcc rejects as
# invalid JS identifiers. The v0 mangling scheme uses only `[A-Za-z0-9_]`.
echo "Building wasmsh-pyodide runtime staticlib with Pyodide's emsdk..."
rustup target add wasm32-unknown-emscripten 2>/dev/null || true

PYODIDE_BUILD="$SCRIPT_DIR/pyodide-build"
RUSTFLAGS="-C symbol-mangling-version=v0 ${RUSTFLAGS:-}" \
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

# ── Patch Pyodide to link the runtime staticlib ────────────
echo "Patching Pyodide Makefile..."

# Add the runtime staticlib to the Pyodide link command.
if ! grep -q "libwasmsh_pyodide" Makefile; then
    "$SED" -i "s|\$(CXX) -o dist/pyodide.asm.js -lpyodide src/core/main.o|\$(CXX) -o dist/pyodide.asm.js -lpyodide src/core/main.o $RUNTIME_LIB|" Makefile
    echo "  Patched link command to include the wasmsh runtime lib."
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

# Keep MAIN_MODULE=1 (upstream Pyodide default) so compiled side modules
# (numpy, pandas, DuckDB, …) can resolve CPython / libc / C++ symbols
# automatically.  An earlier attempt switched to MAIN_MODULE=2 to avoid
# Rust legacy mangling that produces '$' in symbol names; we now keep
# MAIN_MODULE=1 because (a) our crates are built with v0 mangling
# (RUSTFLAGS above) and (b) we patch emscripten.py below to filter the
# remaining `$`-bearing symbols that come from precompiled std out of
# the JS bindings list.

# Add probe + runtime symbols to EXPORTED_FUNCTIONS so they survive
# tree-shaking and are reachable via ccall.  MAIN_MODULE=1 also exports
# everything else automatically.
if ! grep -q "wasmsh_probe_version" Makefile.envs; then
    "$SED" -i 's|EXPORTS=_main|EXPORTS=_main \\\n   ,_wasmsh_probe_version \\\n   ,_wasmsh_probe_write_text \\\n   ,_wasmsh_probe_file_equals \\\n   ,_wasmsh_runtime_new \\\n   ,_wasmsh_runtime_handle_json \\\n   ,_wasmsh_runtime_free \\\n   ,_wasmsh_runtime_free_string|' Makefile.envs
    echo "  Added wasmsh_probe_* and wasmsh_runtime_* to EXPORTS."
fi

# Patch upstream emscripten to filter out exports whose names are not valid
# JS identifiers, instead of erroring out. Rust's precompiled std library
# uses legacy mangling that produces symbol names containing `$` and `..`
# (e.g. `_ZN72_$LT$$RF$str$u20$as$u20$alloc..ffi..c_str...$E`), which em++
# rejects when generating Module bindings. The patched code drops them from
# the JS bindings list while leaving them in the wasm exports table — so
# dynamic linking from compiled side modules can still resolve them, while
# the JS glue stays valid.
export EMSCRIPTEN_PY="$PYODIDE_SRC/emsdk/emsdk/upstream/emscripten/tools/emscripten.py"
if ! grep -q "wasmsh patch: filter invalid identifier exports" "$EMSCRIPTEN_PY"; then
    python3 - <<'PYEOF'
import os, pathlib
p = pathlib.Path(os.environ["EMSCRIPTEN_PY"])
src = p.read_text()
old = (
    "  # Rust side modules may have exported symbols that are not valid\n"
    "  # identifiers. They are meant to be called from native code in the main\n"
    "  # module not from JavaScript anyways, so don't perform this check on them.\n"
    "  if not settings.SIDE_MODULE:\n"
    "    for n in unexpected_exports:\n"
    "      if not n.isidentifier():\n"
    "        exit_with_error(f'invalid export name: {n}')\n"
)
new = (
    "  # wasmsh patch: filter invalid identifier exports instead of erroring.\n"
    "  # Rust side/main modules link std library code using legacy mangling\n"
    "  # which produces symbol names containing '$' and '..'. Such symbols\n"
    "  # are meant to be reachable via wasm dynamic linking, not JavaScript,\n"
    "  # so we drop them from the JS bindings list while leaving them in the\n"
    "  # wasm exports table.\n"
    "  unexpected_exports = [e for e in unexpected_exports if e.isidentifier()]\n"
)
if old not in src:
    raise SystemExit("ERROR: emscripten.py does not contain the expected block to patch")
p.write_text(src.replace(old, new))
print("  Patched emscripten.py to filter invalid identifier exports.")
PYEOF
    # Invalidate Python bytecode cache so the patched module is reloaded.
    find "$PYODIDE_SRC/emsdk/emsdk/upstream/emscripten/tools" \
        -name '__pycache__' -type d -exec rm -rf {} + 2>/dev/null || true
else
    echo "  emscripten.py already patched."
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

PYODIDE_CDN="https://cdn.jsdelivr.net/pyodide/v${PYODIDE_VERSION}/full"

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
