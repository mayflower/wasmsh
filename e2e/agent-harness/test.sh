#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

export PATH="$HOME/.cargo/bin:$(rustc --print sysroot 2>/dev/null)/bin:$PATH"

# ── Step 1: Rebuild the Pyodide wasm from current Rust source ──
echo "=== Rebuilding custom Pyodide from current Rust source ==="
cd "$REPO_ROOT"
bash tools/pyodide/build-custom.sh

# ── Step 2: Package the assets into the npm package ────────────
echo ""
echo "=== Packaging runtime assets ==="
node tools/pyodide/package-runtime-assets.mjs

# ── Step 3: Reinstall local deps so the agent harness picks up fresh wasm ──
echo ""
echo "=== Reinstalling agent harness dependencies ==="
cd "$SCRIPT_DIR"
npm install

# ── Step 4: Run the harness ────────────────────────────────────
echo ""
echo "=== Running agent harness ==="
node run.mjs "$@"
