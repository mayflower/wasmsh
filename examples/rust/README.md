# wasmsh Rust Example

Minimal `cargo run` demonstration of embedding `wasmsh-runtime` directly from Rust — no WebAssembly host, no JSON bridge, no container.

Useful when you want bash-compatible scripting inside a native CLI or test harness without any of the deployment infrastructure that the other examples use.

## Run (from this directory)

```bash
cargo run
```

Expected output:

```
hello from rust example
pipeline demo:
ONE
THREE
TWO
```

## Code

See [`src/main.rs`](src/main.rs). Three calls make up the whole API:

```rust
use wasmsh_protocol::{HostCommand, WorkerEvent};
use wasmsh_runtime::WorkerRuntime;

let mut runtime = WorkerRuntime::new();

runtime.handle_command(HostCommand::Init {
    step_budget: 100_000,
    allowed_hosts: vec![],   // empty list = no outbound network
});

for event in runtime.handle_command(HostCommand::Run {
    input: "echo hello".to_string(),
}) {
    if let WorkerEvent::Stdout(bytes) = event {
        print!("{}", String::from_utf8_lossy(&bytes));
    }
}
```

## For your own project

In-tree this example uses `path = "..."` dependencies so edits to the workspace crates flow through immediately. To use it in a downstream crate, swap to the crates.io versions:

```toml
[dependencies]
wasmsh-runtime = "0.6"
wasmsh-protocol = "0.6"
```

## Beyond `Run`

`HostCommand` has a lot more shape than one-shot execution — `StartRun` + `PollRun` for progressive execution, `Signal` for SIGINT/SIGTERM delivery, `WriteFile`/`ReadFile` for the virtual filesystem, `Cancel` for step-budget-respecting interruption. See the [protocol reference](../../docs/reference/protocol.md) for the full surface.

## Related

- Pyodide in-process (Node, Python, browser) — [`examples/deepagent-typescript/`](../deepagent-typescript/), [`examples/deepagent-python/`](../deepagent-python/), [`examples/deepagent-browser/`](../deepagent-browser/)
- Scalable HTTP client against the dispatcher — [`examples/deepagent-kubernetes/`](../deepagent-kubernetes/)
- Raw wasm-pack from JS/TS — [`examples/web/`](../web/), [`examples/typescript/`](../typescript/)
