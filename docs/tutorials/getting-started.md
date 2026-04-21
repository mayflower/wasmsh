# Getting Started with wasmsh (Rust embedding)

This tutorial walks you through building wasmsh from source and embedding
it in a Rust program. By the end you will have a small Rust binary that
seeds the VFS, runs a shell pipeline, and reads the result back.

> **Looking for the JS or Python on-ramp?** Use the
> [JavaScript / Node.js quickstart](javascript-quickstart.md) or
> [Python quickstart](python-quickstart.md) instead. They are faster and
> require no Rust toolchain.

## Prerequisites

- Rust 1.89 or later (pinned via `rust-toolchain.toml`)
- Git
- Optional: Emscripten 4.0.9 (only needed for the Pyodide build target)

## Step 1: Clone and Build

```bash
git clone https://github.com/mayflower/wasmsh
cd wasmsh
cargo build --workspace
```

The workspace contains 15 crates (plus 2 emcc-only crates excluded from the
default workspace). All should compile without errors.

## Step 2: Run the Tests

```bash
cargo test --workspace
# or, faster:
just test
```

This runs the unit, integration, and TOML conformance suites — 1400+ tests
in total, including ~550 declarative `.toml` shell tests under `tests/suite/`.

## Step 3: Run your first command

wasmsh is not a traditional shell binary. It's a library that exposes a
`WorkerRuntime` and you drive it via protocol messages. (For the
*reasoning* behind this design, see
[Architecture](../explanation/architecture.md). For now we just use it.)

Add `wasmsh-runtime` and `wasmsh-protocol` to your `Cargo.toml`:

```toml
[dependencies]
wasmsh-runtime  = "0.5"
wasmsh-protocol = "0.5"
```

Then in `src/main.rs`:

```rust
use wasmsh_runtime::WorkerRuntime;
use wasmsh_protocol::{HostCommand, WorkerEvent};

fn main() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 100_000,
        allowed_hosts: vec![],
    });

    let events = rt.handle_command(HostCommand::Run {
        input: "echo hello world".into(),
    });

    for event in &events {
        match event {
            WorkerEvent::Stdout(data) => print!("{}", String::from_utf8_lossy(data)),
            WorkerEvent::Exit(code) => println!("[exit: {code}]"),
            _ => {}
        }
    }
}
```

Run it:

```bash
cargo run
```

You should see:

```
hello world
[exit: 0]
```

## Step 4: Build a small pipeline that processes a file

Replace the body of `main` with:

```rust
fn main() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 100_000,
        allowed_hosts: vec![],
    });

    // 1. Seed a CSV file inside the sandbox.
    rt.handle_command(HostCommand::WriteFile {
        path: "/data/scores.csv".into(),
        data: b"name,score\nalice,42\nbob,17\ncarol,99\n".to_vec(),
    });

    // 2. Run a pipeline against it.
    let events = rt.handle_command(HostCommand::Run {
        input: "tail -n +2 /data/scores.csv | cut -d, -f2 | sort -n | tail -1".into(),
    });

    let mut stdout = Vec::new();
    let mut exit = -1;
    for evt in &events {
        match evt {
            WorkerEvent::Stdout(d) => stdout.extend_from_slice(d),
            WorkerEvent::Exit(c) => exit = *c,
            _ => {}
        }
    }

    println!("highest score: {}", String::from_utf8_lossy(&stdout).trim());
    println!("exit: {exit}");
}
```

`cargo run` again. Output:

```
highest score: 99
exit: 0
```

What you just did:

- Seeded a CSV file in the in-process VFS via `WriteFile`.
- Ran a four-stage pipeline (`tail`, `cut`, `sort`, `tail`) entirely
  in-process — no host commands were spawned.
- Pulled stdout out of the event vector and parsed it.

## Step 5: Read a file written by the script

Add this after the pipeline:

```rust
rt.handle_command(HostCommand::Run {
    input: "echo 'top: carol (99)' > /data/result.txt".into(),
});

let events = rt.handle_command(HostCommand::ReadFile {
    path: "/data/result.txt".into(),
});

for evt in events {
    if let WorkerEvent::Stdout(bytes) = evt {
        println!("file: {}", String::from_utf8_lossy(&bytes).trim());
    }
}
```

You should see `file: top: carol (99)`.

The shell wrote to `/data/result.txt` via redirection; the host pulled it
back via `ReadFile`. Both sides see the same file because the VFS is
owned by the runtime.

## Recap

You have:

- Built the workspace with `cargo build`.
- Run the test suite.
- Initialised a `WorkerRuntime`, run a shell command, processed events.
- Seeded the VFS, run a four-stage pipeline against it, and read the
  result back.

That is the full embedding lifecycle. Everything else is variation on
this theme.

A runnable version of the same shape lives at
[`examples/rust/`](../../examples/rust/) — `cargo run` from that
directory builds against the local workspace and prints the output
shown above.

## Where to go next

| If you want to … | Read |
|------------------|------|
| See this as a standalone cargo package | [`examples/rust/`](../../examples/rust/) |
| Drive wasmsh from JS or Python instead | [JavaScript quickstart](javascript-quickstart.md) / [Python quickstart](python-quickstart.md) |
| Build the standalone Web Worker bundle | [Embedding wasmsh](../guides/embedding.md) and `just build-standalone` |
| Build the Pyodide target | [Pyodide integration](../guides/pyodide-integration.md) and `just build-pyodide` |
| Add a new builtin or utility | [Adding a command](../guides/adding-commands.md) |
| Look up a syntax or builtin | [Reference](../reference/index.md) |
| Understand how it all fits together | [Architecture](../explanation/architecture.md) |
| Diagnose a runtime error | [Troubleshooting](../guides/troubleshooting.md) |
