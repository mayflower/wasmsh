#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

# Make sure cargo is on PATH.  rustup-installed toolchains live under
# `~/.cargo/bin`; we don't hardcode the host triple subdir because that
# only matches one host (e.g. macOS aarch64) and is wrong everywhere
# else.
export PATH="$HOME/.cargo/bin:$PATH"

# ── Locate emcc ─────────────────────────────────────────────
# In CI, the only emcc on the runner lives inside Pyodide's vendored
# emsdk at `tools/pyodide/pyodide-src/emsdk/emsdk/`, and that emsdk's
# `emsdk_env.sh` is the only thing that puts emcc on PATH.  Source it
# if emcc isn't already visible (`just build-pyodide` runs in a
# separate shell, so its in-process activation doesn't carry over).
PYODIDE_EMSDK_ENV="$REPO_ROOT/tools/pyodide/pyodide-src/emsdk/emsdk/emsdk_env.sh"
if ! command -v emcc &>/dev/null && [ -f "$PYODIDE_EMSDK_ENV" ]; then
    echo "Activating Pyodide-built emsdk: $PYODIDE_EMSDK_ENV"
    # shellcheck disable=SC1090
    source "$PYODIDE_EMSDK_ENV"
fi

if ! command -v emcc &>/dev/null; then
    echo "ERROR: emcc not found on PATH."
    echo ""
    echo "Tried sourcing the Pyodide-built emsdk at:"
    echo "  $PYODIDE_EMSDK_ENV"
    echo "but it does not exist.  Run \`just build-pyodide\` first to install"
    echo "Pyodide's vendored emsdk, or install Emscripten system-wide:"
    echo "  https://emscripten.org/docs/getting_started/downloads.html"
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
    --locked \
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
