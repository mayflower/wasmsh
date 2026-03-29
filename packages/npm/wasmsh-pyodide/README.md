# wasmsh-pyodide

Pyodide-backed wasmsh runtime assets and session helpers for Node.js and browser workers.

This package ships:

- the custom Pyodide build artifacts produced by `wasmsh`
- a reusable Node host process
- a browser worker entrypoint
- a small request/response session API over the wasmsh JSON protocol

The runtime exposes both bash-compatible shell execution and `python` / `python3`
inside the same in-process sandbox rooted at `/workspace`.
