# wasmsh

**Bash-compatible shell runtime in Rust, compiled to WebAssembly. Runs in browsers and inside Pyodide — no server needed.**

[![CI](https://img.shields.io/badge/CI-passing-brightgreen)](.github/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![crates.io](https://img.shields.io/crates/v/wasmsh-runtime.svg)](https://crates.io/crates/wasmsh-runtime)
[![npm](https://img.shields.io/npm/v/@mayflowergmbh/wasmsh-pyodide.svg)](https://www.npmjs.com/package/@mayflowergmbh/wasmsh-pyodide)

## What it does

A sandboxed shell with 88 utilities (grep, sed, awk, jq, tar, curl, …), Python 3.13 with pip/micropip for installing pure-Python packages, and a virtual filesystem — all running in-process as WebAssembly. No OS processes, no network access unless explicitly allowed, step budgets to prevent runaway execution.

Three build targets from one codebase:
- **Standalone** (`wasm32-unknown-unknown`) — browser Web Worker
- **Pyodide** (`wasm32-unknown-emscripten`) — shell and Python share the same filesystem
- **Component Model** (`wasm32-wasip2`) — WASI P2 component exporting the same JSON `HostCommand` / `WorkerEvent` transport used by Pyodide through a thin `wasmsh:component/runtime` handle plus shared probe helpers. Reuses the same libc-backed filesystem path as Pyodide. See [ADR-0030](docs/adr/adr-0030-wasmcloud-component-transport.md).

## Use with LangChain Deep Agents

wasmsh is a sandbox backend for [LangChain Deep Agents](https://github.com/langchain-ai/deepagentsjs). LLM agents get `execute`, `read_file`, `write_file`, `edit_file`, `ls`, `grep`, `glob` tools backed by the WASM sandbox.

Adapter packages are Mayflower-maintained and live in this repo:

- **npm** — [`@mayflowergmbh/langchain-wasmsh`](packages/npm/langchain-wasmsh) (Node + browser)
- **Python** — [`langchain-wasmsh`](packages/python/langchain-wasmsh)

```typescript
import { createDeepAgent } from "deepagents";
import { WasmshSandbox } from "@mayflowergmbh/langchain-wasmsh";

const sandbox = await WasmshSandbox.createNode();
const agent = createDeepAgent({ backend: sandbox });
const result = await agent.invoke({
  messages: [{ role: "user", content: "Analyze data.csv and create a summary" }],
});
await sandbox.stop();
```

```python
from deepagents import create_deep_agent
from langchain_wasmsh import WasmshSandbox

sandbox = WasmshSandbox()
agent = create_deep_agent(backend=sandbox)
```

See [`docs/integrations/langchain-wasmsh.md`](docs/integrations/langchain-wasmsh.md) for the full integration guide.

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

## Scalable deployment (Kubernetes)

In addition to the in-process wasm targets, wasmsh ships a server-side
deployment path: a Rust **dispatcher** plus a Node + Pyodide **runner**
packaged as container images and installable via Helm. LLM agents or
other clients talk HTTP to the dispatcher, which routes sessions across a
horizontally-scaled pool of runner pods.

| Piece | Purpose |
|-|-|
| [`crates/wasmsh-dispatcher`](crates/wasmsh-dispatcher/) | axum HTTP service: session routing, affinity, capacity-aware scheduling |
| [`tools/runner-node`](tools/runner-node/) | Node runner: template worker, per-session restore, metrics |
| [`deploy/docker/Dockerfile.{dispatcher,runner}`](deploy/docker/) | Production images (`ghcr.io/mayflower/wasmsh-{dispatcher,runner}`) |
| [`deploy/helm/wasmsh`](deploy/helm/wasmsh/) | Helm chart with HPA, PDB, NetworkPolicy, optional ServiceMonitor |
| [`e2e/kind`](e2e/kind/) | Full-stack kind end-to-end test suite (`just test-e2e-kind`) |

Container images are built and pushed to GHCR automatically by the
`Release` workflow on `v*` tags; digests are attached to the GitHub
Release as `image-digests.json`. See
[docs/explanation/snapshot-runner.md](docs/explanation/snapshot-runner.md)
for architecture, [deploy/helm/wasmsh/README.md](deploy/helm/wasmsh/README.md)
for the chart surface, and [docs/how-to/runner-runbook.md](docs/how-to/runner-runbook.md)
for operations.

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
