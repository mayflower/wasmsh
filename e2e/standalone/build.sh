#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT_DIR="$REPO_ROOT/e2e/standalone/fixture/pkg"

export PATH="$HOME/.cargo/bin:$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"

echo "Building wasmsh-browser for web target..."
wasm-pack build "$REPO_ROOT/crates/wasmsh-browser" \
  --target web \
  --release \
  --out-dir "$OUT_DIR"

echo "Built to $OUT_DIR"
ls -la "$OUT_DIR"
