# ADR-0031: Pyodide-WASI Same-Module Runtime

**Status:** Accepted  
**Date:** 2025-04-13  
**Supersedes:** —  
**Related:** ADR-0018 (Pyodide same-module architecture), ADR-0030 (Component Model transport)

## Context

The existing Pyodide path (`wasmsh-pyodide`, `wasm32-unknown-emscripten`) runs Python through a JS-hosted Pyodide instance. This requires Node.js or a browser to operate. For direct Wasmtime execution and future wasmCloud integration, a standalone no-JS artifact is needed that preserves the same Python execution path.

ADR-0030 established the Component Model transport seam (`wasmsh:component/runtime`), but that crate is transport-only — it constructs `JsonRuntimeHandle::new()` without Python support. The real Python-capable path remains in `wasmsh-pyodide`.

## Decision

Add a **new standalone same-module Pyodide-WASI artifact** that:

1. **Keeps the real Python execution path** in the Pyodide-linked runtime (`wasmsh-pyodide` crate with inline `python3` handler and network backend).
2. **Builds as a standalone no-JS Wasm module** (`STANDALONE_WASM=1`) runnable under Wasmtime without Node or browser.
3. **Uses an in-memory C filesystem** (`wasi-shims/memfs.c`) replacing Emscripten's JS-based MEMFS. The Rust `BackendFs = EmscriptenFs` type alias is unchanged — it calls libc, which routes to memfs.
4. **WASI imports are minimal**: stdin/stdout/stderr (fd 0/1/2), `proc_exit`, `environ_get/sizes_get`, `clock_time_get/res_get`, `random_get`. No WASI filesystem preopens.
5. **Host-provided HTTP fetch** via `__wasmsh_host_fetch` env import, implemented by the Wasmtime runner with `ureq`. Enforces `allowed_hosts` allowlist at the host level.
6. **Embeds micropip + packaging** in the artifact's Python stdlib for package installation. A synchronous wrapper (`_micropip_sync.py`) patches `asyncio.gather` and `create_task` to avoid needing a full event loop (sockets unavailable in standalone WASI).
7. **Preserves the existing JSON runtime C ABI**: `wasmsh_runtime_new`, `wasmsh_runtime_handle_json`, `wasmsh_runtime_free`, `wasmsh_runtime_free_string`, `wasmsh_pyodide_boot`.

## Architecture

```
┌─────────────────────────────────────────────────┐
│  Wasmtime Runner (tools/pyodide-wasi-host-runner)│
│  - Loads wasmsh_pyodide_wasi.wasm                │
│  - Provides __wasmsh_host_fetch (ureq + allowlist)│
│  - WASI P1 for stdin/stdout/stderr               │
│  - Drives JSON HostCommand/WorkerEvent protocol   │
└─────────────────────────┬───────────────────────┘
                          │ wasm exports
┌─────────────────────────▼───────────────────────┐
│  wasmsh_pyodide_wasi.wasm (same-module artifact) │
│  ┌──────────────────────────────────────────────┐│
│  │ wasmsh-pyodide (Rust)                        ││
│  │  - JsonRuntimeHandle with python handler     ││
│  │  - PyodideNetworkBackend → wasmsh_js_http_fetch│
│  │  - wasmsh_pyodide_boot → Py_Initialize       ││
│  ├──────────────────────────────────────────────┤│
│  │ CPython 3.13 (static archive)                ││
│  │  - _PyEM_TrampolineCall (count_args dispatch)││
│  │  - Stripped emscripten_trampoline.o           ││
│  ├──────────────────────────────────────────────┤│
│  │ memfs.c (in-memory filesystem)               ││
│  │  - Overrides Emscripten standalone stubs      ││
│  │  - Directory listing (__syscall_getdents64)   ││
│  │  - Buffered write flush (__stdio_write)       ││
│  ├──────────────────────────────────────────────┤│
│  │ wasmsh_fetch_module.c (_wasmsh_fetch)        ││
│  │  - CPython extension: fetch(url) from Python  ││
│  ├──────────────────────────────────────────────┤│
│  │ Embedded Python stdlib + micropip + packaging ││
│  │  - _micropip_sync.py (no-event-loop wrapper)  ││
│  │  - _emfs_handler.py (emfs: URL handler)       ││
│  └──────────────────────────────────────────────┘│
└──────────────────────────────────────────────────┘
```

## Key Technical Decisions

- **Pure C memfs, not WASI filesystem**: Emscripten's standalone stubs return `-EPERM` for file ops. We override them with a complete in-memory filesystem compiled into the wasm module. WASI preopens are not used.
- **2MB stack**: Default Emscripten stack (64KB) is insufficient for the call chain wasm export → Rust JSON → shell → python handler → CPython.
- **exnref conversion**: Emscripten's `SUPPORT_LONGJMP=wasm` generates legacy try/catch that Wasmtime doesn't support. Post-processed with `wasm-opt --translate-to-exnref`.
- **Trampoline via ref.test**: CPython's `PY_CALL_TRAMPOLINE` is stripped; replaced with `_PyEM_TrampolineCall` that uses a `count_args` helper wasm module (GC proposal `ref.test`).
- **No asyncio event loop**: Standalone WASI has no sockets for the event loop self-pipe. `_micropip_sync.py` patches `asyncio.gather` and `create_task` to run coroutines sequentially.

## Naming

| Item | Path |
|------|------|
| Build script | `tools/pyodide/build-wasi.sh` |
| Artifact | `dist/pyodide-wasi/wasmsh_pyodide_wasi.wasm` |
| Host runner | `tools/pyodide-wasi-host-runner/` |
| Filesystem shims | `tools/pyodide/wasi-shims/` |
| Build-contract test | `e2e/build-contract/tests/pyodide-wasi-build.test.mjs` |
| Python behavioral tests | `e2e/build-contract/tests/pyodide-wasi-python.test.mjs` |
| Network + micropip tests | `e2e/build-contract/tests/pyodide-wasi-network.test.mjs` |
| Just commands | `build-pyodide-wasi`, `test-e2e-pyodide-wasi`, `test-e2e-pyodide-wasi-network` |

## What This Does Not Change

- The existing browser/Node Pyodide path (`build-custom.sh`, `e2e/pyodide-node/`, `e2e/pyodide-browser/`) is untouched.
- The Component Model transport (`wasmsh-component`, `wasmsh:component/runtime` WIT) is untouched.
- No new filesystem backend — `BackendFs = EmscriptenFs` is preserved.
- No DeepAgents adapter or wasmCloud host plugin in this ADR.

## Consequences

- A 20MB standalone artifact can run Python + shell under Wasmtime without Node/browser.
- `micropip.install("emfs:...")` works for offline wheel installs.
- `micropip.install("http://...")` works for HTTP wheel installs with allowlist enforcement.
- Shell HTTP utilities (`curl`, `wget`) share the same host-provided network backend.
- Future wasmCloud integration can wire the existing Component Model transport to invoke this artifact.
