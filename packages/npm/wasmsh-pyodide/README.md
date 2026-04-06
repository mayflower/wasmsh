# wasmsh-pyodide

Pyodide-backed wasmsh runtime for Node.js, Deno, and browser Web Workers.
Provides both bash-compatible shell execution and Python 3 inside the same
in-process sandbox, with a shared filesystem rooted at `/workspace`.

## Getting started

### Install

```bash
npm install wasmsh-pyodide
```

Requires Node.js 20+ or Deno 2+.

### Run your first command (Node.js)

```js
import { createNodeSession } from "wasmsh-pyodide";

const session = await createNodeSession();

const result = await session.run("echo hello && python3 -c \"print('world')\"");
console.log(result.stdout); // hello\nworld\n
console.log(result.exitCode); // 0

await session.close();
```

### Run in a browser Web Worker

```js
import { createBrowserWorkerSession } from "wasmsh-pyodide";

const session = await createBrowserWorkerSession({
  assetBaseUrl: "/node_modules/wasmsh-pyodide/assets",
});

const result = await session.run("python3 -c \"print(2 + 2)\"");
console.log(result.stdout); // 4\n

await session.close();
```

## How-to guides

### Seed files before execution

Pass `initialFiles` when creating a session to pre-populate the sandbox
filesystem:

```js
const session = await createNodeSession({
  initialFiles: [
    { path: "/workspace/config.json", content: new TextEncoder().encode('{"key": "value"}') },
  ],
});

const result = await session.run("cat /workspace/config.json");
```

### Transfer files at runtime

Use `writeFile` and `readFile` to move data in and out of the sandbox:

```js
// Write a file into the sandbox
await session.writeFile("/workspace/input.csv", new TextEncoder().encode("a,b\n1,2\n"));

// Process it and write output
await session.run(
  "python3 -c \"import csv; rows = list(csv.reader(open('/workspace/input.csv'))); open('/workspace/output.txt','w').write(str(len(rows)))\"",
);

// Read the output back
const result = await session.readFile("/workspace/output.txt");
const text = new TextDecoder().decode(result.content);
```

### Install Python packages

Use `installPythonPackages` to add Python packages into the sandbox.
Packages are installed into `/lib/python3.13/site-packages` and become
importable from subsequent `python3` commands in the same session.

#### Bundled packages (offline, no network required)

Some packages are bundled with the runtime and can be installed offline.
These are listed in the local `pyodide-lock.json` and resolve via
`pyodide.loadPackage()` without requiring `allowedHosts`:

```js
// DuckDB is bundled — works offline, no allowedHosts needed
await session.installPythonPackages("duckdb");

await session.run(`python3 -c "
import duckdb
con = duckdb.connect('/workspace/demo.duckdb')
con.sql('create table t as select 42 as x')
print(con.sql('select * from t').fetchall())
"`);
```

The `pip install` shell command also uses the bundled path automatically:

```js
await session.run("pip install duckdb");
```

Database files created by DuckDB live on the shared Emscripten filesystem
at `/workspace/...` and are visible to all subsequent shell and Python
commands in the same session.

#### Local wheel files

```js
// Upload a wheel and install from the in-sandbox filesystem
await session.writeFile("/tmp/my_pkg-1.0-py3-none-any.whl", wheelBytes);
await session.installPythonPackages("emfs:/tmp/my_pkg-1.0-py3-none-any.whl");

// The package is now importable
const result = await session.run('python3 -c "import my_pkg; print(my_pkg.__version__)"');
```

Multiple requirements can be passed as an array:

```js
await session.installPythonPackages([
  "emfs:/tmp/pkg_a-1.0-py3-none-any.whl",
  "emfs:/tmp/pkg_b-2.0-py3-none-any.whl",
]);
```

#### Network installs (requires allowedHosts)

Install from HTTP URLs or by package name:

```js
const session = await createNodeSession({
  allowedHosts: ["pypi.org", "files.pythonhosted.org"],
});

// Install by package name (resolved from PyPI)
await session.installPythonPackages("six");

// Install from direct URL
await session.installPythonPackages(
  "https://files.pythonhosted.org/packages/.../my_pkg-1.0-py3-none-any.whl",
);
```

**Install resolution order**: plain package names are first checked against the
bundled `pyodide-lock.json`. If the package is bundled, it loads offline. If not,
it falls through to micropip which requires `allowedHosts`.

**Security**: Installs are session-local and do not persist between sessions.
`file:` URIs are rejected to prevent host filesystem access. Network-based
installs (HTTP URLs, non-bundled package names) require `allowedHosts`.

### Use pip from shell commands

Shell commands like `pip install` are intercepted and routed through micropip
automatically. This means LLM agents and scripts can use the familiar pip
workflow:

```js
await session.run("pip install pyyaml");
await session.run('python3 -c "import yaml; print(yaml.dump({\"key\": \"value\"}))"');
```

Supported forms: `pip install`, `pip3 install`, `python3 -m pip install`.
Only the `install` subcommand is supported. Flags like `-q` and `--upgrade`
are ignored; package names are extracted and passed to micropip.

### List directory contents

```js
const dir = await session.listDir("/workspace");
console.log(dir.output); // file-per-line listing
```

### Limit execution budget

Set `stepBudget` to cap the number of VM steps. This prevents runaway commands
from consuming unbounded resources:

```js
const session = await createNodeSession({ stepBudget: 100_000 });
```

A budget of `0` (the default) means unlimited.

### Use a custom asset directory

By default, assets are resolved from the package's `assets/` directory. Override
this to use a separately downloaded or cached Pyodide build:

```js
const session = await createNodeSession({
  assetDir: "/path/to/custom/pyodide-dist",
});
```

## Reference

### `createNodeSession(options?): Promise<WasmshSession>`

Create a session backed by a Node.js child process running the wasmsh host.

**Options:**

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `assetDir` | `string` | package `assets/` | Path to Pyodide distribution directory |
| `nodeExecutable` | `string` | `process.execPath` | Path to Node.js binary |
| `stepBudget` | `number` | `0` (unlimited) | VM step budget per command |
| `initialFiles` | `Array<{path, content}>` | `[]` | Files to seed before init |
| `allowedHosts` | `string[]` | `[]` | Hostnames allowed for network access |

### `createBrowserWorkerSession(options): Promise<WasmshSession>`

Create a session backed by a browser Web Worker.

**Options:**

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `assetBaseUrl` | `string` | **(required)** | URL prefix for Pyodide assets |
| `worker` | `Worker` | auto-created | Pre-existing Worker instance |
| `stepBudget` | `number` | `0` (unlimited) | VM step budget per command |
| `initialFiles` | `Array<{path, content}>` | `[]` | Files to seed before init |
| `allowedHosts` | `string[]` | `[]` | Hostnames allowed for network access |

### `WasmshSession`

Returned by both `createNodeSession` and `createBrowserWorkerSession`.

| Method | Returns | Description |
|--------|---------|-------------|
| `run(command)` | `Promise<RunResult>` | Execute a shell command |
| `writeFile(path, content)` | `Promise<{events}>` | Write a `Uint8Array` to the sandbox |
| `readFile(path)` | `Promise<ReadFileResult>` | Read a file as `Uint8Array` |
| `listDir(path)` | `Promise<ListDirResult>` | List directory entries |
| `installPythonPackages(reqs, opts?)` | `Promise<InstallResult>` | Install Python wheel(s) into the sandbox |
| `close()` | `Promise<void>` | Shut down the session and release resources |

### `RunResult`

| Field | Type | Description |
|-------|------|-------------|
| `stdout` | `string` | Decoded stdout output |
| `stderr` | `string` | Decoded stderr output |
| `output` | `string` | Combined `stdout + stderr` |
| `exitCode` | `number \| null` | Exit code, or `null` if unavailable |
| `events` | `unknown[]` | Raw protocol events |

### Constants

| Export | Value | Description |
|--------|-------|-------------|
| `DEFAULT_WORKSPACE_DIR` | `"/workspace"` | The fixed sandbox root |

### Helper functions

| Function | Returns | Description |
|----------|---------|-------------|
| `resolveAssetPath(...segments)` | `string` | Absolute path into the package `assets/` directory |
| `resolveNodeHostPath()` | `string` | Path to `node-host.mjs` |
| `resolveBrowserWorkerPath()` | `URL` | URL to `browser-worker.js` |

## Explanation

### What this is

`wasmsh-pyodide` packages the [wasmsh](https://github.com/mayflower/wasmsh)
shell runtime linked into a custom [Pyodide](https://pyodide.org) build. The
result is a single WebAssembly module that provides both a bash-compatible shell
and a CPython interpreter sharing the same in-process POSIX filesystem.

### What this is not

This is **not** a Linux container. There is no kernel, no real process isolation,
and no OS-level package manager. The command set is the wasmsh utility suite (88
commands including jq, awk, rg, fd, diff, tar, gzip) plus `python`/`python3`
and `pip`/`pip3` (backed by micropip). System binaries like `apt`, `docker`, or
`systemctl` are not available.

### How it works

1. **Node.js mode**: `createNodeSession` spawns a child process running
   `node-host.mjs`, which boots the Pyodide/Emscripten module and communicates
   via JSON-RPC over stdin/stdout.

2. **Browser mode**: `createBrowserWorkerSession` creates a Web Worker running
   `browser-worker.js`, which loads the Pyodide WASM module and communicates via
   `postMessage`.

Both modes use the same wasmsh JSON protocol (`Init`, `Run`, `WriteFile`,
`ReadFile`, `ListDir`, `Close`) and produce the same event types (`Stdout`,
`Stderr`, `Exit`, `Diagnostic`, `Version`).

### Filesystem

All commands execute against a shared virtual filesystem. In Node.js mode this
is an Emscripten MemoryFS. In browser mode it is also an Emscripten MemoryFS
inside the Worker. The filesystem is ephemeral — it exists only for the lifetime
of the session.

The sandbox root is always `/workspace`. Files seeded via `initialFiles` or
`writeFile` are available to both bash and Python immediately.

### Bundled packages

Some compiled Python packages are bundled with the runtime in the local
`pyodide-lock.json`. These packages can be installed offline without network
access. Currently bundled:

- **DuckDB** (`duckdb` v1.5.0) — in-process SQL analytics database

The bundled wheel lives in `packages/npm/wasmsh-pyodide/assets/`. The lockfile
at `packages/npm/wasmsh-pyodide/assets/pyodide-lock.json` indexes it. The asset
packaging script (`tools/pyodide/package-runtime-assets.mjs`) preserves both
the wheel file and lockfile across Pyodide rebuilds.

#### Bumping or adding bundled packages

1. Place the new `.whl` in `packages/npm/wasmsh-pyodide/assets/`
2. Add/update the entry in `packages/npm/wasmsh-pyodide/assets/pyodide-lock.json`
   (name, version, file_name, sha256, imports, depends)
3. Run `just package-pyodide-runtime` to copy to the Python package
4. Run `just test-e2e-pyodide-node` to verify

For DuckDB specifically, matching wheels are published at
[xlwings/duckdb-pyodide](https://github.com/xlwings/duckdb-pyodide) for each
Pyodide release. Choose the wheel matching the repo's pinned Pyodide version
and Python ABI (see `tools/pyodide/versions.env`).
