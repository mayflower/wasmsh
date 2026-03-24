# Getting Started with wasmsh

This tutorial walks you through building wasmsh from source, running your first shell commands, and understanding the execution model.

## Prerequisites

- Rust 1.75 or later
- Git

## Step 1: Clone and Build

```bash
git clone https://github.com/user/wasmsh
cd wasmsh
cargo build --workspace
```

You should see all 14 crates compile without errors.

## Step 2: Run the Tests

```bash
cargo test --workspace
```

This runs 288 Rust tests and 237 TOML-based shell conformance tests. All should pass.

## Step 3: Understand the Execution Model

wasmsh is not a traditional shell binary. It's a library that exposes a `WorkerRuntime` — designed to run inside a browser Web Worker. You interact with it through protocol messages:

```rust
use wasmsh_browser::WorkerRuntime;
use wasmsh_protocol::{HostCommand, WorkerEvent};

// Create and initialize the runtime
let mut rt = WorkerRuntime::new();
rt.handle_command(HostCommand::Init { step_budget: 100_000 });

// Execute a shell command
let events = rt.handle_command(HostCommand::Run {
    input: "echo hello world".into(),
});

// Process the events
for event in &events {
    match event {
        WorkerEvent::Stdout(data) => {
            print!("{}", String::from_utf8_lossy(data));
        }
        WorkerEvent::Exit(code) => {
            println!("Exit: {}", code);
        }
        _ => {}
    }
}
```

## Step 4: Try Some Shell Features

The runtime supports real shell scripts:

```rust
let events = rt.handle_command(HostCommand::Run {
    input: r#"
        for i in 1 2 3; do
            echo "Item $i"
        done
    "#.into(),
});
// Output: Item 1\nItem 2\nItem 3\n
```

Variable expansion, pipelines, and command substitution all work:

```rust
let events = rt.handle_command(HostCommand::Run {
    input: "echo $(uname) running on $(uname -m)".into(),
});
// Output: wasmsh running on wasm32
```

Arrays, `[[ ]]`, arithmetic commands, and advanced expansion:

```rust
let events = rt.handle_command(HostCommand::Run {
    input: r#"
        # Arrays and [[ ]]
        fruits=(apple banana cherry)
        for f in "${fruits[@]}"; do
            if [[ $f == b* ]]; then
                echo "found: $f"
            fi
        done

        # Arithmetic
        for (( i=1; i<=5; i++ )); do
            (( sum += i ))
        done
        echo "sum=$sum"

        # Case modification and declare
        declare -u SHOUT="hello world"
        echo "$SHOUT"
    "#.into(),
});
// Output: found: banana\nsum=15\nHELLO WORLD\n
```

## Step 5: Use the Virtual Filesystem

Files live in an in-memory VFS. You can populate it via the protocol:

```rust
rt.handle_command(HostCommand::WriteFile {
    path: "/data/hello.txt".into(),
    data: b"Hello from VFS!\n".to_vec(),
});

let events = rt.handle_command(HostCommand::Run {
    input: "cat /data/hello.txt | wc -l".into(),
});
// Output: 1
```

## Next Steps

- Read the [Shell Syntax Reference](../reference/shell-syntax.md) for supported features
- See [How-to Guides](../guides/) for common tasks
- Explore the [Architecture](../explanation/architecture.md) to understand the pipeline
