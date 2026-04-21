# DeepAgents + wasmsh — Python Example

An LLM agent backed by wasmsh's sandboxed shell runtime. The agent gets `execute` (bash/python3), `read_file`, `write_file`, `edit_file`, `ls`, `grep`, and `glob` tools — all running inside a virtual machine with no host OS access.

Three runnable scripts in this directory cover the two deployment shapes:

| Script | Backend | When to use |
|-|-|-|
| [`basic.py`](basic.py) | `WasmshSandbox` (in-process) | Local development, single-machine use, no LLM |
| [`example.py`](example.py) | `WasmshSandbox` (in-process) | Full Deep Agent run with Anthropic |
| [`remote_basic.py`](remote_basic.py) | `WasmshRemoteSandbox` (HTTP → dispatcher) | Scalable Docker Compose / Kubernetes deployment |

## In-process setup

```bash
pip install -r requirements.txt
export ANTHROPIC_API_KEY=sk-ant-...        # only needed for example.py
python example.py                           # or: python basic.py
```

A `WasmshSandbox` is created — it spawns a Node.js subprocess running the wasmsh Pyodide runtime. A CSV file is seeded into the sandbox's virtual filesystem, `create_deep_agent` creates an LLM agent with the sandbox as backend, the agent analyzes the data using Python and shell tools, and the report is read back. All execution happens inside wasmsh's sandboxed environment — no host filesystem or process access.

## Remote setup (Docker Compose)

Point `WasmshRemoteSandbox` at a running wasmsh dispatcher. The easiest way to get one is the stack in [`deploy/docker/`](../../deploy/docker/README.md) — identical sandbox semantics as the in-process backend, but the sandbox lives in a container (or pool of containers) on a different host.

```bash
# 1. bring the dispatcher + runner stack up (from the repo root)
docker compose -f deploy/docker/compose.yml up -d --wait

# 2. run the sandbox against the forwarded dispatcher port
pip install -r requirements.txt
WASMSH_DISPATCHER_URL=http://127.0.0.1:8080 python remote_basic.py

# 3. teardown
docker compose -f deploy/docker/compose.yml down
```

Scale the runner pool out with `--scale runner=N` on `docker compose up` — the dispatcher load-balances by free restore capacity across all replicas.

For Kubernetes-based deployments see the [kubernetes example](../deepagent-kubernetes/README.md) — same script, different way to reach the dispatcher.

## Packages

- [`langchain-wasmsh`](https://github.com/mayflower/wasmsh/tree/main/packages/python/langchain-wasmsh) — wasmsh sandbox backend for LangChain Deep Agents (`WasmshSandbox` + `WasmshRemoteSandbox`)
- [`wasmsh-pyodide-runtime`](https://pypi.org/project/wasmsh-pyodide-runtime/) — wasmsh Pyodide runtime assets (only needed for the in-process backend)
- [`deepagents`](https://github.com/langchain-ai/deepagents) — LLM agent framework
