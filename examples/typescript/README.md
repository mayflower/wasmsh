# wasmsh TypeScript Example

Demonstrates using the wasmsh shell runtime from Node.js with TypeScript.

## Prerequisites

Build the wasm-pack nodejs target from the repository root:

```bash
wasm-pack build crates/wasmsh-browser --target nodejs --release --out-dir ../../pkg/nodejs
```

## Run

```bash
npm install
npx tsx example.ts
```

## What it demonstrates

- Basic command execution and output parsing
- Pipelines (`echo | tr | sort`)
- Variables and parameter expansion (`${var##*/}`)
- Arrays and iteration
- Arithmetic (Fibonacci, bitwise)
- Virtual filesystem (write, read, grep)
- Functions with recursion (factorial)
- Extended test `[[ ]]` with glob matching
- Error handling
- Utilities (md5sum, base64)
