# DeepAgents + wasmsh — Node.js Example

An LLM agent backed by wasmsh's sandboxed shell runtime. The agent gets `execute` (bash/python3), `read_file`, `write_file`, `edit_file`, `ls`, `grep`, and `glob` tools — all running inside a virtual machine with no host OS access.

Three runnable scripts in this directory cover the two deployment shapes:

| Script | Backend | When to use |
|-|-|-|
| [`basic.ts`](basic.ts) | `WasmshSandbox` (in-process) | Local development, single-machine use, no LLM |
| [`example.ts`](example.ts) | `WasmshSandbox` (in-process) | Full Deep Agent run with Anthropic |
| [`remote-basic.ts`](remote-basic.ts) | `WasmshRemoteSandbox` (HTTP → dispatcher) | Scalable Docker Compose / Kubernetes deployment |

## In-process setup

```bash
npm install
export ANTHROPIC_API_KEY=sk-ant-...         # only needed for example.ts
npm run example                              # or: npm run basic
```

A `WasmshSandbox` is created with a virtual `/workspace` filesystem, a CSV file is seeded into it, `createDeepAgent` wires an LLM agent to the sandbox, and the agent analyzes the data using Python and shell tools. All execution happens inside wasmsh's sandboxed environment — no host filesystem or process access.

## Remote setup (Docker Compose)

Point `WasmshRemoteSandbox` at a running wasmsh dispatcher. The easiest way to get one is the stack in [`deploy/docker/`](../../deploy/docker/README.md) — identical sandbox semantics as the in-process backend, but the sandbox lives in a container (or pool of containers) on a different host.

```bash
# 1. bring the dispatcher + runner stack up (from the repo root)
docker compose -f deploy/docker/compose.yml up -d --wait

# 2. run the sandbox against the forwarded dispatcher port
npm install
WASMSH_DISPATCHER_URL=http://127.0.0.1:8080 npm run remote-basic

# 3. teardown
docker compose -f deploy/docker/compose.yml down
```

Scale the runner pool out with `--scale runner=N` on `docker compose up` — the dispatcher load-balances by free restore capacity across all replicas.

For Kubernetes-based deployments see the [kubernetes example](../deepagent-kubernetes/README.md) — same script, different way to reach the dispatcher.

## Packages

- [`@mayflowergmbh/langchain-wasmsh`](https://www.npmjs.com/package/@mayflowergmbh/langchain-wasmsh) — wasmsh sandbox provider for DeepAgents (`WasmshSandbox` + `WasmshRemoteSandbox`)
- [`@mayflowergmbh/wasmsh-pyodide`](https://www.npmjs.com/package/@mayflowergmbh/wasmsh-pyodide) — wasmsh Pyodide runtime (only needed for the in-process backend)
- [`deepagents`](https://github.com/langchain-ai/deepagentsjs) — LLM agent framework
