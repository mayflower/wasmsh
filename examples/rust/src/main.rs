//! Minimal example: embed wasmsh-runtime directly from Rust.
//!
//! Drives the shell via `HostCommand::Init` + `HostCommand::Run` and
//! decodes the resulting `WorkerEvent` stream to stdout.  No WebAssembly
//! host, no JSON bridge, no container — just a library call graph.
//!
//! Useful for:
//!   - native CLI tools that want bash-compatible scripting without
//!     spawning `/bin/bash`
//!   - tests that need a deterministic in-memory shell
//!   - anywhere the other three paths (browser, Pyodide, scalable
//!     dispatcher) are too much infrastructure

use wasmsh_protocol::{HostCommand, WorkerEvent};
use wasmsh_runtime::WorkerRuntime;

fn main() {
    let mut runtime = WorkerRuntime::new();

    // Init configures the runtime; `allowed_hosts: vec![]` means no
    // outbound network access, which is the safe default for untrusted
    // input.  Raise `step_budget` for longer scripts.
    let _init_events = runtime.handle_command(HostCommand::Init {
        step_budget: 100_000,
        allowed_hosts: vec![],
    });

    // A small demonstration script exercising bash features that most
    // embedders will want: variables, expansion, pipelines, and a simple
    // loop.  Nothing in here touches the host filesystem or network.
    let script = r#"
name="rust example"
echo "hello from $name"
echo "pipeline demo:"
for word in one two three; do
    echo "$word"
done | tr '[:lower:]' '[:upper:]' | sort
"#;

    let events = runtime.handle_command(HostCommand::Run {
        input: script.to_string(),
    });

    let mut exit_code = 0;
    for event in events {
        match event {
            WorkerEvent::Stdout(bytes) => {
                print!("{}", String::from_utf8_lossy(&bytes));
            }
            WorkerEvent::Stderr(bytes) => {
                eprint!("{}", String::from_utf8_lossy(&bytes));
            }
            WorkerEvent::Exit(code) => {
                exit_code = code;
            }
            // Version announcement, diagnostics, filesystem change
            // notifications, and yield signals are all irrelevant for
            // this minimal driver — skip them quietly.
            _ => {}
        }
    }

    std::process::exit(exit_code);
}
