# Pyodide Integration

How to drive wasmsh from a Pyodide host (Node.js or browser). This guide
covers the npm package layout, the host adapter API, custom commands,
package installation via `pip`/`micropip`, and the JSON protocol on the
wire.

If you just want to run a few commands, start with the
[JavaScript quickstart](../tutorials/javascript-quickstart.md) instead.

## Package layout

`@mayflowergmbh/wasmsh-pyodide` ships:

```
@mayflowergmbh/wasmsh-pyodide/
├── index.js              Node host entrypoint (createNodeSession)
├── browser.js            Browser host entrypoint (createBrowserWorkerSession)
├── browser-worker.js     Web Worker bundle that loads Pyodide + wasmsh
├── node-host.mjs         Standalone Node host (used by the Python bridge)
├── index.d.ts            TypeScript definitions for both adapters
└── assets/               Custom Pyodide build with wasmsh-pyodide linked in
```

The same package serves both Node and browser. The two adapters share the
underlying JSON protocol; only the transport differs (subprocess pipes vs
`postMessage`).

## Booting a Node session

```javascript
import { createNodeSession } from "@mayflowergmbh/wasmsh-pyodide";

const session = await createNodeSession({
  stepBudget: 100_000,
  allowedHosts: ["pypi.org", "*.pythonhosted.org"],
  initialFiles: [
    { path: "/workspace/seed.txt", content: new TextEncoder().encode("hello\n") },
  ],
  timeoutMs: 5 * 60_000,   // per-call timeout (default 5 minutes)
});
```

The full options surface (from the `.d.ts`):

| Option            | Default      | Notes |
|-------------------|--------------|-------|
| `assetDir`        | bundled      | Override the Pyodide asset directory. Use this to load a custom build. |
| `nodeExecutable`  | `process.execPath` | Override the node binary used to spawn the host. |
| `stepBudget`      | unbounded    | Maximum VM steps per `run()` call. |
| `initialFiles`    | none         | Files to write into the VFS before the session is exposed. |
| `allowedHosts`    | `[]` (deny)  | Network allowlist patterns. See [ADR-0021](../adr/adr-0021-network-capability.md). |
| `timeoutMs`       | `300_000`    | Per-call timeout. `0` disables. |

## Booting a browser session

```javascript
import { createBrowserWorkerSession } from "@mayflowergmbh/wasmsh-pyodide";

const session = await createBrowserWorkerSession({
  assetBaseUrl: new URL("./node_modules/@mayflowergmbh/wasmsh-pyodide/assets/", import.meta.url).toString(),
  stepBudget: 100_000,
  allowedHosts: [],
});
```

The browser adapter spawns a Web Worker that loads the same Pyodide
bundle. The host page interacts with it via `postMessage` under the hood;
the API surface is identical to the Node session.

## The session API

Both `createNodeSession()` and `createBrowserWorkerSession()` return a
`WasmshSession`:

```typescript
interface WasmshSession {
  run(command: string): Promise<RunResult>;
  writeFile(path: string, content: Uint8Array): Promise<{ events: unknown[] }>;
  readFile(path: string): Promise<ReadFileResult>;
  listDir(path: string): Promise<ListDirResult>;
  installPythonPackages(
    requirements: string | string[],
    options?: InstallPythonPackagesOptions,
  ): Promise<InstallResult>;
  close(): Promise<void>;
}

interface RunResult {
  events: unknown[];   // raw protocol events
  output: string;      // stdout + stderr concatenated
  stdout: string;
  stderr: string;
  exitCode: number | null;
}
```

The `events` field on each result is the raw `WorkerEvent` array from the
[worker protocol](../reference/protocol.md). The `stdout`, `stderr`,
`output`, and `exitCode` fields are the host adapter's convenience
projection.

## Installing Python packages

`pip install` is intercepted at the JS host and routed through micropip:

```javascript
const result = await session.installPythonPackages([
  "requests",
  "beautifulsoup4",
]);

console.log(result.installed);
// [ { requirement: 'requests', name: 'requests', version: '2.31.0' }, ... ]
```

Three requirement formats are supported:

- **PyPI name**: `"requests"` — resolved against PyPI. Requires
  `allowedHosts` to include `pypi.org` and `*.pythonhosted.org`.
- **HTTPS URL**: `"https://example.com/pkg-1.0-py3-none-any.whl"` —
  downloads and installs. Requires the host in `allowedHosts`.
- **In-sandbox path**: `"emfs:/path/to/local.whl"` — installs from a wheel
  already present in the Emscripten VFS. No network required. Useful for
  bundling private wheels.

Only pure-Python wheels (`py3-none-any`) are supported. Native wheels are
rejected (Pyodide cannot dlopen arbitrary native code).

`file:` URIs are rejected as a security measure to prevent host
filesystem reads.

## Adding custom commands (`ExternalCommandHandler`)

If you are embedding wasmsh in your own Rust application (not via the npm
package), you can register an `ExternalCommandHandler` to add commands
that the host wants to expose:

```rust
use wasmsh_runtime::{ExternalCommandResult, WorkerRuntime};

let mut rt = WorkerRuntime::new();

rt.set_external_handler(Box::new(|name, argv, stdin| {
    match name {
        "query-db" => Some(ExternalCommandResult {
            status: 0,
            stdout: format!("rows: {}\n", argv.len()).into_bytes(),
            stderr: vec![],
        }),
        _ => None,   // fall through to "command not found"
    }
}));
```

The handler is consulted *after* runtime intercepts, builtins, functions,
and utilities, and *before* the runtime emits "command not found". The
Pyodide adapter installs an internal handler that dispatches `python` and
`python3` to `PyRun_SimpleString` — see `crates/wasmsh-pyodide/src/python_cmd.rs`
for the production example.

## The JSON wire format

The Node and browser adapters serialise commands as JSON and pass them
through `wasmsh_runtime_handle_json` (the C ABI exported from
`wasmsh-pyodide`). The schema is `serde_json`'s default tagged
representation of `HostCommand` and `WorkerEvent`. See
[Worker protocol reference](../reference/protocol.md#json-wire-format) for
the exact shape and a worked example.

If you are writing a custom host adapter, the contract is:

1. Allocate a runtime with `wasmsh_runtime_new()`.
2. For each command, encode as JSON, call `wasmsh_runtime_handle_json()`,
   parse the returned JSON event array, free the returned string with
   `wasmsh_runtime_free_string()`.
3. Drop the runtime with `wasmsh_runtime_free()` when done.

There is no streaming inside a single call. If you need progressive output
during a long-running command, you have to break it into multiple `Run`
calls or use the `Cancel` mechanism described below.

## Cancellation

`Cancel` is cooperative. To interrupt a long-running script:

1. Issue `Run` from one task.
2. From another task, send `Cancel`. The runtime sets a cancellation token.
3. The first task's `Run` call observes the token at the next VM step and
   returns its event vector with an `Exit(130)` event.

In the Node adapter, `session.run()` is a Promise. To cancel it from the
host, send a separate `{"Cancel": null}` JSON message via the host's
internal message channel. The npm package does not currently expose a
`session.cancel()` method directly — track the upstream issue if you need
this.

## Sharing the filesystem with Python

The most powerful feature of the Pyodide build is that the shell and the
Python interpreter share the same VFS. A file written by `cat` is visible
to `open()` and vice versa:

```javascript
await session.writeFile("/data/scores.csv", new TextEncoder().encode(csv));
await session.run("python3 -c \"import csv; print(list(csv.DictReader(open('/data/scores.csv'))))\"");
await session.run("awk -F, 'NR>1 {sum+=$2} END {print sum}' /data/scores.csv");
```

All three operations see the same file. There is no marshalling, no
serialisation, no IPC.

## Boot performance

`createNodeSession()` does the following on the first call:

1. Spawn a Node subprocess.
2. Load the Pyodide module (the wasm + python stdlib bundle).
3. Initialise the wasmsh runtime inside the module.
4. Run any `initialFiles` writes.

The Pyodide load is the most expensive step. It is cached on disk after
the first run. Subsequent boots are noticeably faster but still take a
few seconds. If you are running many short scripts, prefer one
long-lived session over many short-lived ones.

## See Also

- [Worker protocol reference](../reference/protocol.md) for the underlying
  JSON message format.
- [Sandbox and capabilities](../reference/sandbox-and-capabilities.md) for
  what `stepBudget` and `allowedHosts` actually enforce.
- [Architecture: Dual-target](../explanation/architecture.md#dual-target-architecture)
  for the system-level picture of how Pyodide and wasmsh share a module.
- [ADR-0018: Pyodide same-module](../adr/ADR-0018-pyodide-same-module.md)
  for the design rationale.
- [ADR-0021: Network capability](../adr/adr-0021-network-capability.md) for
  the security model behind `allowedHosts` and `installPythonPackages`.
