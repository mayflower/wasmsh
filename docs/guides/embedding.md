# Embedding wasmsh in Your Application

How to integrate the wasmsh runtime into a host application. There are
three supported embedding paths, depending on your stack:

| If your host is … | Use |
|-------------------|-----|
| Rust              | This page (`wasmsh-runtime` crate) |
| Node.js, browser, TypeScript | [Pyodide integration](pyodide-integration.md) (npm package) |
| Python            | [Python quickstart](../tutorials/python-quickstart.md) (`wasmsh-pyodide-runtime` package) |

The rest of this page covers the Rust embedding path.

## As a Rust Library

The supported embedding API lives in the platform-agnostic
`wasmsh-runtime` crate. The `wasmsh-browser` crate is a thin wasm-bindgen
adapter and should only be depended on when targeting
`wasm32-unknown-unknown`.

```toml
[dependencies]
wasmsh-runtime  = "0.5"
wasmsh-protocol = "0.5"
```

For local workspace builds use `path = "crates/wasmsh-runtime"` instead.

### Initialize the Runtime

```rust
use wasmsh_runtime::WorkerRuntime;
use wasmsh_protocol::{HostCommand, WorkerEvent};

let mut rt = WorkerRuntime::new();
rt.handle_command(HostCommand::Init {
    step_budget: 100_000,
    allowed_hosts: vec![],
});
```

The `step_budget` limits how many VM steps a single execution can take.
Set to `0` for unlimited. `allowed_hosts` is the network capability
allowlist used by `curl` and `wget` — leave it empty to disable network
access entirely. See ADR-0021 for the supported pattern syntax.

### Execute Commands

```rust
let events = rt.handle_command(HostCommand::Run {
    input: "echo hello".into(),
});
```

### Process Events

Events are returned as `Vec<WorkerEvent>`:

```rust
for event in events {
    match event {
        WorkerEvent::Stdout(data) => { /* stdout bytes */ }
        WorkerEvent::Stderr(data) => { /* stderr bytes */ }
        WorkerEvent::Exit(code) => { /* exit status */ }
        WorkerEvent::Diagnostic(level, msg) => { /* runtime diagnostic */ }
        WorkerEvent::FsChanged(path) => { /* VFS file changed */ }
        WorkerEvent::Version(v) => { /* protocol version */ }
    }
}
```

### Manage the Virtual Filesystem

```rust
// Write a file
rt.handle_command(HostCommand::WriteFile {
    path: "/data/input.csv".into(),
    data: b"a,b,c\n1,2,3\n".to_vec(),
});

// Read a file
let events = rt.handle_command(HostCommand::ReadFile {
    path: "/data/input.csv".into(),
});

// List a directory
let events = rt.handle_command(HostCommand::ListDir {
    path: "/data".into(),
});
```

### Cancel Execution

```rust
rt.handle_command(HostCommand::Cancel);
```

## Execution Limits

Configure limits via `HostCommand::Init`:

- `step_budget`: Maximum VM steps per execution (0 = unlimited)
- `allowed_hosts`: Network allowlist for `curl` / `wget`. Empty disables
  network access. Patterns: exact host, wildcard (`*.example.com`), IP, or
  host with port.

The VM also tracks output bytes and can emit diagnostics when limits are
approached.

## Persistent State

The runtime maintains state between `Run` commands:

- Shell variables persist across invocations
- VFS files persist across invocations
- Functions defined in one command are callable in the next
- The working directory persists

To reset, send a new `Init` command. Calling `Init` a second time
re-creates the VM and clears all of the above.

## Adding custom commands (`ExternalCommandHandler`)

`WorkerRuntime` accepts a callback that is consulted *after* runtime
intercepts, builtins, functions, and utilities, and *before* the runtime
emits "command not found". Use this to expose host-side commands to the
shell — for example, a database query, an API call, or in the Pyodide
case, the in-process Python interpreter.

```rust
use wasmsh_runtime::{WorkerRuntime, ExternalCommandResult};

let mut rt = WorkerRuntime::new();
rt.handle_command(HostCommand::Init {
    step_budget: 100_000,
    allowed_hosts: vec![],
});

rt.set_external_handler(Box::new(|name, argv, stdin| {
    match name {
        "say-hello" => Some(ExternalCommandResult {
            exit_code: 0,
            stdout: format!("hello, {}\n", argv.first().map(|s| s.as_str()).unwrap_or("world")).into_bytes(),
            stderr: vec![],
        }),
        _ => None,   // fall through to "command not found"
    }
}));

let events = rt.handle_command(HostCommand::Run {
    input: "say-hello alice".into(),
});
// stdout: "hello, alice\n", exit 0
```

The handler signature is:

```rust
type ExternalCommandHandler =
    Box<dyn FnMut(&str, &[String], Option<&[u8]>) -> Option<ExternalCommandResult>>;
```

- `name`: the command name as the script wrote it.
- `argv`: the expanded argument vector (excluding `name`).
- `stdin`: the piped or here-doc input bytes, if any.
- Return `Some(result)` to handle the command, or `None` to fall through.

The Pyodide adapter installs an `ExternalCommandHandler` that dispatches
`python` and `python3` straight into `PyRun_SimpleString`. See
`crates/wasmsh-pyodide/src/python_cmd.rs` for the production example.

## Output and streaming model

`handle_command` returns a `Vec<WorkerEvent>` synchronously. There is no
async streaming inside one call; the entire script runs (subject to
`step_budget`) and then returns its events. This is fine for most
embedders because:

- Step budgets bound the wall time of any single `Run`.
- The host can break long workflows into multiple `Run` calls.
- Output bytes are tracked and capped, so a runaway `yes` will not OOM
  the host.

If you need progressive output during a single command, the options are:

- Split the user's intent into multiple `Run` calls (e.g. one per line).
- Run the runtime in a worker thread and use `Cancel` from the main
  thread when you have enough output.
- Wait for the upstream issue tracking streaming events to land.

`Cancel` is cooperative: it sets a flag that the VM checks at every
instruction boundary. The in-flight `Run` call returns shortly after
with whatever events were produced before the flag was observed.

## Threading

`WorkerRuntime` is `!Sync` (it owns mutable state) but `Send`. You can
move it across threads but you cannot share it between them without
external synchronisation. Typical patterns:

- One runtime per worker thread.
- One runtime guarded by a `Mutex` if multiple tasks need to share it.
- Spawn a runtime per session and let the host route requests to the
  right session.

## Security model

The runtime sandboxes scripts but it does not sandbox the host. Some
guidance:

- **Always set a `step_budget`** unless you fully trust the input.
  `0` (unlimited) is a footgun for any embedder accepting untrusted
  scripts.
- **Default `allowed_hosts` to empty.** Add hosts only when the script
  needs network access, and prefer narrow patterns (`api.example.com`,
  not `*.example.com`).
- **Treat `ExternalCommandHandler` as part of your attack surface.**
  Anything reachable from the handler is reachable from a malicious
  script. Validate inputs, rate-limit, and sandbox the host-side
  resources you expose.
- **Do not trust filesystem paths.** A script can attempt path traversal
  via `..` or absolute paths. The VFS confines reads/writes to the
  in-process filesystem, but an `ExternalCommandHandler` that touches the
  host filesystem must do its own validation.
- **Cap the wasm module memory** at the host level. The runtime cannot
  prevent a script from allocating large strings inside the wasm heap.

For the full enforcement story, see
[Sandbox and capabilities](../reference/sandbox-and-capabilities.md).

## See Also

- [Worker protocol reference](../reference/protocol.md) — the message
  shape returned from every `handle_command` call.
- [Sandbox and capabilities](../reference/sandbox-and-capabilities.md) —
  what the sandbox enforces and what it does not.
- [Adding a command](adding-commands.md) — when to add a builtin /
  utility / runtime intercept instead of an `ExternalCommandHandler`.
- [Pyodide integration](pyodide-integration.md) — for the JS / Python
  embedding paths.
