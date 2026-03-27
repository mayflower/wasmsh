#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

export PATH="$HOME/.cargo/bin:$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"

# ── Check emcc ──────────────────────────────────────────────
if ! command -v emcc &>/dev/null; then
    echo "ERROR: emcc not found on PATH."
    echo "Install Emscripten SDK: https://emscripten.org/docs/getting_started/downloads.html"
    echo ""
    echo "Quick start:"
    echo "  git clone https://github.com/emscripten-core/emsdk.git"
    echo "  cd emsdk && ./emsdk install latest && ./emsdk activate latest"
    echo "  source emsdk_env.sh"
    exit 1
fi

echo "emcc found: $(emcc --version | head -1)"

# ── Ensure the Rust target is installed ─────────────────────
rustup target add wasm32-unknown-emscripten 2>/dev/null || true

# ── Build ───────────────────────────────────────────────────
echo "Building wasmsh-pyodide-probe for wasm32-unknown-emscripten..."
cargo build \
    --manifest-path "$REPO_ROOT/crates/wasmsh-pyodide-probe/Cargo.toml" \
    --target wasm32-unknown-emscripten \
    --release

ARTIFACT="$REPO_ROOT/crates/wasmsh-pyodide-probe/target/wasm32-unknown-emscripten/release/libwasmsh_pyodide_probe.a"

if [ -f "$ARTIFACT" ]; then
    echo "Build succeeded."
    echo "Artifact: $ARTIFACT"
    ls -lh "$ARTIFACT"
else
    echo "ERROR: Expected artifact not found at: $ARTIFACT"
    exit 1
fi
