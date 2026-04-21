# langchain-wasmsh — LangChain Deep Agents sandbox backend

The `langchain-wasmsh` packages expose the wasmsh sandbox as a
[LangChain Deep Agents](https://docs.langchain.com/oss/python/deepagents/sandboxes)
backend.  Each ecosystem ships **two** interchangeable backends so an
agent can scale from a single laptop to a Kubernetes cluster with a
one-line import change — both classes implement the identical
`BaseSandbox` surface:

| Backend | Use for | Transport |
| --- | --- | --- |
| `WasmshSandbox` | Local development, CI, single-process agents, browser | In-process Pyodide/WASM over a Deno or Node subprocess (or Web Worker) |
| `WasmshRemoteSandbox` | Production, Kubernetes, shared agent fleets | JSON/HTTP to the wasmsh dispatcher + runner pool ([Helm chart](../../deploy/helm/wasmsh/)) |

Packages:

| Ecosystem | Package | Import | Source |
| --- | --- | --- | --- |
| Python | `langchain-wasmsh` | `from langchain_wasmsh import WasmshSandbox, WasmshRemoteSandbox` | [`packages/python/langchain-wasmsh`](../../packages/python/langchain-wasmsh) |
| npm | `@mayflowergmbh/langchain-wasmsh` | `import { WasmshSandbox, WasmshRemoteSandbox } from "@mayflowergmbh/langchain-wasmsh"` | [`packages/npm/langchain-wasmsh`](../../packages/npm/langchain-wasmsh) |

Both packages are Mayflower-maintained and live in this repository.  The
underlying Pyodide assets come from `wasmsh-pyodide-runtime` (Python) and
`@mayflowergmbh/wasmsh-pyodide` (npm).  The dispatcher + runner images
the remote backend talks to are published by `release.yml` to
`ghcr.io/mayflower/wasmsh-{dispatcher,runner}`.

## Why these packages are hosted here, not upstream

LangChain's current policy for new integrations is to publish them as
standalone packages under the maintainer's own organisation and submit a
docs-only PR upstream.  Following that guidance:

- Code lives in `mayflower/wasmsh` (this repo).
- Class names stay sandbox-shaped (`WasmshSandbox`), not agent-shaped.
- Package names are LangChain-style (`langchain-wasmsh`,
  `@mayflowergmbh/langchain-wasmsh`) so consumers recognise the integration
  role.

See the naming recommendation in the repository notes for the full reasoning.

## Python quickstart

```bash
pip install langchain-wasmsh deepagents langchain-anthropic
export ANTHROPIC_API_KEY=sk-ant-...
```

```python
from deepagents import create_deep_agent
from langchain_wasmsh import WasmshSandbox

backend = WasmshSandbox()
try:
    agent = create_deep_agent(
        model="claude-haiku-4-5-20251001",
        system_prompt="You are a coding assistant with bash and Python access.",
        backend=backend,
    )
    result = agent.invoke(
        {"messages": [{"role": "user", "content": "Compute fibonacci(10)"}]},
    )
    print(result["messages"][-1].content)
finally:
    backend.close()
```

Runnable examples:

- [`examples/deepagent-python/basic.py`](../../examples/deepagent-python/basic.py) — bash + Python, no LLM.
- [`examples/deepagent-python/example.py`](../../examples/deepagent-python/example.py) — full Deep Agent with CSV analysis (needs `ANTHROPIC_API_KEY`).

## npm quickstart

```bash
pnpm add @mayflowergmbh/langchain-wasmsh deepagents @langchain/anthropic
export ANTHROPIC_API_KEY=sk-ant-...
```

```ts
import { createDeepAgent } from "deepagents";
import { WasmshSandbox } from "@mayflowergmbh/langchain-wasmsh";

const sandbox = await WasmshSandbox.createNode();
try {
  const agent = createDeepAgent({
    model: "claude-haiku-4-5-20251001",
    systemPrompt: "You are a coding assistant with bash and Python access.",
    backend: sandbox,
  });
  const result = await agent.invoke({
    messages: [{ role: "user", content: "Compute fibonacci(10)" }],
  });
  console.log(result.messages.at(-1)?.content);
} finally {
  await sandbox.stop();
}
```

Runnable examples:

- [`examples/deepagent-typescript/basic.ts`](../../examples/deepagent-typescript/basic.ts) — minimal Node usage, no LLM.
- [`examples/deepagent-typescript/example.ts`](../../examples/deepagent-typescript/example.ts) — full Deep Agent, needs `ANTHROPIC_API_KEY`.
- [`examples/deepagent-browser/main.js`](../../examples/deepagent-browser/main.js) — fully in-browser agent, needs `ANTHROPIC_API_KEY`.

## What the sandbox provides

- Bash with 88 built-in utilities (`jq`, `awk`, `rg`, `fd`, `diff`, `tar`,
  `gzip`, `curl`, `wget`, …).
- `python` / `python3` via Pyodide — shares `/workspace` with bash.
- `pip install` intercepted and routed through `micropip` for pure-Python
  wheels and Pyodide-compatible compiled wheels.
- A deterministic, capability-based network model (`allowedHosts`).

This is not a Linux container.  If you need a full OS, use a container-based
backend such as `langchain-modal` or `langchain-daytona`.

## Reference

Both packages expose the same public surface.  See the per-ecosystem READMEs
for the full API:

- [`packages/python/langchain-wasmsh/README.md`](../../packages/python/langchain-wasmsh/README.md)
- [`packages/npm/langchain-wasmsh/README.md`](../../packages/npm/langchain-wasmsh/README.md)

## `WasmshRemoteSandbox` — Docker / Kubernetes backend

For production use the remote variant, which routes every sandbox call
through the wasmsh **dispatcher** (Axum HTTP service in
[`crates/wasmsh-dispatcher`](../../crates/wasmsh-dispatcher)) to a pool
of runner pods (Node + Pyodide baked into
[`deploy/docker/Dockerfile.runner`](../../deploy/docker/Dockerfile.runner)).
The Helm chart in [`deploy/helm/wasmsh`](../../deploy/helm/wasmsh)
provisions dispatcher, runners, HPA, and drain-aware rolling updates.
The HTTP contract is documented in
[`docs/reference/dispatcher-api.md`](../reference/dispatcher-api.md).

Both adapters ship a `WasmshRemoteSandbox` that implements the same
`BaseSandbox` surface as the in-process backend — switching is a
one-line import change.

### Python

```python
import os
from deepagents import create_deep_agent
from langchain_wasmsh import WasmshRemoteSandbox

backend = WasmshRemoteSandbox(os.environ["WASMSH_DISPATCHER_URL"])
try:
    agent = create_deep_agent(
        model="claude-haiku-4-5-20251001",
        system_prompt="You are a coding assistant with bash and Python access.",
        backend=backend,
    )
    result = agent.invoke(
        {"messages": [{"role": "user", "content": "Compute fibonacci(10)"}]},
    )
    print(result["messages"][-1].content)
finally:
    backend.close()
```

### TypeScript

```ts
import { createDeepAgent } from "deepagents";
import { WasmshRemoteSandbox } from "@mayflowergmbh/langchain-wasmsh";

const sandbox = await WasmshRemoteSandbox.create({
  dispatcherUrl: process.env.WASMSH_DISPATCHER_URL!,
});
try {
  const agent = createDeepAgent({
    model: "claude-haiku-4-5-20251001",
    systemPrompt: "You are a coding assistant with bash and Python access.",
    backend: sandbox,
  });
  const result = await agent.invoke({
    messages: [{ role: "user", content: "Compute fibonacci(10)" }],
  });
  console.log(result.messages.at(-1)?.content);
} finally {
  await sandbox.stop();
}
```

### Try it locally

The repo ships a production-oriented docker-compose stack (dispatcher
plus one or more runners, tunable via `--scale runner=N`) in
[`deploy/docker/`](../../deploy/docker/README.md):

```bash
docker compose -f deploy/docker/compose.yml up -d --wait
WASMSH_DISPATCHER_URL=http://127.0.0.1:8080 \
  uv --project packages/python/langchain-wasmsh \
  run python examples/deepagent-python/remote_basic.py
WASMSH_DISPATCHER_URL=http://127.0.0.1:8080 \
  pnpm --filter wasmsh-deepagent-typescript-example run remote-basic
docker compose -f deploy/docker/compose.yml down
```

The thinner `compose.dispatcher-test.yml` next to it is used by the
dispatcher-compose e2e suite; prefer `compose.yml` for anything
outside that loop.

### End-to-end tests

Two self-contained e2e suites exercise `WasmshRemoteSandbox` against
both deployment targets:

```bash
just test-e2e-dispatcher-compose   # docker-compose stack (~2 min)
just test-e2e-kind                 # kind cluster + helm install (~7 min)
```

Each orchestrator builds the dispatcher + runner images, brings up the
stack (or cluster), runs the **TypeScript** `WasmshRemoteSandbox` test
suite, then runs the **Python** `SandboxIntegrationTests` standard
suite through the same dispatcher endpoint, and tears everything down.

- Docker-compose: [`e2e/dispatcher-compose`](../../e2e/dispatcher-compose)
- Kubernetes (kind): [`e2e/kind`](../../e2e/kind)
- CI coverage: [`.github/workflows/remote-sandbox-e2e.yml`](../../.github/workflows/remote-sandbox-e2e.yml)

Runnable examples:

- [`examples/deepagent-python/remote_basic.py`](../../examples/deepagent-python/remote_basic.py) — minimal Python usage, no LLM.
- [`examples/deepagent-typescript/remote-basic.ts`](../../examples/deepagent-typescript/remote-basic.ts) — minimal TypeScript usage, no LLM.
- [`examples/deepagent-kubernetes/`](../../examples/deepagent-kubernetes/) — Helm install + three ways to reach the dispatcher (port-forward, ingress, in-cluster DNS), reusing the two scripts above.

### In production

Deploy the dispatcher + runners with:

```bash
helm install wasmsh ./deploy/helm/wasmsh --namespace wasmsh --create-namespace
```

Point the client at the dispatcher's in-cluster service:

```python
WasmshRemoteSandbox("http://wasmsh-dispatcher.wasmsh.svc.cluster.local:8080")
```

For non-default authentication needs, pass `headers={"Authorization": ...}`
— the dispatcher itself expects to run behind a trusted mesh; add your
own auth proxy if you need one.
