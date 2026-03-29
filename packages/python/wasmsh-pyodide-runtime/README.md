# wasmsh-pyodide-runtime

Packaged Pyodide-backed wasmsh runtime assets for Python integrations.

This package exposes stable helpers to locate:

- the packaged Pyodide distribution directory
- the packaged Node host script from `wasmsh-pyodide`

It is intended to be consumed by DeepAgents sandbox integrations that need a
portable bash + Python sandbox rooted at `/workspace`.
