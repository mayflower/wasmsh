# wasmsh-pyodide-runtime

Packaged Pyodide-backed wasmsh runtime assets for Python integrations.

This package bundles the custom Pyodide distribution and Node host script
produced by the [wasmsh](https://github.com/mayflower/wasmsh) build, and
exposes stable helpers to locate them at runtime. It is the Python-side
companion to the `wasmsh-pyodide` npm package.

## Install

```bash
pip install wasmsh-pyodide-runtime
```

## Usage

```python
from wasmsh_pyodide_runtime import get_dist_dir, get_asset_path, get_node_host_script

# Path to the packaged Pyodide distribution directory
dist = get_dist_dir()

# Path to a specific asset (e.g., the WASM binary)
wasm = get_asset_path("pyodide.asm.wasm")

# Path to the Node host script (used by langchain-wasmsh)
host = get_node_host_script()
```

## Reference

### `get_dist_dir() -> Path`

Returns the directory containing the packaged Pyodide assets (`pyodide.asm.js`,
`pyodide.asm.wasm`, `python_stdlib.zip`, etc.).

### `get_asset_path(name: str) -> Path`

Returns the path to a specific asset file within the distribution directory.

### `get_node_host_script() -> Path`

Returns the path to the packaged `node-host.mjs` script. This is the
entrypoint that `langchain-wasmsh` launches as a Node.js child process to host
the Pyodide/Emscripten runtime.

## Explanation

This package exists so that Python consumers (like `langchain-wasmsh`) can
depend on a single PyPI package to get the wasmsh runtime assets, without
needing to install npm or run a build. The assets are identical to those in
the `wasmsh-pyodide` npm package — the `tools/pyodide/package-runtime-assets.mjs`
script copies them into both package layouts from the same build output.
