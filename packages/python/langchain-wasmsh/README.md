# langchain-wasmsh

wasmsh sandbox backend for [LangChain Deep Agents](https://github.com/langchain-ai/deepagents).
Mayflower-maintained, hosted under [`mayflower/wasmsh`](https://github.com/mayflower/wasmsh).

Runs bash and Python 3 inside an in-process Pyodide/WASM sandbox locally — no
container or cloud service required.

## Examples

Runnable examples live in the repository at
[`examples/deepagent-python`](https://github.com/mayflower/wasmsh/tree/main/examples/deepagent-python):

- [`basic.py`](https://github.com/mayflower/wasmsh/blob/main/examples/deepagent-python/basic.py) —
  minimal non-LLM usage (bash + Python sharing `/workspace`).
- [`example.py`](https://github.com/mayflower/wasmsh/blob/main/examples/deepagent-python/example.py) —
  full Deep Agent analyzing a CSV, requires `ANTHROPIC_API_KEY`.

See also the
[integration guide](https://github.com/mayflower/wasmsh/blob/main/docs/integrations/langchain-wasmsh.md).

## Getting started

### Requirements

- Python 3.11+
- Deno 2+ (preferred) or Node.js 20+ — Deno provides sandboxed permissions for defense-in-depth

### Install

```bash
pip install langchain-wasmsh
```

### Create an agent with a wasmsh sandbox

```python
from deepagents import create_deep_agent
from langchain_wasmsh import WasmshSandbox

backend = WasmshSandbox()
try:
    agent = create_deep_agent(
        model="claude-sonnet-4-5-20250929",
        system_prompt="You are a coding assistant with bash and Python access.",
        backend=backend,
    )

    result = agent.invoke({
        "messages": [{"role": "user", "content": "Write a script that computes fibonacci(10) and run it."}]
    })

    print(result["messages"][-1].content)
finally:
    backend.close()
```

The agent automatically gets `execute`, `read_file`, `write_file`, `edit_file`,
`ls`, `glob`, and `grep` tools — all routed through the sandbox.

## How-to guides

### Seed files before the agent runs

Pass `initial_files` to pre-populate `/workspace`:

```python
backend = WasmshSandbox(
    initial_files={
        "/workspace/data.csv": b"name,score\nalice,95\nbob,87\n",
        "/workspace/config.json": '{"threshold": 90}',
    },
)
```

Both `bytes` and `str` values are accepted (strings are UTF-8 encoded).

### Retrieve files after execution

Use `download_files` to pull artifacts out of the sandbox:

```python
results = backend.download_files(["/workspace/report.txt"])
if results[0].error is None:
    print(results[0].content.decode())
```

### Upload files at runtime

```python
backend.upload_files([("/workspace/input.txt", b"new data")])
```

### Run bash and Python in the same session

Bash and Python share the same `/workspace` filesystem. Write a file in one
language, read it in the other:

```python
# Bash writes a JSON file
backend.execute('echo \'{"status": "ok"}\' > /workspace/status.json')

# Python reads and validates it
result = backend.execute(
    "python3 -c \""
    "import json; "
    "d = json.load(open('/workspace/status.json')); "
    "print(d['status'])\""
)
print(result.output)  # ok
```

### Use a custom working directory

By default, all commands run relative to `/workspace`. Override this:

```python
backend = WasmshSandbox(working_directory="/home/user")
```

### Limit execution budget

`step_budget` and `execute(timeout=...)` are different controls: one counts
VM steps, the other counts wall-clock seconds.

```python
backend = WasmshSandbox(step_budget=100_000)   # 0 (default) = unlimited
```

`step_budget` bounds a command without ending the session. A wall-clock
deadline cannot do the same: Pyodide runs synchronously inside the
WebAssembly module with no cancellation point, so enforcing a deadline means
destroying the session.

```python
result = backend.execute("python3 slow.py", timeout=30)
# On expiry: exit_code == 124 (GNU `timeout(1)`), and the session is gone —
# every later call raises WasmshSessionTerminatedError.
```

Prefer `step_budget` when you want the session to survive; use `timeout`
when a hung command must be cut off regardless. `timeout=None` (the default)
and `timeout=0` both mean "no deadline".

## Use the sandbox as a Python REPL middleware

`WasmshInterpreterMiddleware` exposes the sandbox as a single `py_eval`
agent tool with state that persists across calls and across agent turns
(via a globals-pickle snapshot stored in private agent state). Shape
matches [`langchain-quickjs`'s `CodeInterpreterMiddleware`](https://docs.langchain.com/oss/python/deepagents/interpreters)
but with a real WebAssembly-isolated sandbox underneath.

```python
from deepagents import create_deep_agent
from langchain_wasmsh import WasmshInterpreterMiddleware

agent = create_deep_agent(
    model="claude-sonnet-4-6",
    middleware=[WasmshInterpreterMiddleware()],
)
```

**Interpreter lifetime** is chosen with `mode`:

| `mode` | Globals persist across | Snapshot in state |
|---|---|---|
| `"thread"` (default) | tool calls and agent turns | yes |
| `"turn"` | tool calls within one turn | no |
| `"call"` | nothing — fresh interpreter each call | no |

`snapshot_between_turns=True/False` still works as a deprecated alias for
`mode="thread"/"turn"` and will be removed in the next minor release.
Passing both is an error rather than a silent precedence rule.

### Programmatic tool calling (PTC)

Pass `ptc=[...]` to expose selected agent tools inside the sandbox as
`tools.<snake_name>` awaitables. The model can then fan out, branch,
and chain tool calls within one `py_eval` invocation without extra LLM
turns:

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

User code may then write `await asyncio.gather(*[tools.lookup_user(user_id=i)
for i in [1, 2, 3]])`.

Each nested call receives a child `ToolRuntime` derived from the outer
`py_eval` invocation — same state, context, store, config and stream writer,
with its own `tool_call_id` — so tools declaring `ToolRuntime`,
`InjectedState`, `InjectedStore`, or `InjectedToolCallId` all work. Values
the generated program supplies for an injected parameter are discarded
before injection, so model-authored code cannot forge identity or state.
Sync and async tools both run, from sync and async agent invocations.
Results keep their shape: a list of dicts arrives as a list of dicts.

`max_ptc_calls` (default 256, per evaluation) bounds nested calls so a loop
in generated code cannot issue unbounded tool calls in one turn; pass `None`
to disable it.

**PTC calls bypass the regular `ToolNode` path, so per-tool `interrupt_on`
approval is *not* enforced.** Treat the allowlist as your permission
boundary: gate the outer `py_eval` tool with `interrupt_on`, expose only
tools that are safe to call unattended, or leave PTC off. The interpreter's
own tool is never exposed, even if you name it.

PTC currently requires the in-process backend; `WasmshRemoteSandbox.run_ptc`
raises `NotImplementedError` until the dispatcher SSE channel ships.
Protocol details: [ADR-0031](https://github.com/mayflower/wasmsh/blob/main/docs/adr/adr-0031-ptc-suspend-resume.md).

**Observing PTC tool errors.** When a PTC tool raises, the dispatcher
converts the exception into a `host_call_result` envelope the model
can recover from — but the original stack and call context vanish in
that conversion. The adapter logs every such error through the
standard `logging` module at `WARNING` level, on the
`langchain_wasmsh._repl` logger, with `exc_info=True` and structured
`extra={"wasmsh_ptc_call_id": ..., "wasmsh_ptc_tool": ...}`. Wire a
handler (Sentry, structlog, plain `logging.basicConfig`) onto the
`langchain_wasmsh` namespace to capture them. Skill load failures and
snapshot/restore problems are logged through the same namespace.

### Python skills (`import skills.<name>`)

Pair the middleware with a `SkillsMiddleware` and a shared
`BackendProtocol`. Python sources under each skill directory become
importable inside the REPL:

```python
from deepagents.backends import StateBackend
from deepagents.middleware import SkillsMiddleware

backend = StateBackend()
middleware = [
    SkillsMiddleware(backend=backend, sources=["/skills/user/"]),
    WasmshInterpreterMiddleware(skills_backend=backend),
]
```

The middleware scans user code for `import skills.<name>` / `from
skills.<name> import …` references and stages the matching skill
directory into the sandbox VFS on first use. An `__init__.py` is
synthesised when the skill author didn't ship one.

Every regular file under the skill directory is staged — scripts, SQL,
templates, and binary assets included — with nested structure and bytes
preserved, bounded by 2 MiB per file, 8 MiB per bundle, and 512 files.

Importability is opt-in: `my-skill` becomes `skills.my_skill`, but a name
that is not a valid Python identifier needs an explicit alias. Two optional
keys live under upstream's `metadata` mapping:

```yaml
metadata:
  wasmsh.python_package: sales_report   # import alias
  wasmsh.python_module: lib/report.py   # re-exported from __init__.py
```

Bundles are cached by content fingerprint. Upstream caches `skills_metadata`
per thread, so an updated skill is picked up by a **new** thread while an
existing checkpointed thread keeps the view it started with.

## Durable memory belongs in a store, not the VFS

The wasmsh VFS is an execution workspace. A local sandbox's files live
inside its host subprocess and are gone when that process exits; a remote
session's files last only as long as the dispatcher keeps that session.
Neither is a cross-process store.

Route anything that must outlive a session — profiles, user/agent/org
memory, skills, policies — to upstream `StoreBackend` over a real LangGraph
`BaseStore`, with wasmsh as the executable default:

```python
from dataclasses import dataclass

from deepagents import create_deep_agent
from deepagents.backends import CompositeBackend, StoreBackend
from langgraph.store.memory import InMemoryStore

from langchain_wasmsh import WasmshSandbox


@dataclass
class AgentContext:
    user_id: str
    assistant_id: str
    org_id: str


store = InMemoryStore()          # your production store in deployment
backend = CompositeBackend(
    default=WasmshSandbox(),      # workspace + transient artifacts
    routes={
        "/profiles/": StoreBackend(
            store=store,
            namespace=lambda rt: ("deepagents", "profile", rt.context.user_id),
        ),
        "/memories/user/": StoreBackend(
            store=store,
            namespace=lambda rt: ("deepagents", "memory-user", rt.context.user_id),
        ),
        "/policies/": StoreBackend(
            store=store,
            namespace=lambda rt: ("deepagents", "policy", rt.context.org_id),
        ),
    },
)

agent = create_deep_agent(
    model="claude-sonnet-4-6",
    backend=backend,
    context_schema=AgentContext,
    store=store,
    memory=["/profiles/user.md", "/memories/user/AGENTS.md", "/policies/AGENTS.md"],
)
```

A persistent user profile is ordinary user-scoped memory at
`/profiles/user.md` — no separate profile database or middleware. Keep
secrets and tokens out of it.

### `WasmshFilesystemBackend` — a namespace/path adapter

When several routes should share **one** wasmsh VFS, wrap the sandbox with a
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

Unlike using the sandbox directly, it does **not** expose `execute()`.

**What the namespace does:** every path is joined onto the prefix and
resolved with `posixpath.normpath`; anything that lands outside is rejected
with `WasmshNamespaceEscapeError` (a `PermissionError` subclass) before any
I/O, on results as well as inputs. That stops an agent-controlled
`file_path` like `../../secret.py` on the ordinary file tools.

**What it does not do: tenant isolation.** The check is lexical and does not
survive symlinks — anything with shell access can link out of the namespace
and read through it. For mutually untrusted principals, use separate sandbox
sessions or a non-executable store namespace.

### Filesystem permissions need routed prefixes

`deepagents==0.7.4` refuses `permissions` when the backend can run commands,
unless every rule path sits under a `CompositeBackend` route — a rule
guarding a path the agent can also reach through `execute` would be
advisory, not enforced. So permissions protect the routed store prefixes,
and the wasmsh workspace stays an unguarded execution area. `delete` is
classified as a **write**.

## Remote / Kubernetes backend

For production or scalable deployments, use `WasmshRemoteSandbox` — same
`BaseSandbox` surface, routed through the wasmsh dispatcher + runner
pool.  The dispatcher HTTP contract is documented in
[`docs/reference/dispatcher-api.md`](https://github.com/mayflower/wasmsh/blob/main/docs/reference/dispatcher-api.md);
the Helm chart lives in [`deploy/helm/wasmsh`](https://github.com/mayflower/wasmsh/tree/main/deploy/helm/wasmsh).

```python
from langchain_wasmsh import WasmshRemoteSandbox

backend = WasmshRemoteSandbox("http://wasmsh-dispatcher.wasmsh.svc.cluster.local:8080")
try:
    result = backend.execute("python3 -c 'print(2 + 2)'")
    print(result.output)
finally:
    backend.close()
```

See [`examples/deepagent-python/remote_basic.py`](https://github.com/mayflower/wasmsh/blob/main/examples/deepagent-python/remote_basic.py)
and the [integration guide](https://github.com/mayflower/wasmsh/blob/main/docs/integrations/langchain-wasmsh.md#wasmshremotesandbox--docker--kubernetes-backend)
for a runnable Docker Compose stack and full deployment notes.

## Reference

### `WasmshSandbox(*, runtime, dist_dir, working_directory, step_budget, initial_files, allowed_hosts)`

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `runtime` | `str \| None` | auto-detect | Runtime path — prefers Deno, falls back to Node.js |
| `dist_dir` | `str \| Path \| None` | auto-resolved | Path to Pyodide distribution assets |
| `working_directory` | `str` | `"/workspace"` | Working directory for `execute()` |
| `step_budget` | `int` | `0` (unlimited) | VM step budget per command |
| `initial_files` | `dict[str, str \| bytes] \| None` | `None` | Files to seed at creation |
| `allowed_hosts` | `list[str] \| None` | `None` (deny all) | Hostnames allowed for network access |

Raises `FileNotFoundError` if neither Deno nor Node.js is found on `PATH`.
When using Node.js, `allowed_hosts` is still enforced at the wasmsh level but
lacks Deno's OS-level permission isolation.

### Properties

| Property | Type | Description |
|----------|------|-------------|
| `id` | `str` | Unique sandbox identifier (e.g., `wasmsh-python-<uuid>`) |

### Methods

| Method | Returns | Description |
|--------|---------|-------------|
| `execute(command, *, timeout=None)` | `ExecuteResponse` | Run a shell command (prepends `cd /workspace &&`). `timeout` is a real wall-clock deadline in seconds; on expiry the host is terminated, `exit_code` is `124`, and the session becomes unusable. `None`/`0` mean no deadline. |
| `upload_files(files)` | `list[FileUploadResponse]` | Write files into the sandbox |
| `download_files(paths)` | `list[FileDownloadResponse]` | Read files from the sandbox |
| `close()` | `None` | Shut down the host subprocess |
| `stop()` | `None` | Alias for `close()` |

### Inherited from `BaseSandbox`

Everything else runs upstream Deep Agents code unchanged — `ls`, `read`,
`write`, `delete`, `glob`, and their async siblings — so their result shapes
and error strings are upstream's by construction, including the full 0.7.4
`ReadResult` pagination metadata.

Exactly two operations override the transport, and both re-route upstream's
own logic rather than reimplementing it:

- **`edit`** — upstream's default route feeds its payload to `python3 -c`
  through a heredoc, and wasmsh's in-process `python3` never receives the
  shell's stdin. The override forces upstream's own temp-file route, so the
  replacement algorithm, CRLF handling, and error strings are unchanged.
  Editing a known binary/media extension is refused rather than corrupting
  bytes the model only ever saw base64-encoded.
- **`grep`** — wasmsh's `grep` silently ignores `-Z`, so upstream's
  NUL-delimited record parser could not read its output. An in-sandbox
  script emits exactly the records upstream expects; parsing, `max_count`,
  and `truncated` stay upstream's.

### `ExecuteResponse`

| Field | Type | Description |
|-------|------|-------------|
| `output` | `str` | Combined stdout + stderr |
| `exit_code` | `int \| None` | Exit code, or `None` if unavailable |
| `truncated` | `bool` | Always `False` for wasmsh |

`enable_capture_offload` stays `False`: the inherited fallback runs the
command exactly once and reports `offloaded=False`. It will be enabled only
once the offload wrapper passes command-level conformance on the wasmsh
shell.

### Error mapping

Diagnostic events from the wasmsh runtime are mapped to `FileOperationError`:

| Diagnostic contains | Mapped to |
|---------------------|-----------|
| `"not found"` | `"file_not_found"` |
| `"directory"` | `"is_directory"` |
| `"permission"` | `"permission_denied"` |
| *(other)* | `"invalid_path"` |

## Explanation

### What runs inside the sandbox

The wasmsh runtime provides 88 shell utilities (including `jq`, `awk`, `rg`,
`fd`, `diff`, `tar`, `gzip`) plus `python`/`python3` via an embedded CPython
interpreter. Both share the same Emscripten POSIX filesystem.

This is **not** a Linux container. There is no kernel, no process isolation, no
`apt`, `pip install`, or `docker`. If you need a full OS environment, use a
container-based provider like `langchain-modal` or `langchain-daytona`.

### How it works

The provider launches a Deno or Node.js subprocess that boots the
Pyodide/Emscripten WebAssembly module. Communication uses JSON-RPC over
stdin/stdout.

```
Python process          Deno / Node.js subprocess
┌─────────────┐        ┌──────────────────────┐
│ WasmshSandbox│──JSON──│ node-host.mjs        │
│ (BaseSandbox)│  RPC   │   ├─ Pyodide/WASM    │
│              │◄─────►│   ├─ wasmsh runtime   │
│              │ stdin/ │   └─ CPython 3.13     │
│              │ stdout │                      │
└─────────────┘        └──────────────────────┘
```

**Runtime selection:** Deno is preferred when available. It provides
defense-in-depth via OS-level permission flags (`--allow-read=<assets>`,
`--allow-net=<hosts>`), restricting the subprocess to only the files and
network hosts it needs. If Deno is not installed, Node.js is used as a
fallback — `allowed_hosts` is still enforced at the wasmsh application
level but without OS-level isolation. You can force a runtime with
`WasmshSandbox(runtime="deno")` or `WasmshSandbox(runtime="node")`.

### How the agent uses the sandbox

When you pass a `WasmshSandbox` as the `backend` to `create_deep_agent`, the
agent gains filesystem tools and a shell `execute` tool:

- **Filesystem tools** (`read_file`, `write_file`, `edit_file`, `ls`, `glob`,
  `grep`) are implemented by `BaseSandbox` using POSIX shell commands via
  `execute()`.
- **`execute()`** prepends `cd /workspace &&` to every command.
- **`initial_files`** are written during sandbox creation before any agent
  code runs.

### Lifecycle

The host process starts when `WasmshSandbox()` is constructed and stops
when `close()` (or its alias `stop()`) is called. Always use try/finally to
avoid orphaned subprocesses.
