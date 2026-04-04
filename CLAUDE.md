# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**wasmsh** is a shell runtime written in Rust that targets two WebAssembly platforms from a shared core:

1. **Standalone** (`wasm32-unknown-unknown`) — browser Web Worker via `wasm-bindgen`
2. **Pyodide** (`wasm32-unknown-emscripten`) — linked into a custom Pyodide build, sharing the Python interpreter's Emscripten module and filesystem

Pipeline: `source → stateful lexer → handwritten parser → AST → HIR → expand → cooperative VM → builtins/utilities → VFS/capabilities → protocol events`

Code, comments, and documentation are in English.

## Current State

1422 tests across multiple layers, 0 failures. The full pipeline works end-to-end for both targets. See `SUPPORTED.md` for syntax/command coverage.

**Standalone path**: `wasmsh-browser` wraps `wasmsh-runtime` with wasm-bindgen glue. 6 Playwright E2E tests.

**Pyodide path**: `wasmsh-pyodide` wraps `wasmsh-runtime` with C ABI + JSON protocol. `EmscriptenFs` backend routes VFS through libc (shared with Python). `python`/`python3` commands run in-process via `PyRun_SimpleString`. `pip install` is intercepted at the JS host and routed through micropip. Both Node and browser use `loadPyodide()` for boot. 19 Node E2E + 12 Playwright browser E2E tests.

Notable features: `[[ ]]`, `(( ))`, C-style `for (( ))`, arrays, `declare`/`typeset`, `alias`/`unalias`, `let`, `shopt`, extended globbing, globstar, full arithmetic, case modification, indirect expansion, dynamic variables (`$RANDOM`, `$LINENO`, `$SECONDS`, `$FUNCNAME`, `$BASH_SOURCE`, `$PIPESTATUS`), `printf`/`read`, `mapfile`, `builtin`, `select`, `|&`, `case` fall-through, `set -euo pipefail`, 88 utilities (jq, awk, yq, bc, rg, fd, diff/patch, tree, tar, gzip, unzip, xxd, dd, strings, md5sum/sha*sum, curl, wget).

## Rust Toolchain

Pinned via `rust-toolchain.toml` (stable + rustfmt, clippy, rust-src, llvm-tools, `wasm32-unknown-unknown` + `wasm32-unknown-emscripten` targets). Cargo may not be on PATH by default:
```bash
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
```

## Build & Test Commands

```bash
# ── Core ────────────────────────────────────────────
just check                  # fmt-check + clippy + fast test (pre-push)
just ci                     # full CI: fmt + clippy + test + deny + doc
just test                   # all Rust tests (nextest or cargo test)
just test-suite             # TOML declarative test suite only
just test-crate wasmsh-lex  # single crate

# ── Standalone ──────────────────────────────────────
just build-standalone       # wasm-pack → e2e/standalone/fixture/pkg/
just test-e2e-standalone    # Playwright browser E2E (6 tests)

# ── Pyodide (requires emcc) ────────────────────────
just build-pyodide          # custom Pyodide → dist/pyodide-custom/
just test-e2e-pyodide-node  # Node E2E (19 tests)
just test-e2e-pyodide-browser # Playwright browser E2E (4 tests)
just build-emscripten-probe # emscripten staticlib probe

# ── Quality ─────────────────────────────────────────
just clippy-wasm            # clippy for wasm32 target
just coverage               # HTML coverage report
just deny                   # license/advisory/ban check
just doc                    # docs with warnings-as-errors
```

## Quality Infrastructure

- **Lints**: Clippy pedantic baseline in `Cargo.toml`, `clippy.toml` for project config
- **Formatting**: `rustfmt.toml` (edition 2021, Unix newlines, 100 cols)
- **License/advisory**: `deny.toml` (permissive-only: MIT/Apache/BSD/ISC)
- **CI**: `.github/workflows/ci.yml` (Rust), `pyodide.yml` (Pyodide E2E), `wasm-build.yml` (standalone)
- **Property tests**: `proptest` in `wasmsh-parse/tests/property_tests.rs`
- **E2E-first policy**: ADR-0020 — new integration capabilities start with a failing E2E test

## Non-Negotiable Rules

1. **No GPL code in the core.** Behavior compatibility is a goal; code transfer is forbidden.
2. **No parser generators.** Handwritten stateful lexer and recursive-descent parser only.
3. **No host `exec` in the browser profile.** No `std::fs` in browser-targeted code.
4. **Clean-room provenance.** Tests are original, not copied from GPL projects.
5. **ADR-conformant changes.** Architectural decisions are documented in `docs/adr/`.

## Cargo Workspace Structure

15 workspace crates under `crates/`: `wasmsh-ast`, `wasmsh-lex`, `wasmsh-parse`, `wasmsh-expand`, `wasmsh-hir`, `wasmsh-ir`, `wasmsh-vm`, `wasmsh-state`, `wasmsh-fs`, `wasmsh-builtins`, `wasmsh-utils`, `wasmsh-runtime`, `wasmsh-browser`, `wasmsh-protocol`, `wasmsh-testkit`.

2 excluded crates (require emcc): `wasmsh-pyodide-probe`, `wasmsh-pyodide`.

## Architecture Layers

- **Syntax**: Lexer (stateful, multi-mode) → Parser (recursive descent) → AST
- **Semantics**: HIR (normalizes AST) → IR (linear instructions with jumps)
- **Execution**: Cooperative VM with step budgets, cancellation tokens. Commands resolve to builtin / shell function / bundled utility / external handler
- **Runtime**: `wasmsh-runtime` — shared platform-agnostic core used by both targets
- **Platform**: `BackendFs` type alias → `MemoryFs` (standalone) or `EmscriptenFs` (Pyodide, via `emscripten` feature)
- **Standalone embedding**: `wasmsh-browser` — wasm-bindgen Web Worker with `WasmShell` JS API
- **Pyodide embedding**: `wasmsh-pyodide` — C ABI + JSON protocol, `python`/`python3` via `ExternalCommandHandler`

## Key ADRs

ADRs are in `docs/adr/`. Key decisions:
- ADR-0001: Clean-room boundary
- ADR-0003: Handwritten parser (no generators)
- ADR-0005: HIR → IR → VM pipeline
- ADR-0006: Capability-based VFS
- ADR-0009: Budgets and cancellation
- ADR-0011: Testing via differential oracles
- ADR-0017: Shared runtime extraction
- ADR-0018: Pyodide same-module architecture
- ADR-0019: Dual-target packaging
- ADR-0020: E2E-first testing policy
- ADR-0021: Network capability model (curl/wget with host allowlist)

## Feature Flags

- `wasmsh-fs/opfs` — OPFS filesystem adapter (stub, planned)
- `wasmsh-fs/emscripten` — Emscripten libc filesystem (used by Pyodide path)
- `wasmsh-runtime/emscripten` — forwards to `wasmsh-fs/emscripten`, swaps `BackendFs` to `EmscriptenFs`
- `wasmsh-browser/browser-core` (default) — core shell runtime for browser
- `wasmsh-browser/browser-extended` — adds OPFS persistence

## Version Pins

Pyodide/Emscripten versions are pinned in `tools/pyodide/versions.env` (single source of truth). All build scripts source this file.

## E2E Test Layout

```
e2e/
├── standalone/       # Playwright: standalone browser worker (6 tests)
├── build-contract/   # node:test: emscripten probe build (2 tests)
├── pyodide-node/     # node:test: Pyodide Node E2E (19 tests)
├── pyodide-browser/  # Playwright: Pyodide browser worker (4 tests)
└── repo-checks/      # node:test: repo structure checks (12 tests)
```
