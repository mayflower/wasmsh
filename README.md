# wasmsh

**Bash-compatible shell runtime in Rust, compiled to WebAssembly. Runs in browsers and inside Pyodide — no server needed.**

[![CI](https://img.shields.io/badge/CI-passing-brightgreen)](.github/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![crates.io](https://img.shields.io/crates/v/wasmsh-runtime.svg)](https://crates.io/crates/wasmsh-runtime)
[![npm](https://img.shields.io/npm/v/@mayflowergmbh/wasmsh-pyodide.svg)](https://www.npmjs.com/package/@mayflowergmbh/wasmsh-pyodide)

## What it does

A sandboxed shell with 88 utilities (grep, sed, awk, jq, tar, curl, …), Python 3.13 with pip/micropip for installing pure-Python packages, and a virtual filesystem — all running in-process as WebAssembly. No OS processes, no network access unless explicitly allowed, step budgets to prevent runaway execution.

Four build targets from one codebase:
- **Standalone** (`wasm32-unknown-unknown`) — browser Web Worker
- **Pyodide** (`wasm32-unknown-emscripten`) — shell and Python share the same filesystem (JS-hosted via Node/browser)
- **Pyodide-WASI** — standalone no-JS same-module Pyodide artifact runnable under Wasmtime. In-memory C filesystem, host-provided HTTP fetch, embedded micropip. See [ADR-0031](docs/adr/adr-0031-pyodide-wasi-same-module-runtime.md).
- **Component Model** (`wasm32-wasip2`) — WASI P2 component exporting the same JSON `HostCommand` / `WorkerEvent` transport used by Pyodide through a thin `wasmsh:component/runtime` handle plus shared probe helpers. See [ADR-0030](docs/adr/adr-0030-wasmcloud-component-transport.md).

## Use with DeepAgents

wasmsh is a sandbox backend for [DeepAgents](https://github.com/langchain-ai/deepagentsjs). LLM agents get `execute`, `read_file`, `write_file`, `edit_file`, `ls`, `grep`, `glob` tools backed by the WASM sandbox.

```typescript
import { createDeepAgent } from "deepagents";
import { WasmshSandbox } from "@langchain/wasmsh";

const sandbox = await WasmshSandbox.createNode();
const agent = createDeepAgent({ backend: sandbox });
const result = await agent.invoke({
  messages: [{ role: "user", content: "Analyze data.csv and create a summary" }],
});
await sandbox.stop();
```

Also works in the browser (Web Worker, no backend needed) and from Python (`langchain-wasmsh`).

## Use directly

```rust
use wasmsh_runtime::WorkerRuntime;
use wasmsh_protocol::HostCommand;

let mut rt = WorkerRuntime::new();
rt.handle_command(HostCommand::Init {
    step_budget: 100_000,
    allowed_hosts: vec![],
});
let events = rt.handle_command(HostCommand::Run { input: "echo hello".into() });
// [Stdout(b"hello\n"), Exit(0)]
```

## Install

| Registry | Package | Install |
|----------|---------|---------|
| crates.io | `wasmsh-runtime` | `cargo add wasmsh-runtime` |
| npm | `@mayflowergmbh/wasmsh-pyodide` | `npm i @mayflowergmbh/wasmsh-pyodide` |
| PyPI | `wasmsh-pyodide-runtime` | `pip install wasmsh-pyodide-runtime` |

Pre-built tarballs: [GitHub Releases](https://github.com/mayflower/wasmsh/releases)

## Build from source

```bash
cargo build --workspace && cargo test --workspace   # Rust (1.89+)
just build-standalone                                # standalone wasm
just build-pyodide                                   # Pyodide wasm (needs emcc)
just build-component                                 # wasm32-wasip2 component
```

## Docs

| | |
|-|-|
| [Tutorials](docs/tutorials/index.md) | Step-by-step on-ramps for [JavaScript](docs/tutorials/javascript-quickstart.md), [Python](docs/tutorials/python-quickstart.md), and [Rust](docs/tutorials/getting-started.md) |
| [How-to Guides](docs/guides/index.md) | [Embedding](docs/guides/embedding.md), [Pyodide integration](docs/guides/pyodide-integration.md), [Adding a command](docs/guides/adding-commands.md), [Troubleshooting](docs/guides/troubleshooting.md) |
| [Reference](docs/reference/index.md) | [Shell syntax](docs/reference/shell-syntax.md), [builtins](docs/reference/builtins.md), [utilities](docs/reference/utilities.md), [protocol](docs/reference/protocol.md), [sandbox](docs/reference/sandbox-and-capabilities.md) |
| [Explanation](docs/explanation/index.md) | [Architecture](docs/explanation/architecture.md), [design decisions](docs/explanation/design-decisions.md), trade-offs |
| [ADRs](docs/adr/) | Architectural Decision Records |
| [Supported Features](SUPPORTED.md) | Complete syntax and command matrix |
| [Examples](examples/) | Standalone and TypeScript usage |

## Acknowledgements

The Pyodide integration would not be possible without the outstanding work of the [Pyodide](https://pyodide.org/) team. They brought CPython to WebAssembly and built an ecosystem that makes running Python in the browser practical and reliable. wasmsh links directly into their Emscripten module, sharing the interpreter and filesystem — a testament to how well-designed their architecture is. Thank you to everyone who contributes to Pyodide.

## License

[Apache-2.0](LICENSE)
