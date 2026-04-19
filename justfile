# wasmsh development commands

# List available recipes
default:
    @just --list

# ── Quality Gates ────────────────────────────────────────────

# Run the full CI locally (mirrors GitHub Actions)
ci: fmt-check clippy clippy-wasm test test-suite deny doc build-wasm

# Quick pre-push check
check: fmt-check clippy test-fast

# ── Formatting ───────────────────────────────────────────────

# Check formatting
fmt-check:
    cargo fmt --all --check

# Fix formatting
fmt:
    cargo fmt --all

# ── Linting ──────────────────────────────────────────────────

# Run clippy on all native targets
clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run clippy for wasm32 target
clippy-wasm:
    rustup target add wasm32-unknown-unknown 2>/dev/null || true
    cargo clippy --target wasm32-unknown-unknown \
        -p wasmsh-ast -p wasmsh-lex -p wasmsh-parse -p wasmsh-expand \
        -p wasmsh-hir -p wasmsh-ir -p wasmsh-state -p wasmsh-fs \
        -p wasmsh-builtins -p wasmsh-utils -p wasmsh-vm \
        -p wasmsh-protocol -p wasmsh-browser \
        -- -D warnings

# ── Testing ──────────────────────────────────────────────────

# Run all Rust tests
test:
    cargo nextest run --workspace --all-features 2>/dev/null || cargo test --workspace --all-features
    cargo test --doc --workspace --all-features

# Fast test run (no doctests)
test-fast:
    cargo nextest run --workspace 2>/dev/null || cargo test --workspace

# Run the TOML declarative test suite
test-suite:
    cargo test -p wasmsh-testkit --test suite_runner

# Run tests for a single crate
test-crate crate:
    cargo nextest run -p {{crate}} 2>/dev/null || cargo test -p {{crate}}

# Run TOML suite with verbose output
test-suite-verbose:
    cargo test -p wasmsh-testkit --test suite_runner -- --nocapture

# ── Benchmarks ───────────────────────────────────────────────

# Run benchmarks
bench:
    cargo bench --workspace

# ── Coverage ─────────────────────────────────────────────────

# Generate HTML coverage report
coverage:
    cargo llvm-cov nextest --workspace --all-features --html
    @echo "Report: target/llvm-cov/html/index.html"

# Generate lcov for CI upload
coverage-lcov:
    cargo llvm-cov nextest --workspace --all-features --lcov --output-path lcov.info

# ── Dependency Checks ────────────────────────────────────────

# Run cargo-deny (licenses, advisories, bans)
deny:
    cargo deny check 2>/dev/null || echo "Install cargo-deny: cargo install cargo-deny"

# Check for dead dependencies
machete:
    cargo machete

# Check all feature flag combinations
hack:
    cargo hack check --feature-powerset --no-dev-deps --workspace

# ── Documentation ────────────────────────────────────────────

# Build and check docs (warnings = errors)
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps

# Build and open docs
doc-open:
    cargo doc --workspace --all-features --no-deps --open

# ── Build ────────────────────────────────────────────────────

# Check the workspace compiles
build-check:
    cargo check --workspace

# Build the workspace (native)
build:
    cargo build --workspace

# Build for wasm32 (debug)
build-wasm:
    rustup target add wasm32-unknown-unknown 2>/dev/null || true
    cargo build --target wasm32-unknown-unknown -p wasmsh-browser

# Build for wasm32 (release, optimized)
build-wasm-release:
    rustup target add wasm32-unknown-unknown 2>/dev/null || true
    cargo build --target wasm32-unknown-unknown --profile dist -p wasmsh-browser

# ── Standalone Build + E2E ───────────────────────────────────

# Build standalone wasm artifact (wasm-pack → e2e/standalone/fixture/pkg)
build-standalone:
    bash e2e/standalone/build.sh

# Run standalone browser E2E tests (Playwright)
test-e2e-standalone:
    cd e2e/standalone && npm install --silent && npx playwright test

# ── Pyodide Build + E2E ─────────────────────────────────────

# Build custom Pyodide distribution with wasmsh linked in (requires emcc)
build-pyodide:
    bash tools/pyodide/build-custom.sh

# Build the immutable snapshot artifact layout for the Node runner
build-snapshot:
    bash -lc 'NODE_BIN="${NODE_BIN:-/opt/homebrew/opt/node@22/bin/node}"; [ -x "$NODE_BIN" ] || NODE_BIN=node; cd "{{justfile_directory()}}" && "$NODE_BIN" tools/pyodide/build-snapshot.mjs --output-dir dist/pyodide-snapshot'

# Copy the custom Pyodide dist into the runtime package layouts
package-pyodide-runtime:
    node tools/pyodide/package-runtime-assets.mjs

# Run Pyodide Node E2E tests
test-e2e-pyodide-node:
    bash -lc 'NODE_BIN="${NODE_BIN:-/opt/homebrew/opt/node@22/bin/node}"; [ -x "$NODE_BIN" ] || NODE_BIN=node; cd "{{justfile_directory()}}" && "$NODE_BIN" --test --test-concurrency=1 e2e/pyodide-node/tests/*.test.mjs'

# Run Node runner E2E tests
test-e2e-runner-node:
    bash -lc 'NODE_BIN="${NODE_BIN:-/opt/homebrew/opt/node@22/bin/node}"; [ -x "$NODE_BIN" ] || NODE_BIN=node; cd "{{justfile_directory()}}" && "$NODE_BIN" --test e2e/runner-node/tests/*.test.mjs'

# Run the restore-latency smoke/bench with optional threshold enforcement
bench-runner-restore:
    bash -lc 'NODE_BIN="${NODE_BIN:-/opt/homebrew/opt/node@22/bin/node}"; [ -x "$NODE_BIN" ] || NODE_BIN=node; cd "{{justfile_directory()}}" && "$NODE_BIN" --test tools/runner-node/bench/restore-latency.test.mjs'

# Run the full scalable prompt-pack verification matrix
verify-scalable-pack:
    bash tools/verify-scalable-pack.sh

# Validate the Helm chart
verify-helm:
    bash tools/verify-helm.sh

# Run the Pyodide Node perf harness contract test
test-e2e-pyodide-node-perf:
    cd {{justfile_directory()}} && node --test e2e/pyodide-node/tests/perf-harness.test.mjs

# Measure Pyodide Node startup and first-command latency
bench-e2e-pyodide-node:
    cd {{justfile_directory()}} && node tools/perf/pyodide-node-session-bench.mjs

# Measure scalable runner restore latency with explicit samples/concurrency controls
bench-runner-load:
    cd {{justfile_directory()}} && node tools/perf/runner-restore-bench.mjs

# Run Pyodide browser E2E tests (Playwright)
test-e2e-pyodide-browser:
    cd e2e/pyodide-browser && npm install --silent && npx playwright test

# ── Emscripten Probe (lower-level) ──────────────────────────

# Build the emscripten probe staticlib (requires emcc)
build-emscripten-probe:
    bash e2e/build-contract/build-probe.sh

# Run the emscripten build contract test (requires emcc)
test-emscripten-probe:
    node --test e2e/build-contract/tests/pyodide-probe-build.test.mjs

# ── WASI P2 Component (Pyodide-parity JSON transport) ───────

# Build the wasmsh-component crate as a WASI P2 component artifact
build-component:
    rustup target add wasm32-wasip2 2>/dev/null || true
    bash e2e/build-contract/build-component.sh

# Run clippy for the component crate on the wasm32-wasip2 target
clippy-component:
    rustup target add wasm32-wasip2 2>/dev/null || true
    cargo clippy --target wasm32-wasip2 -p wasmsh-component --features component-export -- -D warnings

# Run the component build-contract test (build + WIT/probe contract checks)
test-e2e-component:
    node --test e2e/build-contract/tests/component-build.test.mjs

# ── Kubernetes-in-Docker E2E (scalable dispatcher + runner) ─

# Build the container images consumed by the kind e2e (expects
# `just build-pyodide` to have baked the runner assets).
build-images:
    @test -f packages/npm/wasmsh-pyodide/assets/pyodide.asm.wasm \
        || (echo "run 'just build-pyodide' first; runner image needs pyodide.asm.wasm" >&2 && exit 1)
    DOCKER_BUILDKIT=1 docker build -f deploy/docker/Dockerfile.dispatcher -t wasmsh-dispatcher:e2e .
    DOCKER_BUILDKIT=1 docker build -f deploy/docker/Dockerfile.runner -t wasmsh-runner:e2e .

# Boot a kind cluster, helm install the chart, run the scalable-path E2E
# suite, and tear the cluster down.  Requires docker/kind/kubectl/helm and
# prebuilt pyodide assets (see `just build-pyodide`).
test-e2e-kind:
    node e2e/kind/scripts/run.mjs

# Leave the cluster running after the tests to allow manual inspection.
test-e2e-kind-keep:
    node e2e/kind/scripts/run.mjs --keep

# Reuse an existing cluster if one is already up (fast re-runs during dev).
test-e2e-kind-reuse:
    node e2e/kind/scripts/run.mjs --reuse --keep

# Tear down the kind cluster created by the e2e (idempotent).
kind-down:
    kind delete cluster --name wasmsh-e2e || true

# ── Docker-compose remote-sandbox E2E ──────────────────────
# Exercises `WasmshRemoteSandbox` end-to-end against the same dispatcher
# + runner images as the kind e2e, but via docker compose for a faster
# local loop (no kind cluster required).  Requires prebuilt Pyodide
# assets (`just build-pyodide`) and docker on PATH.
test-e2e-dispatcher-compose:
    node e2e/dispatcher-compose/scripts/run.mjs

# Leave the compose stack running after the tests for manual inspection.
test-e2e-dispatcher-compose-keep:
    node e2e/dispatcher-compose/scripts/run.mjs --keep

# Reuse existing images (skip the build step) for fast re-runs.
test-e2e-dispatcher-compose-reuse:
    node e2e/dispatcher-compose/scripts/run.mjs --skip-build

# ── Wasm Post-processing ──────────────────────────────────────

# Build optimized wasm with wasm-opt post-processing
wasm-dist:
    cargo build --target wasm32-unknown-unknown --profile dist -p wasmsh-browser
    wasm-opt -Os --enable-bulk-memory target/wasm32-unknown-unknown/dist/wasmsh_browser.wasm -o target/wasmsh-browser-opt.wasm
    @echo "Original: $(wc -c < target/wasm32-unknown-unknown/dist/wasmsh_browser.wasm) bytes"
    @echo "Optimized: $(wc -c < target/wasmsh-browser-opt.wasm) bytes"

# Profile wasm binary size with twiggy
wasm-size:
    cargo build --target wasm32-unknown-unknown --profile dist -p wasmsh-browser
    twiggy top target/wasm32-unknown-unknown/dist/wasmsh_browser.wasm -n 20

# Check wasm binary size against budget (2MB raw, 500KB gzipped)
wasm-budget:
    cargo build --target wasm32-unknown-unknown --profile dist -p wasmsh-browser
    @SIZE=$(wc -c < target/wasm32-unknown-unknown/dist/wasmsh_browser.wasm | tr -d ' '); \
    echo "Wasm binary size: $SIZE bytes"; \
    if [ "$SIZE" -gt 2097152 ]; then echo "ERROR: Binary exceeds 2MB budget!" && exit 1; fi

# ── Release ──────────────────────────────────────────────────

# Create a release tag (usage: just release 0.1.0)
release version:
    @echo "Creating release v{{version}}..."
    @echo "Running full CI..."
    just ci
    @echo "All checks passed. Tag and push with:"
    @echo "  git tag -a v{{version}} -m 'Release {{version}}'"
    @echo "  git push origin v{{version}}"
