# Security Policy

## Threat Model

wasmsh is a POSIX-like shell that runs entirely in the browser via WebAssembly.
All code execution is sandboxed within the browser's WebAssembly runtime. There
is no access to the host operating system, native filesystem, or network stack
beyond what the browser page explicitly provides through the protocol layer.

### Trust Boundaries

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

### What wasmsh Does NOT Protect Against

- **Malicious host pages**: If the embedding page is compromised, all bets are
  off. The host page controls the protocol layer and can inject arbitrary
  commands.
- **Browser vulnerabilities**: wasmsh relies on the browser's Wasm sandbox for
  isolation. Browser-level exploits are out of scope.
- **Timing side channels**: No hardening against speculative execution or
  timing attacks has been performed.

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

## Reporting Vulnerabilities

If you discover a security issue, please report it by opening a GitHub issue
with the "security" label, or contact the maintainers directly. Please include:

1. A description of the vulnerability
2. Steps to reproduce
3. Expected vs. actual behavior
4. The severity you believe it represents

We aim to acknowledge reports within 48 hours and provide a fix or mitigation
plan within 7 days for critical issues.
