# Design Decisions

Key choices made in wasmsh and the reasoning behind them.

## Why a New Shell?

Existing shells (bash, zsh, ash) are designed for Unix systems with processes, signals, filesystems, and TTYs. They cannot run in a browser. Porting them would require emulating the entire POSIX process model.

wasmsh takes the opposite approach: build a shell runtime from scratch that is native to the browser's constraints. The result is smaller, more controllable, and doesn't carry Unix baggage into the browser.

## Why Bash Syntax?

Bash is the de facto scripting language. Developers write bash scripts without thinking about it. Supporting bash syntax means wasmsh can run the scripts people already have — CI pipelines, deployment scripts, data processing, configuration management.

We don't aim for 100% bash compatibility. We aim for the subset that real scripts actually use.

## Why Not WebAssembly System Interface (WASI)?

WASI provides a POSIX-like system interface for WebAssembly. However:

- WASI shells still need a process model (fork/exec)
- WASI filesystem access is host-dependent
- WASI doesn't solve the cooperative execution problem
- Browser deployment of WASI is still experimental

wasmsh targets `wasm32-unknown-unknown` directly, with its own VFS and execution model.

## Why Rust?

- Compiles to efficient WebAssembly
- Memory safety without garbage collection
- Rich type system for modeling shell semantics
- Cargo ecosystem for dependency management
- No runtime required

## Function Scope Model

In bash, functions share the parent scope by default. Only `local` creates isolation. This is different from most programming languages but is critical for bash compatibility:

```sh
COUNT=0
increment() { COUNT=$((COUNT + 1)); }
increment
echo $COUNT  # prints 1, not 0
```

wasmsh implements this correctly: function calls do not push a new scope. `local` saves the old value and restores it when the function returns.

## Pipeline Execution Model

Real shells fork processes for each pipeline stage and connect them with OS pipes. wasmsh can't fork. Instead:

1. Each stage runs to completion
2. Its stdout is captured into a `PipeBuffer`
3. The buffer is provided as stdin to the next stage

This works correctly for all non-streaming commands. Future versions may add coroutine-based interleaving for streaming pipelines.

## Virtual System Commands

Commands like `id`, `whoami`, `uname`, and `hostname` return deterministic virtual values. This is intentional:

- Sandboxed execution should not leak host information
- Tests should be reproducible
- The browser has no concept of "users" or "hostnames"

## Deterministic Date

`date` returns a fixed value (`2026-01-01 00:00:00 UTC`) by default. This ensures reproducibility. Scripts that need a configurable date can set `$WASMSH_DATE`.

## No GPL Code

wasmsh is Apache-2.0-licensed. No code, test cases, or documentation is copied from GPL-licensed projects (bash, BusyBox). Behavioral compatibility is achieved through specification reading and black-box testing.
