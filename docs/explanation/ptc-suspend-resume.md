# Spec: PTC suspend/resume for the wasmsh sandbox

**Status:** draft — needs review before implementation
**Scope:** wasmsh-pyodide, wasmsh-protocol, wasmsh-json-bridge, tools/runner-node, wasmsh-dispatcher, langchain-wasmsh (Python + TS)
**Owner:** TBD
**Tracking issue:** TBD

## 1. Problem

`WasmshInterpreterMiddleware` (Python) and the equivalent in `@mayflowergmbh/langchain-wasmsh` (TS) want to expose host-registered LangChain tools to user code running inside a wasmsh sandbox session — the **Programmatic Tool Calling** (PTC) feature popularised by `langchain-quickjs`:

```python
# Model emits this through the py_eval tool:
results = await asyncio.gather(
    tools.search(query="retrieval"),
    tools.search(query="memory"),
)
print(results)
```

Today every sandbox `run` is a single-shot JSON-RPC round trip: host sends `{method: "run", code}` → sandbox writes the script to its VFS, executes it to completion, returns one `{ok, result}` envelope. There is no mechanism for user code to suspend mid-execution and call back into the LangChain tool dispatcher. PTC is therefore raised as `PTCNotSupportedError` at middleware construction.

This spec describes the protocol, runtime, and adapter changes required to lift that restriction.

## 2. Goals

- **G1.** From inside a sandbox eval, user Python can `await tools.<name>(**kwargs)` and receive the tool's native value back, with arbitrary interleaving (`asyncio.gather`, branching, loops).
- **G2.** Multiple concurrent host calls within a single eval are supported (correlated by id).
- **G3.** Each tool call surfaces in LangSmith / OpenTelemetry the same way an out-of-band tool call would (it goes through the `BaseTool` dispatch path on the host).
- **G4.** Symmetric surface across local (`WasmshSandbox`) and remote (`WasmshRemoteSandbox`) backends, and across Python and TS adapters.
- **G5.** Per-eval budgets (`max_host_calls`, wall-clock, payload size) match the safety knobs of the QuickJS interpreter.
- **G6.** The transport extension is reusable for JS interpreters layered on the same sandbox in the future.

## 3. Non-goals

- **N1.** Cross-eval suspension. A host call cannot survive across separate `run` invocations; the suspend window is the lifetime of one eval.
- **N2.** Bidirectional streaming arbitrary data. Tool args / results are JSON values, capped by a payload size budget.
- **N3.** Synchronous (non-`await`) tool calls from user code. PTC requires `await`; sync attempts raise `RuntimeError`.
- **N4.** Pre-emptive cancellation of tool calls in the host's LLM-tool path. Cancel terminates the eval; in-flight tool calls run to completion on the host (their results are discarded).

## 4. Current state

Two paths run the Pyodide interpreter:

### 4.1 Local subprocess path

`WasmshSandbox` (`packages/python/langchain-wasmsh/sandbox.py`) spawns Deno or Node executing `tools/runner-node/src/host.mjs` (or the Pyodide-runtime equivalent). Communication is **newline-delimited JSON** over stdin/stdout. Each request has an `id`; the host writes back one response per request. The JS host shape:

```
host ──{id, method, params}──▶ runner-js  → wasmsh-pyodide → Pyodide
                            ◀──{id, ok, result}──
```

### 4.2 Scalable dispatcher path

`WasmshRemoteSandbox` POSTs JSON to `wasmsh-dispatcher` (Axum). The dispatcher routes to a `wasmsh-runner` pod that runs the same runner-js host as 4.1, exposed over HTTP at `/sessions/<id>/run`. Today this is a single request → single response; the dispatcher proxies the full body in both directions.

### 4.3 Bridge types

`wasmsh-json-bridge` (Rust) defines the wire serialisation of `HostCommand` / `WorkerEvent`. The Pyodide path uses the same `HostCommand` / `WorkerEvent` shape over a JSON channel. The current `WorkerEvent::Result` is the single terminal event per command; intermediate events like `Diagnostic` already exist but never carry round-trip semantics.

## 5. Wire protocol

The transport extension is one new pair of message types layered onto the existing JSON channel. Within one `run` request the response stream becomes a sequence of messages terminated by `result`:

### 5.1 Sandbox → host (interleaved before `result`)

```jsonc
{
  "type": "host_call",
  "id": "abc",                 // string, opaque, sandbox-generated, unique within this eval
  "tool": "search",            // resolved tool name (post snake_case mapping)
  "args": { "query": "foo" }   // JSON object; serialisable LangChain ToolInput
}
```

Multiple `host_call` messages may be in flight concurrently. Order is *not* required to be call-order; the sandbox emits each one as the corresponding asyncio task reaches its `await`.

### 5.2 Host → sandbox (during a `run`)

```jsonc
// Success
{
  "type": "host_call_result",
  "id": "abc",
  "ok": true,
  "value": "..."               // any JSON value the tool returned
}

// Failure
{
  "type": "host_call_result",
  "id": "abc",
  "ok": false,
  "error": "ToolException",    // exception class name, kebab-tolerant
  "message": "rate limited",
  "stack": "..."               // optional; truncated by max_error_chars
}
```

The host must respond to every `host_call` exactly once. Late responses (after `result`) are protocol violations; the sandbox MAY log and drop them.

### 5.3 Cancellation

```jsonc
// Either direction, at any time
{ "type": "cancel", "reason": "timeout" }
```

Receiver behaviour:
- **Sandbox-side cancel:** Pyodide's eval is aborted via Pyodide's interrupt buffer (already used for `step_budget`). All pending `host_call` futures are settled with `asyncio.CancelledError`.
- **Host-side cancel:** in-flight LangChain tool calls are allowed to complete (cf. N4), their results are not forwarded. The host then writes the terminal `result` (with `ok=false`, `error="Cancelled"`).

### 5.4 Terminal event

Existing shape; unchanged:

```jsonc
{ "type": "result", "envelope": { ... } }
```

### 5.5 Versioning

`HostCommand::Run` gains an optional `capabilities` field:

```jsonc
{ "type": "run", "code": "...", "capabilities": { "host_call": "v1" } }
```

A sandbox without the capability MUST ignore the field and behave exactly as today (no `host_call` will ever be emitted because no `tools.*` shim is installed). A sandbox with the capability but the field omitted MUST also behave as today. Hosts MUST NOT install the PTC shim unless they declared `capabilities.host_call: "v1"` and received an `Ack` (§ 5.6).

### 5.6 Capability negotiation

Pre-eval handshake (one shot, lazy):

```jsonc
{ "type": "ack", "capabilities": { "host_call": "v1" } }
```

The runner-js host emits `ack` on session creation; the host adapter caches it per session. If `host_call` is missing from the ack, PTC stays raised as `PTCNotSupportedError` even if the middleware was constructed with `ptc=`.

## 6. Sandbox-side implementation

### 6.1 Launcher rewrite (Python)

`packages/python/langchain-wasmsh/langchain_wasmsh/_launcher.py` LAUNCHER_SCRIPT is rewritten to support top-level `await`. Two strategies — choose **A** for v1.

**Strategy A — Pyodide `runPythonAsync` on the JS side.** Move launcher orchestration into the runner-js host:

```js
// runner-node/src/host.mjs (sketch)
const tools = pyodide.runPython(`
  class _Tools:
      def __getattr__(self, name):
          return _make_proxy(name)
  tools = _Tools()
  tools
`);
pyodide.globals.set("__wasmsh_host_call", jsHostCallFn);
const result = await pyodide.runPythonAsync(userCode);
```

`__wasmsh_host_call(tool, args)` is a JS async function that emits the `host_call` message and returns a Promise resolved by the next matching `host_call_result`. Pyodide already bridges JS Promises to Python awaitables, so `await __wasmsh_host_call(...)` inside Python suspends naturally on the JS event loop.

This avoids running an asyncio loop inside Pyodide and reuses Pyodide's native top-level-await compilation. **Cost:** the launcher's stdout/stderr capture + globals pickling now spans Python and JS — JS owns the message multiplexing, Python owns the namespace persistence.

**Strategy B — asyncio in Python with a Pyodide foreign-event-loop.** Run the launcher under `asyncio.run(main())`, install a custom event loop that yields to the JS-side message pump. More moving parts; defer to a follow-up if Strategy A hits a wall.

### 6.2 `tools` namespace

Installed in `__main__` globals before the eval starts:

```python
class _PtcTool:
    def __init__(self, name): self._name = name
    async def __call__(self, **kwargs):
        return await _wasmsh_invoke_tool(self._name, kwargs)

class _PtcTools:
    def __init__(self, names):
        for n in names: setattr(self, n, _PtcTool(n))

tools = _PtcTools(["search", "task", ...])  # snake_case names from filter_tools_for_ptc
```

The `__call__` boundary is the only point where the JS bridge is invoked; user code never sees the bridge directly. Positional args are rejected with `TypeError` to force the keyword-arg shape `BaseTool.invoke` expects.

### 6.3 Budget enforcement

Per eval, before each `_wasmsh_invoke_tool` call:

- Increment `host_calls_used`; raise `PTCCallBudgetExceeded` (subclass of `RuntimeError`) if `> max_host_calls`.
- Check elapsed wall-clock against `host_call_timeout`; raise on overflow.
- Reject `args` whose JSON-encoded size exceeds `max_call_args_bytes`.

The budget counter lives in the launcher's eval-scoped state; it does **not** persist across evals.

### 6.4 Failure handling

- A `host_call_result` with `ok=false` is converted into a Python exception. We mint a synthetic class hierarchy rooted at `wasmsh.PTCToolError`, with `error_name` set from the wire `error` field, so `except PTCToolError` catches all and `except SomeSpecificError` works when the host names a known class.
- A host that fails to respond before the eval's wall-clock budget triggers the same path as § 5.3 sandbox-side cancel.

## 7. Host-side implementation

### 7.1 runner-node (`tools/runner-node/src/`)

Adds an outbound writer that interleaves with the existing single-response writer:

```js
function writeMessage(msg) { process.stdout.write(JSON.stringify(msg) + "\n"); }
async function runWithHostCalls(code, dispatcher) {
  pyodide.globals.set("__wasmsh_host_call", async (tool, argsObj) => {
    const id = crypto.randomUUID();
    writeMessage({ type: "host_call", id, tool, args: toPlainObject(argsObj) });
    return await dispatcher.awaitResponse(id);  // resolved when host_call_result lands
  });
  // Existing run flow:
  return await pyodide.runPythonAsync(code);
}
```

A small `dispatcher` object reads stdin in a loop, demultiplexing `host_call_result` into the per-id Promise resolvers it tracks.

### 7.2 langchain-wasmsh (Python adapter)

`WasmshSandbox` and `WasmshRemoteSandbox` gain a new internal method:

```python
def _run_with_ptc(
    self,
    code: str,
    *,
    tools: Mapping[str, BaseTool],
    budgets: PtcBudgets,
) -> ExecuteResponse: ...
```

The `WasmshSandbox` (local) implementation runs the request in a worker thread so the main thread can read incoming `host_call` messages on the stdout pipe, dispatch them to `BaseTool.invoke`, and write back the `host_call_result`. The interleaving loop sits in `_repl.py` next to `_ThreadREPL`.

For LangChain instrumentation: each `host_call` dispatch creates a child run via `RunnableConfig`'s callbacks, so LangSmith traces show `py_eval → tools.search → BaseTool.invoke` as nested spans.

### 7.3 WasmshRemoteSandbox + dispatcher

HTTP plain `POST /sessions/<id>/run` is single round-trip; we need bidirectional streaming. **Choose A:**

**Option A — Server-Sent Events for the response, separate POST for host_call_result.**

```
POST /sessions/<id>/run         → text/event-stream
                                  event: host_call    data: {...}
                                  event: host_call    data: {...}
                                  event: result       data: {...}
POST /sessions/<id>/host_result body: {id, ok, value | error, ...}
```

Pros: HTTP-native, works through standard proxies, simple to implement on Axum (`Sse`).
Cons: two-channel; client must correlate id ↔ open SSE stream.

**Option B — WebSocket on `/sessions/<id>/ws`.**

Pros: one channel, lower overhead, bidirectional.
Cons: more infra friction (some ingress paths don't pass WS cleanly), client lib complexity.

**Recommendation:** Option A for v1 because the rest of the dispatcher API is HTTP/JSON; revisit WS once HTTP runs into latency limits with high host-call fan-out.

The dispatcher proxies the SSE stream from the runner pod end-to-end without parsing — that keeps host-call payloads opaque to the dispatcher and avoids a new pod-level RPC seam. `host_result` POST goes back through the same pod via session-affinity routing (already implemented for `run`).

### 7.4 TS adapter

The TypeScript surface mirrors the Python adapter's shape. Concretely:

- `@mayflowergmbh/wasmsh-pyodide` (this repo) exposes
  `NodeSession.runPtc({ code, tools, onHostCall })` since v0.7.0. Browser
  sessions throw on `runPtc`; browser PTC is out of scope for now.
- `@mayflowergmbh/langchain-wasmsh` (in `deepagentsjs/libs/providers/wasmsh`,
  the canonical TS adapter location per LangChain's partner-package policy)
  ships `createWasmshInterpreterMiddleware`, the same `ptc=` knob shape as
  the Python adapter, and `WasmshLogger` for structured observability of
  PTC tool errors and skill-load failures.

The runner-side JS in § 7.1 is reused verbatim by both packages.

## 8. Failure & cancellation semantics

| Scenario | Sandbox-side observable | Host-side observable |
|-|-|-|
| Tool returns OK | `await tools.x(...)` resolves to value | LangSmith logs child run with ok status |
| Tool raises exception | `PTCToolError` subclass raised at `await` | LangSmith logs child run with error status |
| Tool exceeds host wall-clock | Same as above with `error="ToolTimeoutError"` | Host cancels the LangChain tool invocation |
| Host responds for unknown id | (impossible — host generates ids it received) | — |
| Sandbox receives `host_call_result` after `result` | Drop + warn | — |
| Eval hits `max_host_calls` | `PTCCallBudgetExceeded` raised | No new `host_call` after the failed N+1th |
| User code calls a non-PTC tool name | `AttributeError` (the name is not on `tools`) | No `host_call` emitted |
| Sandbox dies mid-eval | — | Host sees stream close before `result`; surfaces `RuntimeError("sandbox terminated during PTC")` |
| Host SSE stream broken | All pending tool calls fail with `IOError`; eval cancelled | Caller-visible error |

## 9. Security model

PTC widens the agent's attack surface compared to the current sandbox: the model can now drive arbitrary LangChain tools without an intervening LLM turn. Mitigations:

- **Allowlist-only**: PTC tools are configured via `ptc=` and filtered by `filter_tools_for_ptc`. The default is "no tools exposed". A tool not on the list is not on the `tools` namespace.
- **`interrupt_on` not enforced per call** (same caveat as `langchain-quickjs`). PTC tools should not be configured for HITL approval; document this prominently in the middleware docstring.
- **Args & results pass through the LangChain `BaseTool` validator** (Pydantic schema), so malformed args are rejected the same as if the tool had been called via the LLM path.
- **Budgets are mandatory**, not best-effort: `max_host_calls` defaults to 256 (matches QuickJS); `host_call_timeout` defaults to 30s per call; `max_call_args_bytes` defaults to 64 KiB.
- **No new host filesystem reach** — PTC piggybacks on the existing JSON-RPC channel and adds no FS hooks.
- **Sandbox identity** — every `host_call` is implicitly bound to the session id the host opened; the dispatcher tags the LangSmith run with that id so cross-tenant misuse is auditable.

## 10. Compatibility & rollout

- **Wire compatibility:** new message types are additive. Old sandbox + new host: host receives no `ack` for `host_call`, falls back to today's behaviour. New sandbox + old host: sandbox emits `host_call`, old host ignores unknown JSON lines (it already tolerates this for `Diagnostic` events).
- **API compatibility:** `WasmshInterpreterMiddleware(ptc=...)` stops raising `PTCNotSupportedError` once the underlying sandbox advertises the capability. Existing callers that intentionally relied on the raise should pin `ptc=None`.
- **Versioning:** `host_call: "v1"` is the protocol tag. Future breaking changes (different `args` shape, etc.) bump to `v2`. Sandboxes may advertise multiple versions; hosts pick the highest mutually supported.

## 11. Implementation phases

### Phase 1 — local protocol & launcher rewrite

Touch points:
- `wasmsh-protocol`: add `HostCallRequest` / `HostCallResult` / `Cancel` message variants.
- `wasmsh-json-bridge`: wire them through JSON serialisation; add capability ack.
- `tools/runner-node`: implement the JS-side dispatcher loop (§ 7.1).
- `packages/python/langchain-wasmsh/_launcher.py`: rewrite to top-level await with `tools` namespace installed.
- `packages/python/langchain-wasmsh/_repl.py`: streaming reader for stdout; remove `PTCNotSupportedError` raise once capability is advertised.
- `packages/python/langchain-wasmsh/interpreter.py`: drop `raise_if_ptc_configured`, install PTC tools per turn (mirrors QuickJS's `_prepare_for_call`).

Exit criteria:
- E2E test in `e2e/pyodide-node/`: `WasmshInterpreterMiddleware(ptc=["search"])` runs a model that calls `await tools.search(...)` and gets a concrete value back.
- Unit tests in `tests/unit_tests/test_interpreter.py` cover: single host call, parallel fan-out, error propagation, budget exhaustion, cancellation.
- TOML suite in `tests/suite/`: a synthetic case asserting that user code can call a stubbed tool through the host_call channel.

### Phase 2 — remote (dispatcher) path

Touch points:
- `wasmsh-dispatcher`: implement `Sse` body for `/sessions/<id>/run`; add `POST /sessions/<id>/host_result` endpoint.
- `wasmsh-dispatcher`: keep session-affinity routing for `host_result` (already implemented for `run`).
- `tools/runner-node`: expose the host-call channel over the HTTP surface the dispatcher fronts.
- `WasmshRemoteSandbox`: switch `run` to read SSE; add `_post_host_result`.

Exit criteria:
- `e2e/dispatcher-compose`: same parallel-tool-call test as Phase 1, executed through the dispatcher.
- `e2e/kind`: full Helm install passes the same test (NetworkPolicy + ingress need to allow `text/event-stream`).

### Phase 3 — TS adapter parity

**Status:** landed for the in-process backend.

Touch points:
- `packages/npm/wasmsh-pyodide`: `NodeSession.runPtc({ code, tools, onHostCall })`
  exposed in v0.7.0 (the JS-side dispatcher loop in § 7.1 is shared with
  the Python adapter through the same JSON-RPC channel).
- `deepagentsjs/libs/providers/wasmsh` (LangChain partner package, not in
  this repo): `createWasmshInterpreterMiddleware` with the same `ptc=`
  shape as the Python adapter, plus the structured `WasmshLogger` hook
  surface (`ptcToolError`, `skillLoadError`) for hosts that need to see
  the errors the middleware swallows into envelopes.

Exit criteria:
- `e2e/pyodide-node/tests/ptc-round-trip.test.mjs` covers Node-host PTC
  end-to-end (host_call fan-out, error envelopes, persistence across
  calls). Done.
- TS adapter tests in `deepagentsjs/libs/providers/wasmsh/src/` cover the
  middleware → sandbox → envelope round-trip with both a scripted
  FakeChatModel (`agent.test.ts`) and real Pyodide
  (`sandbox-runPtc.int.test.ts`). Done.

Browser-side PTC (Phase 3b) remains out of scope; the browser session
throws if `runPtc` is called.

## 12. Open questions

1. **`tools.__getattr__` vs eager attribute install.** Eager install (§ 6.2) gives early `AttributeError` for typos; lazy `__getattr__` gives clearer signal (`tools.foo()` raises *"tool 'foo' not on PTC allowlist"*) but lets bad names through type checkers. Recommend eager.
2. **Tool result coercion.** LangChain tool returns can be any Python value; how do we coerce non-JSON values (e.g., `Document` objects) on the host side before serialising? Reuse `langchain-quickjs._repl.coerce_tool_output_for_ptc`? It currently calls `to_dict` / `model_dump` with a chain of fallbacks — vendor that as `_coerce.py` to keep behaviour identical.
3. **Single-flight per session.** The current `WasmshSandbox` serialises evals via `_ThreadREPL._lock`. Does PTC need to relax that so the host can issue *another* `run` while a PTC eval is in flight? **Recommendation: no.** Keep the eval serialised; PTC fan-out happens inside one eval via `asyncio.gather`.
4. **`step_budget` interaction.** The existing `step_budget` is a count of VM steps. A `host_call` await might consume zero steps but unbounded wall-clock. Add a dedicated `host_call_timeout` knob (§ 8) and document the difference.
5. **LangSmith parent-run linkage.** The host-side dispatcher must propagate the current `RunnableConfig.run_id` to the synthetic child runs; otherwise PTC tool calls show up unparented in LangSmith. Confirm the API surface for `BaseTool.invoke(config=...)` carries this correctly.
6. **Snapshot semantics.** The launcher pickles globals at end-of-eval (§ 4 of `_launcher.py`). PTC adds no new pickle complications — the `tools` namespace is rebuilt every eval and not persisted. Document this explicitly.
7. **Backpressure.** No host-side backpressure on `host_call` emission. If the model writes `await asyncio.gather(*[tools.x() for _ in range(10_000)])`, the sandbox queues 10 000 in-flight calls. `max_host_calls` cap prevents this; confirm the budget is checked *before* the sandbox emits the wire message, not after.

## 13. References

- `langchain-quickjs` (`/Volumes/CrucialMusic/src/deepagents/libs/partners/quickjs/langchain_quickjs/`): reference implementation for the PTC concept, including budget enforcement, prompt rendering, and host-function bridging.
- `quickjs-rs` (`/Volumes/CrucialMusic/src/quickjs-rs/src/`): the underlying async host-function dispatcher pattern (`set_async_host_dispatcher`, `resolve_pending`, `reject_pending`).
- `wasmsh-protocol` (`crates/wasmsh-protocol`): existing `HostCommand` / `WorkerEvent` types this extension layers on.
- ADR-0020: E2E-first testing policy — Phase 1 & 2 exits gate on E2E presence.
- ADR-0021: Network capability model — analogous shape (allowlist + budget + audit trail) we mirror for PTC.
