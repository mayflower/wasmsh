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

# ── Release ──────────────────────────────────────────────────

# Create a release tag (usage: just release 0.1.0)
release version:
    @echo "Creating release v{{version}}..."
    @echo "Running full CI..."
    just ci
    @echo "All checks passed. Tag and push with:"
    @echo "  git tag -a v{{version}} -m 'Release {{version}}'"
    @echo "  git push origin v{{version}}"
