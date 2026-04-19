# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Thinking & Quality

Always use maximum thinking effort. Never rush to completion. Read code thoroughly before modifying. When investigating bugs, verify assumptions by reading the actual code rather than guessing. Test locally before publishing or deploying. Never use `git add -A` — always stage specific files by name.

## Project Overview

**wasmsh** is a shell runtime written in Rust that targets two in-process WebAssembly platforms plus a scalable server-side deployment path from a shared core:

1. **Standalone** (`wasm32-unknown-unknown`) — browser Web Worker via `wasm-bindgen`
2. **Pyodide** (`wasm32-unknown-emscripten`) — linked into a custom Pyodide build, sharing the Python interpreter's Emscripten module and filesystem
3. **Scalable** (Kubernetes) — `wasmsh-dispatcher` (Rust HTTP control plane) plus a pool of `wasmsh-runner` pods (Node + Pyodide) installed via `deploy/helm/wasmsh`. Clients speak JSON/HTTP to the dispatcher; the `langchain-wasmsh` adapters ship `WasmshRemoteSandbox` as the first-party client. See `docs/explanation/snapshot-runner.md`.

Execution pipeline: `source -> lexer -> parser -> AST -> HIR -> runtime executor`.

The runtime currently uses two execution paths:
- Default path: direct HIR interpretation in `wasmsh-runtime`
- VM subset path: selected top-level `and/or` lists lower through `wasmsh-ir` into `wasmsh-vm`

Expansion, redirection planning, dispatch, budgeting, and protocol emission are owned by the runtime layer and shared by both paths.

Code, comments, and documentation are in English.

## Current State

The repository has multi-layer coverage across crate tests, runtime/protocol tests, TOML suite cases, and E2E adapters. The runtime/protocol crates are expected to stay green; the broad TOML suite still contains known conformance gaps in areas like arrays, brace expansion, globbing, and `xtrace`. See `SUPPORTED.md` for syntax/command coverage.

**Standalone path**: `wasmsh-browser` wraps `wasmsh-runtime` with wasm-bindgen glue. 6 Playwright E2E tests.

**Pyodide path**: `wasmsh-pyodide` wraps `wasmsh-runtime` with C ABI + JSON protocol. `EmscriptenFs` backend routes VFS through libc (shared with Python). `python`/`python3` commands run in-process via `PyRun_SimpleString`. `pip install` is intercepted at the JS host and routed through micropip. Both Node and browser use `loadPyodide()` for boot. 19 Node E2E + 12 Playwright browser E2E tests.

**Scalable path**: `crates/wasmsh-dispatcher` is an Axum HTTP control plane with session affinity, restore-capacity routing, drain, and Prometheus metrics. Runner pods run `tools/runner-node/src/server.mjs` (Node + the Pyodide path above) and expose `/readyz`, `/runner/snapshot`, `/sessions/...`. Deployment lives in `deploy/helm/wasmsh` (HPA, PDB, NetworkPolicy, headless service) with production images `ghcr.io/mayflower/wasmsh-{dispatcher,runner}`. End-to-end coverage: `e2e/dispatcher-compose` (docker-compose, fast) and `e2e/kind` (full Helm install in kind).

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

# ── Scalable dispatcher + runner ────────────────────
just test-e2e-dispatcher-compose  # docker-compose e2e (fast local loop)
just test-e2e-kind                # kind + Helm e2e (production parity)

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

18 workspace crates under `crates/`: `wasmsh-ast`, `wasmsh-lex`, `wasmsh-parse`, `wasmsh-expand`, `wasmsh-hir`, `wasmsh-ir`, `wasmsh-vm`, `wasmsh-state`, `wasmsh-fs`, `wasmsh-builtins`, `wasmsh-utils`, `wasmsh-runtime`, `wasmsh-browser`, `wasmsh-json-bridge`, `wasmsh-protocol`, `wasmsh-dispatcher`, `wasmsh-testkit`.

2 excluded crates (require emcc): `wasmsh-pyodide-probe`, `wasmsh-pyodide`.

## Adapter Packages (non-Rust)

Published alongside the Rust runtime, under `packages/`:

- `packages/npm/wasmsh-pyodide` → `@mayflowergmbh/wasmsh-pyodide` — Pyodide runtime + Node/browser session helpers.
- `packages/npm/langchain-wasmsh` → `@mayflowergmbh/langchain-wasmsh` — LangChain Deep Agents sandbox backend (TypeScript). Depends on `@mayflowergmbh/wasmsh-pyodide` via `workspace:*`.
- `packages/python/wasmsh-pyodide-runtime` → `wasmsh-pyodide-runtime` — Pyodide dist assets.
- `packages/python/langchain-wasmsh` → `langchain-wasmsh` — LangChain Deep Agents sandbox backend (Python). Depends on `wasmsh-pyodide-runtime`.

The two npm packages form a pnpm workspace (`pnpm-workspace.yaml` at the repo root). Use `pnpm --filter @mayflowergmbh/langchain-wasmsh <cmd>` to run scripts against the adapter. The Python package uses `uv` — run `uv sync --group test` inside `packages/python/langchain-wasmsh` before `pytest`.

Each adapter ships two backends on the identical `BaseSandbox` surface: `WasmshSandbox` (in-process, Node/browser) and `WasmshRemoteSandbox` (HTTP to the `wasmsh-dispatcher` Helm chart for Kubernetes deployments). One-line import change to scale from laptop to cluster. See [`docs/integrations/langchain-wasmsh.md`](docs/integrations/langchain-wasmsh.md).

## Architecture Layers

- **Syntax**: Lexer (stateful, multi-mode) → Parser (recursive descent) → AST
- **Semantics**: HIR normalizes AST into executable command shapes
- **Execution**: `wasmsh-runtime` interprets HIR directly and can lower a bounded subset into `wasmsh-ir` / `wasmsh-vm`
- **VM subset**: simple assignments, builtin execution, selected redirections, and top-level `&&` / `||` short-circuiting
- **Runtime**: `wasmsh-runtime` — shared platform-agnostic core used by both targets
- **Platform**: `BackendFs` type alias → `MemoryFs` (standalone/native tests) or the libc-backed `EmscriptenFs` path (Pyodide, via `emscripten` feature)
- **Standalone embedding**: `wasmsh-browser` — wasm-bindgen Web Worker with `WasmShell` JS API
- **Pyodide embedding**: `wasmsh-pyodide` — C ABI + JSON protocol, `python`/`python3` via `ExternalCommandHandler`
- **Scalable embedding**: `wasmsh-dispatcher` (Axum HTTP) + `tools/runner-node` (Node host running the Pyodide embedding per session) — the JSON bridge in `wasmsh-json-bridge` serialises the same `HostCommand` / `WorkerEvent` protocol that Pyodide uses, so backend code stays shared

## Key ADRs

ADRs are in `docs/adr/`. Key decisions:
- ADR-0001: Clean-room boundary
- ADR-0003: Handwritten parser (no generators)
- ADR-0005: HIR / IR / VM direction of travel
- ADR-0006: Capability-based VFS
- ADR-0009: Budgets and cancellation
- ADR-0011: Testing via differential oracles
- ADR-0017: Shared runtime extraction
- ADR-0018: Pyodide same-module architecture
- ADR-0019: Dual-target packaging
- ADR-0020: E2E-first testing policy
- ADR-0021: Network capability model (curl/wget with host allowlist)
- ADR-0029: Dual-path executor (runtime interpreter + VM subset)
- ADR-0030: Superseded — the WASI P2 Component transport was an early wasmCloud seam; the scalable dispatcher + runner path supersedes it.

## Feature Flags

- `wasmsh-fs/opfs` — OPFS filesystem adapter (stub, planned)
- `wasmsh-fs/emscripten` — libc-backed filesystem path used by Pyodide
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
