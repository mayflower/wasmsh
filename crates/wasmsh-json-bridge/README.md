# wasmsh-json-bridge

Part of the [wasmsh](https://github.com/mayflower/wasmsh) workspace — a browser-first shell runtime in Rust.

Shared JSON transport + probe helpers used by the Pyodide embedding and the scalable runner path. Keeps `HostCommand` / `WorkerEvent` serialisation identical whether the embedder reaches the runtime through a C ABI (Pyodide, in-process) or over HTTP (runner pod, through the dispatcher).

See the [main repository](https://github.com/mayflower/wasmsh) for documentation, architecture overview, and usage examples.
