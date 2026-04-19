# wasmsh

**Bash-compatible shell runtime in Rust, compiled to WebAssembly. Runs in browsers and inside Pyodide — no server needed.**

[![CI](https://img.shields.io/badge/CI-passing-brightgreen)](.github/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![crates.io](https://img.shields.io/crates/v/wasmsh-runtime.svg)](https://crates.io/crates/wasmsh-runtime)
[![npm](https://img.shields.io/npm/v/@mayflowergmbh/wasmsh-pyodide.svg)](https://www.npmjs.com/package/@mayflowergmbh/wasmsh-pyodide)

## What it does

A sandboxed shell with 88 utilities (grep, sed, awk, jq, tar, curl, …), Python 3.13 with pip/micropip for installing pure-Python packages, and a virtual filesystem — all running in-process as WebAssembly. No OS processes, no network access unless explicitly allowed, step budgets to prevent runaway execution.

Two in-process build targets plus one scalable server-side path from one codebase:
- **Standalone** (`wasm32-unknown-unknown`) — browser Web Worker
- **Pyodide** (`wasm32-unknown-emscripten`) — shell and Python share the same filesystem
- **Scalable** (Kubernetes) — `wasmsh-dispatcher` (Rust, HTTP control plane) plus a pool of `wasmsh-runner` pods (Node + Pyodide) installed via the [Helm chart](deploy/helm/wasmsh/). Clients speak JSON/HTTP to the dispatcher. See [Scalable deployment](#scalable-deployment-kubernetes) below.

## Use with LangChain Deep Agents

wasmsh is a sandbox backend for [LangChain Deep Agents](https://github.com/langchain-ai/deepagentsjs). LLM agents get `execute`, `read_file`, `write_file`, `edit_file`, `ls`, `grep`, `glob` tools backed by the WASM sandbox.

Adapter packages are Mayflower-maintained and live in this repo. Each
ships two interchangeable backend classes — `WasmshSandbox` for
single-process use and `WasmshRemoteSandbox` for dispatcher-backed
Kubernetes deployments — with the identical `BaseSandbox` surface so
upgrading is a one-line import change:

| Ecosystem | Package | In-process | Scalable |
|-|-|-|-|
| npm | [`@mayflowergmbh/langchain-wasmsh`](packages/npm/langchain-wasmsh) | `WasmshSandbox.createNode()` / `.createBrowserWorker()` | `WasmshRemoteSandbox.create({ dispatcherUrl })` |
| Python | [`langchain-wasmsh`](packages/python/langchain-wasmsh) | `WasmshSandbox()` | `WasmshRemoteSandbox(dispatcher_url)` |

### In-process (single machine, no server)

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

### Scalable (Kubernetes, shared with other agents)

```typescript
import { WasmshRemoteSandbox } from "@mayflowergmbh/langchain-wasmsh";

const sandbox = await WasmshRemoteSandbox.create({
  dispatcherUrl: "http://wasmsh-dispatcher.wasmsh.svc.cluster.local:8080",
});
const agent = createDeepAgent({ backend: sandbox });
```

```python
from langchain_wasmsh import WasmshRemoteSandbox

sandbox = WasmshRemoteSandbox("http://wasmsh-dispatcher.wasmsh.svc.cluster.local:8080")
```

The remote backend talks HTTP to the `wasmsh-dispatcher` provisioned by
[`deploy/helm/wasmsh`](deploy/helm/wasmsh/); the dispatcher routes
sessions across a horizontally-scaled pool of `wasmsh-runner` pods with
session affinity and restore-capacity-aware scheduling. See
[Scalable deployment](#scalable-deployment-kubernetes) for the server
side.

Runnable examples (both include LLM-free and Deep Agent variants):

- Python: [`examples/deepagent-python/`](examples/deepagent-python/) — `basic.py`, `remote_basic.py`, `example.py`.
- TypeScript: [`examples/deepagent-typescript/`](examples/deepagent-typescript/) — `basic.ts`, `remote-basic.ts`, `example.ts`.

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
```

## Scalable deployment (Kubernetes)

In addition to the in-process wasm targets, wasmsh ships a server-side
deployment path: a Rust **dispatcher** plus a Node + Pyodide **runner**
packaged as container images and installable via Helm. Clients speak
JSON/HTTP to the dispatcher, which routes sessions across a
horizontally-scaled pool of runner pods.

| Piece | Purpose |
|-|-|
| [`crates/wasmsh-dispatcher`](crates/wasmsh-dispatcher/) | axum HTTP service: session routing, affinity, capacity-aware scheduling |
| [`tools/runner-node`](tools/runner-node/) | Node runner: template worker, per-session restore, metrics |
| [`deploy/docker/Dockerfile.{dispatcher,runner}`](deploy/docker/) | Production images (`ghcr.io/mayflower/wasmsh-{dispatcher,runner}`) |
| [`deploy/docker/compose.dispatcher-test.yml`](deploy/docker/compose.dispatcher-test.yml) | Single-host docker-compose stack for local smoke tests |
| [`deploy/helm/wasmsh`](deploy/helm/wasmsh/) | Helm chart with HPA, PDB, NetworkPolicy, optional ServiceMonitor |
| [`e2e/kind`](e2e/kind/) | Kind-based end-to-end suite (`just test-e2e-kind`) |
| [`e2e/dispatcher-compose`](e2e/dispatcher-compose/) | docker-compose-based end-to-end suite (`just test-e2e-dispatcher-compose`) |

Clients:

- **LangChain Deep Agents** — the `WasmshRemoteSandbox` class in both
  adapter packages is the officially-supported client. See
  [Use with LangChain Deep Agents](#use-with-langchain-deep-agents)
  above and the [integration guide](docs/integrations/langchain-wasmsh.md).
- **Anything else** — the dispatcher contract is plain JSON/HTTP,
  documented in [docs/reference/dispatcher-api.md](docs/reference/dispatcher-api.md).
  Write your own client in whatever stack you like.

Container images are built and pushed to GHCR automatically by the
`Release` workflow on `v*` tags; digests are attached to the GitHub
Release as `image-digests.json`.

The scalable path is exercised end-to-end on every PR by
[`.github/workflows/remote-sandbox-e2e.yml`](.github/workflows/remote-sandbox-e2e.yml),
which runs both the docker-compose and the kind+Helm variants through
the `WasmshRemoteSandbox` TypeScript and Python clients plus the
`langchain-tests` sandbox standard suite.

See [docs/explanation/snapshot-runner.md](docs/explanation/snapshot-runner.md)
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
