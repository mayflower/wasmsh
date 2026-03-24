# wasmsh Python Example

Demonstrates using the wasmsh shell runtime from Python via a Node.js bridge.

## Prerequisites

1. Node.js 18+ installed
2. Build the wasm-pack nodejs target from the repository root:

```bash
wasm-pack build crates/wasmsh-browser --target nodejs --release --out-dir ../../pkg/nodejs
```

## Run

```bash
python example.py
```

## How it works

The `WasmShell` class spawns a Node.js subprocess that loads the wasm-pack
generated package. Commands are sent as JSON over stdin, results are read as
JSON from stdout. This gives Python full access to the shell runtime without
needing a native wasm runtime.

## What it demonstrates

- Basic command execution and output parsing
- Pipelines, variables, parameter expansion
- Arrays and iteration
- Arithmetic (Fibonacci, bitwise)
- Virtual filesystem (write, read, grep)
- Functions with recursion (factorial)
- Extended test `[[ ]]`
- Error handling
- Utilities (md5sum, base64)
