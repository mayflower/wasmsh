# wasmsh Web Example

Interactive shell in the browser using wasmsh as an ES module.

## Prerequisites

Build the wasm-pack web target from the repository root:

```bash
wasm-pack build crates/wasmsh-browser --target web --release --out-dir ../../pkg/web
```

## Run

Serve the repository root via HTTP (required for wasm loading):

```bash
cd ../..
python3 -m http.server 8080
# Open http://localhost:8080/examples/web/
```

## What it demonstrates

- Loading wasmsh as an ES module in the browser
- Interactive REPL with command input
- Real-time stdout/stderr rendering
- VFS operations from the browser
- All shell features (arrays, arithmetic, pipelines, etc.)
