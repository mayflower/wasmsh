# Embedding wasmsh in Your Application

How to integrate the wasmsh runtime into a Rust application or browser context.

## As a Rust Library

Add the browser crate as a dependency:

```toml
[dependencies]
wasmsh-browser = { path = "crates/wasmsh-browser" }
wasmsh-protocol = { path = "crates/wasmsh-protocol" }
```

### Initialize the Runtime

```rust
use wasmsh_browser::WorkerRuntime;
use wasmsh_protocol::{HostCommand, WorkerEvent};

let mut rt = WorkerRuntime::new();
rt.handle_command(HostCommand::Init { step_budget: 100_000 });
```

The `step_budget` limits how many VM steps a single execution can take. Set to `0` for unlimited.

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

The VM also tracks output bytes and can emit diagnostics when limits are approached.

## Persistent State

The runtime maintains state between `Run` commands:

- Shell variables persist across invocations
- VFS files persist across invocations
- Functions defined in one command are callable in the next
- The working directory persists

To reset, send a new `Init` command.
