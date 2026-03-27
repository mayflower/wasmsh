# ADR-0018: Pyodide Same-Module Architecture

## Status

Accepted

## Context

Integrating wasmsh with Pyodide requires the shell runtime and Python interpreter to share a filesystem. Two approaches exist: side-loading (separate wasm modules with message passing) or same-module linking (Rust code compiled into the Pyodide main module).

Side-loading would require a cross-module FS bridge and introduce latency. Same-module linking gives zero-cost shared memory and filesystem access.

## Decision

Link wasmsh Rust code as a staticlib into Pyodide's main Emscripten module during the Pyodide build. This is achieved by:

1. **`wasmsh-pyodide-probe`** (excluded from workspace): minimal C ABI probe proving the emscripten toolchain works. `crate-type = ["staticlib"]`.
2. **`wasmsh-pyodide`** (excluded from workspace): C ABI wrapper around `WorkerRuntime` with JSON protocol. Depends on `wasmsh-runtime` with `emscripten` feature.
3. **Build integration**: `tools/pyodide/build-custom.sh` patches Pyodide's Makefile to add both staticlibs to the link command, using `MAIN_MODULE=2` (explicit exports to avoid Rust mangled symbol issues with `$`).

Key technical details:
- Sentinel stubs: Pyodide imports a `sentinel` wasm GC module. The host wrapper provides JS stubs via `instantiateWasm`.
- Python stdlib: Mounted as a zip file in `preRun` before `callMain` boots CPython.
- `python`/`python3` commands: Registered via `ExternalCommandHandler` in the runtime. Captures stdout/stderr by redirecting `sys.stdout`/`sys.stderr` to `StringIO` in a wrapper, then reads temp files via libc.

## Consequences

- Shell and Python share the Emscripten POSIX filesystem (verified bidirectionally)
- No cross-module overhead
- Both crates are excluded from the workspace to avoid requiring emcc for normal development
- Build requires Pyodide source checkout (~10 min first build, cached thereafter)
