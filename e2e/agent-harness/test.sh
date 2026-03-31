#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
PYODIDE_SRC="$REPO_ROOT/tools/pyodide/pyodide-src"

export PATH="$HOME/.cargo/bin:$(rustc --print sysroot 2>/dev/null)/bin:$PATH"

# ── Step 1: Force-rebuild the Rust staticlibs ──────────────────
# Delete the cached .a files so Cargo recompiles from current source.
echo "=== Cleaning Rust build artifacts ==="
rm -rf "$REPO_ROOT/tools/pyodide/probe-build" \
       "$REPO_ROOT/tools/pyodide/pyodide-build"

# Also delete the Pyodide link output so make re-runs the link step.
if [ -d "$PYODIDE_SRC" ]; then
    rm -f "$PYODIDE_SRC/dist/pyodide.asm.js" \
          "$PYODIDE_SRC/dist/pyodide.asm.wasm" \
          "$PYODIDE_SRC/src/core/main.o"
fi

# ── Step 2: Rebuild the Pyodide wasm from current Rust source ──
echo ""
echo "=== Rebuilding custom Pyodide from current Rust source ==="
cd "$REPO_ROOT"
bash tools/pyodide/build-custom.sh

# Verify the wasm was actually rebuilt
WASM="$REPO_ROOT/dist/pyodide-custom/pyodide.asm.wasm"
if [ ! -f "$WASM" ]; then
    echo "ERROR: pyodide.asm.wasm not found after build"
    exit 1
fi
echo "Built: $WASM ($(stat -f '%Sm' "$WASM" 2>/dev/null || stat -c '%y' "$WASM" 2>/dev/null))"

# ── Step 3: Package the assets into the npm package ────────────
echo ""
echo "=== Packaging runtime assets ==="
node tools/pyodide/package-runtime-assets.mjs

# ── Step 4: Reinstall local deps so the agent harness picks up fresh wasm ──
echo ""
echo "=== Reinstalling agent harness dependencies ==="
cd "$SCRIPT_DIR"
npm install

# ── Step 5: Run the harness ────────────────────────────────────
echo ""
echo "=== Running agent harness ==="
node run.mjs "$@"
