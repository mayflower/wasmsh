# ADR-0019: Dual-Target Packaging

## Status

Accepted

## Context

wasmsh now ships two distinct wasm artifacts from the same codebase:

1. **Standalone** (`wasm32-unknown-unknown`): browser Web Worker via `wasm-bindgen`/`wasm-pack`, used directly with the `WasmShell` JS API.
2. **Pyodide** (`wasm32-unknown-emscripten`): linked into a custom Pyodide build, sharing the Python interpreter's Emscripten module and filesystem.

These targets have different toolchains, build processes, and JS integration patterns.

## Decision

Maintain both targets as first-class with explicit just targets, CI jobs, and documentation:

| Aspect | Standalone | Pyodide |
|--------|-----------|---------|
| Target | `wasm32-unknown-unknown` | `wasm32-unknown-emscripten` |
| Build tool | `wasm-pack` | `cargo build` + Pyodide `make` |
| JS API | `WasmShell` (wasm-bindgen) | C ABI + JSON protocol |
| FS backend | `MemoryFs` | `EmscriptenFs` (libc) |
| Python | N/A | In-process via `PyRun_SimpleString` |
| Just targets | `build-standalone`, `test-e2e-standalone` | `build-pyodide`, `test-e2e-pyodide-node`, `test-e2e-pyodide-browser` |

Version pins are centralized in `tools/pyodide/versions.env` (single source of truth for `PYODIDE_VERSION` and `EMSCRIPTEN_VERSION`).

## Consequences

- Developers without emcc can work on the standalone path without friction
- CI runs both paths (Pyodide job requires emsdk)
- The shared `wasmsh-runtime` crate ensures behavioral parity
- Protocol parity tests verify both paths produce the same event shapes
