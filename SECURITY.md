# Security Policy

## Threat Model

wasmsh is a POSIX-like shell that runs via WebAssembly. It supports two
deployment targets with different trust boundaries:

- **Standalone**: browser Web Worker (`wasm32-unknown-unknown`) with an
  in-memory VFS. No host OS access.
- **Pyodide**: linked into a custom Pyodide build (`wasm32-unknown-emscripten`),
  sharing the Python interpreter's Emscripten module and filesystem. Shell
  commands and Python code run in the same Wasm instance.

### Trust Boundaries — Standalone

1. **Browser sandbox**: The outermost boundary. wasmsh executes as a Wasm
   module inside a Web Worker. It cannot escape the browser sandbox.
2. **Protocol layer**: Communication between the worker and the host page is
   mediated by `wasmsh-protocol` messages. The host page controls which
   commands are available and what resources are exposed.
3. **Virtual filesystem (VFS)**: All file operations use an in-memory VFS
   (`wasmsh-fs`). No real filesystem access occurs. File size is capped at
   64 MiB per file to prevent memory exhaustion.
4. **Shell execution engine**: The shell parses and executes user input.
   Multiple resource limits are enforced to prevent denial-of-service within
   the sandbox.

### Trust Boundaries — Pyodide

1. **Browser sandbox**: Same outermost boundary as standalone.
2. **Shared Emscripten module**: wasmsh and CPython are statically linked into
   the same Wasm module. They share a single linear memory, Emscripten
   filesystem (`EmscriptenFs`), and environment variables. There is **no
   isolation** between shell and Python execution — each can read and modify
   the other's files and memory.
3. **C FFI interface**: The host communicates with wasmsh via exported C
   functions (`wasmsh_runtime_new`, `wasmsh_runtime_handle_json`,
   `wasmsh_runtime_free_string`). Input is JSON-encoded `HostCommand`
   messages. The host is responsible for constructing valid JSON; malformed
   input is rejected but should not cause memory corruption.
4. **Python interpreter**: The `python`/`python3` built-in commands execute
   Python code via `PyRun_SimpleString` in the same process. Python code has
   full access to the shared filesystem and can call any C API exported by
   the module. Shell resource limits (step budgets, output caps) do **not**
   constrain Python execution.
5. **Emscripten filesystem**: Unlike the standalone VFS, `EmscriptenFs` routes
   through Emscripten's libc `open`/`read`/`write`/`stat`. File operations
   are bounded by the Emscripten heap, not wasmsh's per-file 64 MiB limit.

### What wasmsh Does NOT Protect Against

- **Malicious host pages**: If the embedding page is compromised, all bets are
  off. The host page controls the protocol layer and can inject arbitrary
  commands.
- **Browser vulnerabilities**: wasmsh relies on the browser's Wasm sandbox for
  isolation. Browser-level exploits are out of scope.
- **Timing side channels**: No hardening against speculative execution or
  timing attacks has been performed.
- **Cross-domain shell ↔ Python attacks** (Pyodide only): Shell and Python
  share the same address space and filesystem. A malicious Python script can
  modify files that shell commands depend on, and vice versa. The Pyodide
  target assumes both shell and Python inputs are equally trusted.

## Resource Limits

The following limits are enforced to prevent resource exhaustion within the
sandbox:

| Resource | Limit | Location |
|---|---|---|
| Brace expansion items | 10,000 per expansion | `wasmsh-expand` |
| Recursion depth (eval/source/subst) | 100 levels | `wasmsh-browser` |
| Arithmetic operations | Wrapping semantics (no panics) | `wasmsh-expand` |
| Variable expansion depth | 50 levels | `wasmsh-expand` |
| Pipe buffer total size | 64 MiB | `wasmsh-vm` |
| Glob expansion results | 10,000 entries | `wasmsh-browser` |
| VFS file size | 64 MiB per file | `wasmsh-fs` |
| `yes` utility output | 65,536 lines | `wasmsh-utils` |
| Regex backtracking (`=~`) | bounded by input length | `wasmsh-browser` |
| Extended glob recursion | bounded by pattern depth | `wasmsh-browser` |

## Known Limitations

- **No cryptographic operations**: wasmsh does not perform any cryptographic
  operations and should not be used for security-sensitive tasks.
- **No process isolation**: All shell commands run in the same Wasm instance.
  There is no process-level isolation between pipeline stages.
- **Memory**: While individual resource limits exist, there is no global memory
  budget. A determined user could still exhaust available memory by creating
  many large files or expanding many variables simultaneously.
- **CPU**: The `step_budget` configuration limits execution steps, but
  computationally expensive expansions (e.g., deeply nested parameter
  operations) may still cause noticeable delays.
- **Python is unconstrained** (Pyodide only): Shell resource limits do not
  apply to Python code executed via `python`/`python3`. Python can allocate
  memory, run indefinitely, and modify the shared filesystem without
  wasmsh-level restrictions. Rate-limiting or sandboxing Python execution is
  the host's responsibility.

## Reporting Vulnerabilities

If you discover a security issue, please report it by opening a GitHub issue
with the "security" label, or contact the maintainers directly. Please include:

1. A description of the vulnerability
2. Steps to reproduce
3. Expected vs. actual behavior
4. The severity you believe it represents

We aim to acknowledge reports within 48 hours and provide a fix or mitigation
plan within 7 days for critical issues.
