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

## `WasmshInterpreterMiddleware` — persistent Python REPL as an agent tool

The Python package ships an `AgentMiddleware` that exposes the sandbox
as a single `py_eval` tool, mirroring the shape of
[`langchain-quickjs`'s `CodeInterpreterMiddleware`](https://docs.langchain.com/oss/python/deepagents/interpreters)
but with a real WebAssembly-isolated sandbox underneath. State —
variables, imports, defined functions — persists across calls and
across agent turns via a globals-pickle snapshot stored in private
agent state.

> **TypeScript equivalent.** Per LangChain's partner-package policy the
> TS counterpart (`createWasmshInterpreterMiddleware`,
> `WasmshFilesystemBackend`, skills loader, `WasmshLogger`) lives in
> [`deepagentsjs/libs/providers/wasmsh`](https://github.com/langchain-ai/deepagentsjs/tree/main/libs/providers/wasmsh)
> rather than in this repo. The wire protocol (`host_call` /
> `host_call_result`) is identical and served by the same Node host
> binary shipped in `@mayflowergmbh/wasmsh-pyodide`. The npm
> `@mayflowergmbh/langchain-wasmsh` package in this repo only ships
> `WasmshSandbox` + `WasmshRemoteSandbox`.

```python
from deepagents import create_deep_agent
from langchain_wasmsh import WasmshInterpreterMiddleware

agent = create_deep_agent(
    model="claude-sonnet-4-6",
    middleware=[WasmshInterpreterMiddleware()],
)
```

### Programmatic tool calling (PTC)

Selected agent tools can be exposed inside the sandbox as
`tools.<snake_name>` awaitables, so user Python can fan out, loop,
branch, and chain tool calls within one `py_eval` invocation — without
extra LLM turns:

```python
from langchain_core.tools import tool

@tool
def lookup_user(user_id: int) -> dict:
    """Return a small user record."""
    return {"id": user_id, "name": "alice"}

agent = create_deep_agent(
    model="claude-sonnet-4-6",
    tools=[lookup_user],
    middleware=[WasmshInterpreterMiddleware(ptc=["lookup_user"])],
)
```

The model may then emit:

```python
import asyncio
users = await asyncio.gather(*[
    tools.lookup_user(user_id=i) for i in [1, 2, 3]
])
print(users)
```

PTC calls round-trip through the sandbox's `host_call` /
`host_call_result` protocol (see [ADR-0031](../adr/adr-0031-ptc-suspend-resume.md)).
On the host, each call is dispatched with a child `ToolRuntime` derived
from the outer `py_eval` invocation, so a nested tool receives the same
state, context, store, config, and stream writer the parent had — with its
own `tool_call_id` so tracing can correlate the sub-call. Tools declaring
`ToolRuntime`, `InjectedState`, `InjectedStore`, or `InjectedToolCallId`
all work; arguments the generated program supplies for an injected
parameter are discarded before injection, so model-authored code cannot
forge identity or state. Sync and async tools both run, from sync and async
agent invocations alike.

Results keep their shape: a tool returning a list of dicts arrives in the
interpreter as a list of dicts. `ToolMessage` and `Command` envelopes are
unwrapped to their payload and Pydantic models keep their fields; only
values with no JSON counterpart become strings, and only at the leaf.

Two bounds apply:

- **`max_ptc_calls`** (default 256, per evaluation) caps nested calls so a
  loop in generated code cannot issue unbounded tool calls in one turn.
  The call past the limit fails without reaching the tool. Pass `None` to
  disable.
- **The allowlist is the permission boundary.** PTC bypasses the regular
  `ToolNode` path, so per-tool `interrupt_on` approval is *not* enforced
  for nested calls. Gate the outer `py_eval` tool with `interrupt_on`,
  expose only tools that are safe to call unattended, or leave PTC off.

The interpreter's own tool is never exposed, even if you name it in the
allowlist, so a program cannot recurse into itself.

PTC currently requires the **in-process** backend;
`WasmshRemoteSandbox.run_ptc` raises `NotImplementedError` until the
dispatcher SSE channel ships (Phase 2 of ADR-0031).

**Observability.** When a PTC tool raises, the middleware converts the
exception into an envelope so the model can recover — but the original
stack disappears in that conversion. Each adapter surfaces the dropped
context the same way:

- **Python**: stdlib `logging` on the `langchain_wasmsh._repl` logger,
  `WARNING` level, `exc_info=True`, with structured
  `extra={"wasmsh_ptc_call_id": ..., "wasmsh_ptc_tool": ...}`. Attach
  your usual handler (`logging.basicConfig`, Sentry, structlog) to the
  `langchain_wasmsh` namespace.
- **TypeScript** (in `deepagentsjs/libs/providers/wasmsh`): pass a
  `WasmshLogger` to `createWasmshInterpreterMiddleware({ logger })`.
  Implement `ptcToolError({ tool, callId, args, error })` and
  `skillLoadError({ skill, error })` — the middleware swallows any
  throw from the logger itself, so a buggy hook cannot break the agent
  loop.

### Python skills

Pair `WasmshInterpreterMiddleware` with a `SkillsMiddleware` and a shared
`BackendProtocol`, and Python sources under each skill directory become
importable inside the REPL as `import skills.<name>`:

```python
from deepagents import create_deep_agent
from deepagents.backends import StateBackend
from deepagents.middleware import SkillsMiddleware
from langchain_wasmsh import WasmshInterpreterMiddleware

backend = StateBackend()
agent = create_deep_agent(
    model="claude-sonnet-4-6",
    backend=backend,
    middleware=[
        SkillsMiddleware(backend=backend, sources=["/skills/user/"]),
        WasmshInterpreterMiddleware(skills_backend=backend),
    ],
)
```

The middleware scans the user's code for `import skills.<name>` /
`from skills.<name> import …` references and stages the matching skill
directory into the sandbox VFS on first use. An `__init__.py` is
synthesised when the skill author didn't ship one.

**Every regular file under the skill directory is staged**, not just
Python: shell scripts, SQL, templates, fixtures, and binary assets all
travel, with nested structure and bytes preserved. Bounds apply and are
loud rather than silent — 2 MiB per file, 8 MiB per bundle, 512 files.
Dot-prefixed files are skipped, which is upstream `glob` behaviour rather
than a wasmsh choice.

**Importability is opt-in, not assumed.** `my-skill` maps to
`skills.my_skill` because that mapping is unambiguous. A name that does not
produce a valid Python identifier — upstream permits lowercase Unicode
letters — is simply *not importable* unless the skill declares an alias.
Discovery, progressive disclosure, and reading its instructions all keep
working either way. Two optional keys live under upstream's free-form
`metadata` mapping:

```yaml
---
name: sales-report
description: Build the weekly sales report
metadata:
  wasmsh.python_package: sales_report   # import alias
  wasmsh.python_module: lib/report.py   # re-exported from __init__.py
---
```

`allowed-tools` in skill frontmatter is descriptive metadata, not an
authorization boundary; tool exposure inside the interpreter is governed by
the PTC allowlist.

**Reload semantics come from upstream and are not papered over.**
`SkillsMiddleware` loads `skills_metadata` once per thread and keeps it in
private state. Editing a skill persists immediately and a **new** thread
sees it; an existing checkpointed thread keeps the view it started with.
Staged bundles are cached by content fingerprint, so changed bytes re-stage
and unchanged ones cost nothing. There is no watcher and no polling loop.

## Durable memory: route it to a store, not the VFS

The wasmsh VFS is an **execution workspace, not durable memory**. A local
sandbox's files live inside its host subprocess and disappear when that
process exits; a remote session's files last only as long as the dispatcher
keeps that session. Neither is a cross-process store.

So anything that must outlive a session — user profiles, user/agent/org
memory, skills, policies — belongs in upstream
[`StoreBackend`](https://docs.langchain.com/oss/python/deepagents/memory)
over a real LangGraph `BaseStore`, routed by prefix through a
`CompositeBackend` whose *default* is wasmsh:

```python
from dataclasses import dataclass

from deepagents import FilesystemPermission, create_deep_agent
from deepagents.backends import CompositeBackend, StoreBackend
from langgraph.checkpoint.memory import InMemorySaver
from langgraph.store.memory import InMemoryStore

from langchain_wasmsh import WasmshSandbox


@dataclass
class AgentContext:
    user_id: str
    assistant_id: str
    org_id: str


store = InMemoryStore()      # use your production store in deployment
sandbox = WasmshSandbox()

backend = CompositeBackend(
    default=sandbox,          # executable workspace + transient artifacts
    routes={
        "/profiles/": StoreBackend(
            store=store,
            namespace=lambda rt: ("deepagents", "profile", rt.context.user_id),
        ),
        "/memories/user/": StoreBackend(
            store=store,
            namespace=lambda rt: ("deepagents", "memory-user", rt.context.user_id),
        ),
        "/memories/agent/": StoreBackend(
            store=store,
            namespace=lambda rt: ("deepagents", "memory-agent", rt.context.assistant_id),
        ),
        "/policies/": StoreBackend(
            store=store,
            namespace=lambda rt: ("deepagents", "policy", rt.context.org_id),
        ),
    },
)

agent = create_deep_agent(
    model=model,
    backend=backend,
    context_schema=AgentContext,
    checkpointer=InMemorySaver(),
    store=store,
    memory=[
        "/profiles/user.md",
        "/memories/user/AGENTS.md",
        "/memories/agent/AGENTS.md",
        "/policies/AGENTS.md",
    ],
    permissions=[
        FilesystemPermission(
            operations=["write"], paths=["/policies/**"], mode="deny",
        ),
    ],
)
```

A persistent user profile is ordinary user-scoped memory at
`/profiles/user.md` — there is no separate profile database or profile
middleware. It must not hold secrets, tokens, or transient session state.

Note that SDK **harness/provider profiles** (`HarnessProfile`,
`ProviderProfile`) are a different concept entirely: configuration
registries for prompt assembly, tool visibility, middleware, and model
construction. They are not user memory.

### Filesystem permissions apply to routed prefixes, not the workspace

`deepagents==0.7.4` refuses `permissions` outright when the backend can run
commands, unless **every** rule path sits under a `CompositeBackend` route.
That is upstream being honest rather than restrictive: a rule guarding a
path the agent can also reach through `execute` would be advisory, not
enforced. In a wasmsh deployment, permissions therefore protect the routed
store prefixes, and the wasmsh workspace stays an unguarded execution area.
`delete` is classified as a **write**, and a recursive delete of a parent is
refused when it would take a protected subtree with it.

## `WasmshFilesystemBackend` — a namespace/path adapter

When you want several routes to share **one** wasmsh VFS,
`WasmshFilesystemBackend` adapts a sandbox to `BackendProtocol` with a
`namespace=` prefix:

```python
from deepagents.backends import CompositeBackend, StateBackend
from langchain_wasmsh import WasmshFilesystemBackend, WasmshSandbox

sandbox = WasmshSandbox()
backend = CompositeBackend(
    default=StateBackend(),
    routes={
        "/scratch/": WasmshFilesystemBackend(sandbox, namespace="/scratch"),
    },
)
```

Unlike using the sandbox directly, it does not expose `execute()` — handing
a routed prefix a shell would make the prefix meaningless.

**What `namespace=` is.** Every path is joined onto the prefix and resolved
with `posixpath.normpath`; anything that lands outside is rejected with
`WasmshNamespaceEscapeError` (a `PermissionError` subclass) before any I/O.
That stops an agent-controlled `file_path` like `../../skills/secret.py` on
the ordinary file tools, which is what it exists for. Containment is checked
on results too, so a misbehaving sandbox cannot surface a foreign path as if
the caller could address it.

**What `namespace=` is not: tenant isolation.** The check is lexical and
does not survive symlinks — the sandbox resolves those at the POSIX layer,
so anything with shell access (`execute`, the interpreter, a custom tool)
can link out of the namespace and read through it. When principals do not
trust each other, give each one its own sandbox session, or put the
sensitive data in a non-executable store namespace.

## Deep Agents 0.7.4 behaviour worth knowing

Things that changed in Deep Agents 0.7, or that wasmsh does differently from
the obvious guess. Each is asserted by a test rather than only described.

- **`write_file` overwrites.** Pre-0.7 it refused to clobber an existing
  file; `BaseSandbox.write` now creates parent directories and writes.
- **`delete` is recursive and counts as a write.** It removes the path plus
  everything under it, and write-deny rules cover it.
- **`execute(timeout=N)` is a real deadline** — and enforcing it destroys the
  session. Pyodide runs synchronously with no cancellation point, so the
  local sandbox kills its host and the remote runner terminates its worker;
  both report exit code 124 (GNU `timeout(1)`), and the session refuses
  further work. Use `step_budget` for a bound that leaves the session alive.
  `timeout` and `step_budget` are different controls: one is wall clock, the
  other is VM steps.
- **PTC allowlists are the permission boundary.** Nested calls do not
  traverse an approval-capable path, so `interrupt_on` never fires for them.
- **Filesystem permissions need routed prefixes** when the backend can
  execute — see above.
- **Namespace routing is not tenant isolation** — see above.
- **Local VFS state is transient.** Durable memory, profiles, and skills
  belong in a `StoreBackend` or an explicitly tested persistent remote
  volume.
- **Node gives weaker network enforcement than Deno.** Under Deno,
  `allowed_hosts` maps to `--allow-net`, an OS-level restriction on the
  subprocess. Under Node it is enforced only at the wasmsh application
  layer. Install Deno for defence in depth.
- **Capture/offload stays disabled.** `enable_capture_offload` remains
  `False`; the inherited fallback runs the command exactly once and reports
  `offloaded=False`. It will be enabled only after the offload wrapper
  passes command-level conformance on the wasmsh shell.
- **Remote PTC is unavailable.** `WasmshRemoteSandbox.run_ptc` raises
  `NotImplementedError` until ADR-0031 Phase 2 lands the dispatcher SSE
  channel; it fails loudly rather than degrading.
- **TodoList middleware is no longer in the default 0.7 stack.** Add it
  explicitly if an example depends on it.

### `create_deep_agent` compatibility matrix

Every row is exercised by a test that builds a real graph with wasmsh
active. See `packages/python/langchain-wasmsh/tests/integration_tests/`.

| Interface | Status | Notes |
|---|---|---|
| `model` | Supported | Pre-built instance or string spec; provider profiles honoured. |
| `tools` | Supported | Sync, async, structured, and `ToolRuntime`-aware tools. A raising tool propagates rather than becoming an error `ToolMessage` — that is 0.7.4 policy. |
| `system_prompt` | Supported | Caller prompt first, then profile base/suffix, memory, skills, interpreter — each once. |
| `middleware` | Supported | Merged by `.name`; a matching name replaces the built-in slot. Two user middlewares sharing a name are rejected upstream. |
| `subagents` | Supported | Declarative sync, compiled, and the default general-purpose subagent. See the inheritance notes below. |
| `skills` | Supported | Upstream `SkillsMiddleware`; layered sources, last source wins. |
| `memory` | Supported | Durability comes from the routed backend, not from wasmsh. |
| `permissions` | Supported, scoped | Only for `CompositeBackend` routes when the backend can execute. |
| `backend` | Supported | wasmsh as default plus `StoreBackend` routes through `CompositeBackend`. |
| `interrupt_on` | Supported | Ordinary tools pause and resume normally. Nested PTC calls are the documented exception. |
| `response_format` | Supported | Structured output with wasmsh middleware and backend active. |
| `state_schema` | Supported | Custom fields survive; the REPL snapshot stays private. |
| `context_schema` | Supported | Reaches `StoreBackend` namespaces, tools, subagents, and nested PTC calls. |
| `checkpointer` | Supported | Same-thread replay, cross-thread isolation, and resumption from a reconstructed graph. |
| `store` | Supported | Cross-thread and cross-sandbox persistence; runtime-injected store in tools and PTC. |
| `cache` | Supported | Smoke-tested with the wasmsh backend active. |
| `debug` / `name` | Supported | Passed through unchanged. |
| Deep Agents Code provider | Not shipped | Optional integration; blocked on a `bash -c` setup-shell mismatch. |
| Remote PTC | Unsupported | ADR-0031 Phase 2. |

**Subagent inheritance** (0.7.4, asserted in `test_agent_subagents.py`): a
declarative subagent shares the backend, inherits the parent's *ordinary*
tools only when it omits `tools` (file and shell tools come from
`FilesystemMiddleware` on every stack regardless), inherits permission rules
only when it omits `permissions` — an explicit list replaces them rather
than merging — and inherits **no memory at all**, because `SubAgent` has no
`memory` field. A `CompiledSubAgent` is used exactly as supplied: no
ambient backend, permissions, memory, skills, or interpreter reach it. Top-
level custom middleware is not inherited either, so an interpreter in a
subagent stack must be added there deliberately.

## Reference

Both packages expose the same public surface.  See the per-ecosystem READMEs
for the full API:

- [`packages/python/langchain-wasmsh/README.md`](../../packages/python/langchain-wasmsh/README.md)
- [`packages/npm/langchain-wasmsh/README.md`](../../packages/npm/langchain-wasmsh/README.md)

Deeper material on the PTC channel:

- [ADR-0031: PTC suspend/resume over the wasmsh-pyodide JSON-RPC channel](../adr/adr-0031-ptc-suspend-resume.md)
- [`docs/explanation/ptc-suspend-resume.md`](../explanation/ptc-suspend-resume.md) — full wire spec and phasing.

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
