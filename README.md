# wasmsh

**Bash-compatible shell runtime in Rust, compiled to WebAssembly. Runs in browsers and inside Pyodide — no server needed.**

[![CI](https://img.shields.io/badge/CI-passing-brightgreen)](.github/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![crates.io](https://img.shields.io/crates/v/wasmsh-runtime.svg)](https://crates.io/crates/wasmsh-runtime)
[![npm](https://img.shields.io/npm/v/@mayflowergmbh/wasmsh-pyodide.svg)](https://www.npmjs.com/package/@mayflowergmbh/wasmsh-pyodide)

## What it does

A sandboxed shell with 88 utilities (grep, sed, awk, jq, tar, curl, …), Python 3.13, and a virtual filesystem — all running in-process as WebAssembly. No OS processes, no network access unless explicitly allowed, step budgets to prevent runaway execution.

Two build targets from one codebase:
- **Standalone** (`wasm32-unknown-unknown`) — browser Web Worker
- **Pyodide** (`wasm32-unknown-emscripten`) — shell and Python share the same filesystem

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
rt.handle_command(HostCommand::Init { step_budget: 100_000 });
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
```

## Docs

| | |
|-|-|
| [Tutorials](docs/tutorials/) | Step-by-step guides to get started |
| [How-to Guides](docs/guides/) | Task-oriented recipes for common operations |
| [Reference](docs/reference/) | Shell syntax, builtins, utilities, protocol |
| [Explanation](docs/explanation/) | Architecture, design decisions, trade-offs |
| [ADRs](docs/adr/) | Architectural Decision Records |
| [Supported Features](SUPPORTED.md) | Complete syntax and command matrix |
| [Examples](examples/) | Standalone and TypeScript usage |

## License

[MIT](LICENSE)
