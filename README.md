# wasmsh

**Bash-compatible shell runtime in Rust, compiled to WebAssembly. Runs in browsers, inside Pyodide, and as a horizontally-scaled sandbox pool on Kubernetes — all from one codebase.**

[![CI](https://img.shields.io/badge/CI-passing-brightgreen)](.github/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![crates.io](https://img.shields.io/crates/v/wasmsh-runtime.svg)](https://crates.io/crates/wasmsh-runtime)
[![npm](https://img.shields.io/npm/v/@mayflowergmbh/wasmsh-pyodide.svg)](https://www.npmjs.com/package/@mayflowergmbh/wasmsh-pyodide)

## What it is

A sandbox for LLM agents that need a shell, Python, and a filesystem without giving the model host access. Bash with 88 utilities (grep, sed, awk, jq, tar, curl, …), Python 3.13 with pip/micropip for pure-Python packages, a virtual filesystem — all running in-process as WebAssembly, with no OS processes and no network unless explicitly allowed.

Three deployment modes from one core:

| Target | When | Entry point |
|-|-|-|
| **Standalone** (`wasm32-unknown-unknown`) | browser Web Worker, offline | [`crates/wasmsh-browser`](crates/wasmsh-browser/) |
| **Pyodide** (`wasm32-unknown-emscripten`) | Node or browser, Python sharing the VFS | [`packages/npm/wasmsh-pyodide`](packages/npm/wasmsh-pyodide/) |
| **Scalable** (Kubernetes) | multi-tenant agent platforms | [`deploy/helm/wasmsh`](deploy/helm/wasmsh/) |

## Why it's a good fit for Deep Agents

### Secure by construction

LLM-generated shell commands are adversarial input. wasmsh is built so a bad `rm -rf /` or a curl to an exfil host cannot escape the sandbox:

- **WASM boundary.** No syscalls, no `std::fs`, no host `exec` in any shipped profile. The wasm module only sees what the embedder hands it.
- **Capability-based VFS.** Every session gets an isolated in-memory filesystem; nothing on the host is visible unless the embedder mounts it.
- **Network allowlist.** `curl` / `wget` route through a host-mediated broker that enforces a per-session hostname allowlist. Empty list = no network.
- **Step budgets.** Every command runs with a bounded step count; runaway loops and fork-bombs terminate deterministically.
- **Per-session V8 isolation** (scalable path). Each session is its own worker with a capped heap; one session cannot starve its neighbours.
- **Clean-room provenance.** No GPL code in the core — behavior-compatible with bash, but a fresh implementation, so no licence contamination for downstream embedders.

Full surface in [docs/reference/sandbox-and-capabilities.md](docs/reference/sandbox-and-capabilities.md); threat model and design choices in [docs/explanation/design-decisions.md](docs/explanation/design-decisions.md).

### Fast and dense

No containers, no VMs, no OS processes per session. Starting a sandbox is a wasm snapshot restore, not a `docker run`:

- **~300 ms cold spawn**, **~6 ms snapshot restore** once the template worker is warm
- **~1.5 ms** per warm bash command, **~3 ms** per warm `python3 -c` round-trip through the dispatcher
- **~80 MB RSS** per active session in steady state (stock Pyodide + bash)

That makes per-node density the ceiling instead of CPU. Rough sizing on a 64 GB / 40-core node: **~500–800 warm sessions** for typical agent workloads, **~100 session creates/s** burst throughput. See [docs/guides/performance-testing.md#sizing-a-scalable-deployment](docs/guides/performance-testing.md#sizing-a-scalable-deployment) for the benchmark and full capacity table — and re-run `just bench-dispatcher-compose` on your own hardware before committing to a size.

## Use with LangChain Deep Agents

Two interchangeable backend classes with the identical `BaseSandbox` surface — upgrading from laptop to cluster is a one-line import change:

| Ecosystem | Package | In-process | Scalable |
|-|-|-|-|
| npm | [`@mayflowergmbh/langchain-wasmsh`](packages/npm/langchain-wasmsh) | `WasmshSandbox.createNode()` | `WasmshRemoteSandbox.create({ dispatcherUrl })` |
| Python | [`langchain-wasmsh`](packages/python/langchain-wasmsh) | `WasmshSandbox()` | `WasmshRemoteSandbox(dispatcher_url)` |

```typescript
import { createDeepAgent } from "deepagents";
import { WasmshSandbox } from "@mayflowergmbh/langchain-wasmsh";

const sandbox = await WasmshSandbox.createNode();
const agent = createDeepAgent({ backend: sandbox });
```

```python
from deepagents import create_deep_agent
from langchain_wasmsh import WasmshSandbox

agent = create_deep_agent(backend=WasmshSandbox())
```

Full integration guide (remote variant, deployment topology, operational knobs): [docs/integrations/langchain-wasmsh.md](docs/integrations/langchain-wasmsh.md). Runnable examples: [`examples/deepagent-typescript/`](examples/deepagent-typescript/), [`examples/deepagent-python/`](examples/deepagent-python/).

## Install

| Registry | Package | Install |
|-|-|-|
| crates.io | `wasmsh-runtime` | `cargo add wasmsh-runtime` |
| npm | `@mayflowergmbh/wasmsh-pyodide` | `npm i @mayflowergmbh/wasmsh-pyodide` |
| PyPI | `wasmsh-pyodide-runtime` | `pip install wasmsh-pyodide-runtime` |
| Containers | `ghcr.io/mayflower/wasmsh-{dispatcher,runner}` | `docker pull` |

Pre-built tarballs and image digests: [GitHub Releases](https://github.com/mayflower/wasmsh/releases). Build from source: `just ci` (Rust), `just build-standalone`, `just build-pyodide`.

## Docs

| | |
|-|-|
| **Start here** | [Tutorials](docs/tutorials/index.md): [Rust](docs/tutorials/getting-started.md), [JavaScript](docs/tutorials/javascript-quickstart.md), [Python](docs/tutorials/python-quickstart.md) |
| **Deep Agents** | [Integration guide](docs/integrations/langchain-wasmsh.md) (in-process + remote, both languages) |
| **Deploy** | [Helm chart](deploy/helm/wasmsh/README.md), [snapshot-runner architecture](docs/explanation/snapshot-runner.md), [runner runbook](docs/how-to/runner-runbook.md) |
| **Tune** | [Performance testing & sizing](docs/guides/performance-testing.md), [dispatcher API](docs/reference/dispatcher-api.md), [runner metrics](docs/reference/runner-metrics.md) |
| **How-to** | [Embedding](docs/guides/embedding.md), [Pyodide integration](docs/guides/pyodide-integration.md), [Adding a command](docs/guides/adding-commands.md), [Troubleshooting](docs/guides/troubleshooting.md) |
| **Reference** | [Shell syntax](docs/reference/shell-syntax.md), [builtins](docs/reference/builtins.md), [utilities](docs/reference/utilities.md), [protocol](docs/reference/protocol.md), [sandbox & capabilities](docs/reference/sandbox-and-capabilities.md), [supported features](SUPPORTED.md) |
| **Explanation** | [Architecture](docs/explanation/architecture.md), [design decisions](docs/explanation/design-decisions.md), [ADRs](docs/adr/) |

## Acknowledgements

The Pyodide integration would not be possible without the outstanding work of the [Pyodide](https://pyodide.org/) team. They brought CPython to WebAssembly and built an ecosystem that makes running Python in the browser practical and reliable. wasmsh links directly into their Emscripten module, sharing the interpreter and filesystem — a testament to how well-designed their architecture is. Thank you to everyone who contributes to Pyodide.

## License

[Apache-2.0](LICENSE)
