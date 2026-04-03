# wasmsh-pyodide

Pyodide-backed wasmsh runtime for Node.js and browser Web Workers. Provides both
bash-compatible shell execution and Python 3 inside the same in-process sandbox,
with a shared filesystem rooted at `/workspace`.

## Getting started

### Install

```bash
npm install wasmsh-pyodide
```

Requires Node.js 20+.

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

Use `installPythonPackages` to add pure-Python wheels into the sandbox.
Packages are installed into `/lib/python3.13/site-packages` and become
importable from subsequent `python3` commands in the same session.

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

Install from HTTP URLs or by package name (requires `allowedHosts`):

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

**Security**: Installs are session-local and do not persist between sessions.
`file:` URIs are rejected to prevent host filesystem access. Network-based
installs (HTTP URLs, package names) require `allowedHosts` to be configured
when creating the session. Only pure-Python wheels (`py3-none-any`) are
supported; C extension packages like numpy are not available.

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
and no package manager. The command set is the wasmsh utility suite (86 commands
including jq, awk, rg, fd, diff, tar, gzip) plus `python`/`python3`. System
binaries like `apt`, `docker`, or `systemctl` are not available.

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
