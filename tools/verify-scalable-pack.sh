#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NODE_BIN="${NODE_BIN:-/opt/homebrew/opt/node@22/bin/node}"

if [[ ! -x "${NODE_BIN}" ]]; then
  NODE_BIN="node"
fi

run() {
  printf '\n==> %s\n' "$*"
  "$@"
}

cd "${ROOT_DIR}"

run cargo fmt --all --check
run cargo clippy --workspace --all-targets --all-features -- -D warnings
run cargo test --workspace --all-features
run cargo test --doc --workspace --all-features
run cargo test -p wasmsh-testkit --test suite_runner
run cargo test -p wasmsh-dispatcher
run "${NODE_BIN}" --test e2e/build-contract/tests/*.test.mjs
run "${NODE_BIN}" --test e2e/pyodide-node/tests/*.test.mjs
run "${NODE_BIN}" --test e2e/runner-node/tests/*.test.mjs
run just build-pyodide
run just build-snapshot
run just test-e2e-pyodide-node
run just test-e2e-runner-node
run env WASMSH_RESTORE_STRICT_MS="${WASMSH_RESTORE_STRICT_MS:-100}" just bench-runner-restore

printf '\nScalable prompt-pack verification passed.\n'
