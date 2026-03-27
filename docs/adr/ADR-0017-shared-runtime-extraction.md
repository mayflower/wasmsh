# ADR-0017: Shared Runtime Extraction

## Status

Accepted

## Context

The shell execution engine lived entirely in `wasmsh-browser`, coupling 4000+ lines of platform-agnostic logic (parse, HIR, expand, execute, builtins, utilities, VFS) with 77 lines of browser-specific `wasm-bindgen` glue. To support a second embedding target (Pyodide/Emscripten), the runtime needed to be reusable without pulling in browser dependencies.

## Decision

Extract the platform-agnostic execution engine into a new crate `wasmsh-runtime`. The browser crate becomes a thin wrapper that re-exports `WorkerRuntime` and keeps only the `wasm-bindgen` entry points and integration tests.

Key design choices:
- **Type alias `BackendFs`** in `wasmsh-fs`: resolves to `MemoryFs` by default, or `EmscriptenFs` when the `emscripten` feature is enabled. This avoids making the runtime generic over the FS type while still supporting backend swapping.
- **Feature-gated FS backend**: `wasmsh-runtime` exposes an `emscripten` feature that forwards to `wasmsh-fs/emscripten`, switching all FS operations to use libc (which routes through Emscripten's POSIX VFS).
- **External command handler**: `WorkerRuntime` accepts an optional `ExternalCommandHandler` callback for host-provided commands (e.g., `python3` in Pyodide), checked in `dispatch_command` after functions/builtins/utilities.

## Consequences

- `wasmsh-browser` dropped from 6076 to ~1976 lines
- `wasmsh-runtime` is 4109 lines, fully testable without wasm
- `wasmsh-testkit` depends on `wasmsh-runtime` directly (no browser dep)
- New embeddings (Pyodide, future Node native) depend only on `wasmsh-runtime`
- All 1379 workspace tests pass unchanged
