#!/usr/bin/env bash
#
# Builds the wasmsh-component crate for wasm32-wasip2 and reports the
# artifact path on success. The mirror of build-probe.sh for the WASI P2
# Component Model transport.
#
# Environment:
#   SKIP_WASIP2=1    Skip the build entirely (used for test reruns on
#                    hosts that cannot install the target).
#   WASMSH_COMPONENT_PROFILE=dev|release  Build profile. Defaults to dev
#                    so the script is fast to iterate on locally; CI
#                    overrides to release to inspect a realistic size.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PYTHON_WASI_SCRIPT="$REPO_ROOT/tools/python-wasi/build.sh"
PYTHON_WASI_LIB="$REPO_ROOT/tools/python-wasi/output/libpython3.13.a"

export PATH="$HOME/.cargo/bin:$PATH"

if [ "${SKIP_WASIP2:-0}" = "1" ]; then
    echo "SKIP_WASIP2=1 — skipping wasm32-wasip2 build"
    exit 0
fi

PROFILE="${WASMSH_COMPONENT_PROFILE:-dev}"

if [ ! -f "$PYTHON_WASI_LIB" ]; then
    echo "Building CPython WASI runtime assets..."
    bash "$PYTHON_WASI_SCRIPT"
fi

# ── Ensure the Rust target is installed ─────────────────────
rustup target add wasm32-wasip2 2>/dev/null || true

# ── Build ───────────────────────────────────────────────────
echo "Building wasmsh-component for wasm32-wasip2 (profile: ${PROFILE})..."
if [ "${PROFILE}" = "release" ]; then
    OUT_DIR="release"
    cargo build \
        --manifest-path "$REPO_ROOT/crates/wasmsh-component/Cargo.toml" \
        --target wasm32-wasip2 \
        --features component-export \
        --locked \
        --release
else
    OUT_DIR="debug"
    cargo build \
        --manifest-path "$REPO_ROOT/crates/wasmsh-component/Cargo.toml" \
        --target wasm32-wasip2 \
        --locked \
        --features component-export
fi

ARTIFACT="$REPO_ROOT/target/wasm32-wasip2/${OUT_DIR}/wasmsh_component.wasm"

if [ -f "$ARTIFACT" ]; then
    echo "Build succeeded."
    echo "Artifact: $ARTIFACT"
    ls -lh "$ARTIFACT"
else
    echo "ERROR: Expected artifact not found at: $ARTIFACT"
    exit 1
fi
