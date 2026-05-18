# ADR-0031: PTC Suspend/Resume Over the wasmsh-pyodide JSON-RPC Channel

## Status

Accepted (Phase 1 — local subprocess path landed; Phase 2 dispatcher SSE and
Phase 3 TS adapter parity tracked separately).

## Context

`WasmshInterpreterMiddleware` (Python) and the equivalent middleware
forthcoming on the npm side want to expose host-registered LangChain tools
to user code running inside a wasmsh sandbox session — the
**Programmatic Tool Calling** (PTC) feature popularised by
`langchain-quickjs`:

```python
# Model emits this through the py_eval tool:
results = await asyncio.gather(
    tools.search(query="retrieval"),
    tools.search(query="memory"),
)
```

The pre-existing JSON-RPC channel between the host adapter and the
wasmsh-pyodide Node host was single-shot: host sends `{method: "run", code}`
→ sandbox executes to completion → host gets one `{ok, result}` envelope.
There was no mechanism for user code to suspend mid-execution and call
back into the LangChain tool dispatcher.

Three transport shapes were considered:

1. **Bidirectional mid-run messages over the existing channel.** Add new
   message types that interleave with the response stream during one
   `run` invocation. Reuses the JSON-line transport.
2. **Two-phase eval with continuation tokens.** User code uses an explicit
   yield/continuation pattern; each yield emits a tool-call request, eval
   returns, the next call resumes. Awkward shape for the model.
3. **Long-polling via VFS sentinel files.** The host doesn't watch the
   sandbox VFS — wrong shape for sync RPC.

Option 1 was chosen as the least disruptive.

## Decision

Two new message types layered onto the existing newline-delimited JSON
channel; an `ack` envelope at boot for capability negotiation; a single
new JSON-RPC method `runPtc`.

### Wire protocol

**Boot ack** (sandbox → host, one-shot):

```jsonc
{"type": "ack", "capabilities": {"host_call": "v1"}}
```

A host adapter MUST NOT issue `runPtc` unless `host_call: "v1"` (or a
forward-compatible version) is in the advertised capabilities.

**Mid-`runPtc` interleaving:**

```jsonc
// sandbox → host
{"type": "host_call", "id": "hc_<seq>", "tool": "search", "args": {"q": "foo"}}

// host → sandbox
{"type": "host_call_result", "id": "hc_<seq>", "ok": true,  "value": "hit:foo"}
{"type": "host_call_result", "id": "hc_<seq>", "ok": false, "error": "...", "message": "..."}
```

Multiple `host_call` messages may be in flight concurrently within one
`runPtc`; correlation is by `id`. The host MUST respond to every emitted
`host_call` exactly once before the terminal `result`.

**Terminal envelope** (unchanged from the existing `run` shape):

```jsonc
{"id": <request_id>, "ok": true, "result": {"envelope": {...}}}
```

### Sandbox-side suspend mechanism

Uses Pyodide's native top-level `await`: the JS host installs a JS async
function `__wasmsh_host_call` and a Python `tools` namespace on
`pyodide.globals`. User code's `await tools.foo(...)` resolves to
`await __wasmsh_host_call("foo", {...})`, whose returned Promise is bridged
into the Python coroutine by Pyodide. The host writes the
`host_call_result` from the readline loop; Pyodide resolves the Promise;
the Python coroutine resumes with the result.

User code is wrapped in an `async def _wasmsh_ptc_main()` before
execution so `await` is syntactically allowed.

### Host-side dispatch concurrency

The Node host's `readline` loop is single-threaded but cooperatively
schedules: the loop body is non-blocking (each request is dispatched via
`Promise.resolve().then(...)` rather than `await`ed inline) so the loop
keeps reading lines while a `runPtc` is in flight. Without this, the
loop would deadlock against itself — `runPtc` cannot complete until the
host writes a `host_call_result`, which the loop cannot do while it is
still awaiting the request body.

### Capability gating

The Python adapter caches the boot `ack`'s capabilities. A call to
`WasmshSandbox.run_ptc(...)` raises a clear `RuntimeError` if
`host_call` is not advertised — for instance against a pre-Phase-1 host
build — so PTC degradation is loud, not silent.

### Safety bounds

- `_MAX_STALE_RESPONSES = 100` — the Python adapter bails out if the host
  emits 100 consecutive responses with mismatched ids. Defends against
  a stuck host without changing legitimate behaviour.
- Same-thread reentry guard — a PTC tool that synchronously calls back
  into the same sandbox raises `RuntimeError("reentrant wasmsh sandbox
  call")` rather than deadlocking on the request lock.

## Consequences

- The JSON-RPC channel is now bidirectional during one `runPtc`. The boot
  `ack` is harmless for clients that don't recognise it (they skip
  unknown JSON lines).
- `WasmshInterpreterMiddleware(ptc=[...])` works end-to-end through
  `create_deep_agent` (Phase 1).
- `WasmshRemoteSandbox.run_ptc` raises `NotImplementedError` until the
  dispatcher path lands (Phase 2: SSE response stream + companion
  `POST /sessions/<id>/host_result` endpoint).
- The npm `NodeSession` (returned by `createNodeSession`) exposes
  `runPtc({ code, tools, onHostCall })` since `@mayflowergmbh/wasmsh-pyodide`
  v0.7.0 — npm consumers no longer need to speak the wire protocol by
  hand. The browser session still throws on `runPtc`; browser PTC is
  out of scope for Phase 3.
- The npm `@mayflowergmbh/langchain-wasmsh` adapter consumes `runPtc`
  through `WasmshInterpreterMiddleware` (TypeScript) and ships a
  structured `WasmshLogger` hook (`ptcToolError`, `skillLoadError`) so
  hosts can observe the exceptions the middleware swallows into
  envelopes. The Python adapter offers the symmetric observability via
  the stdlib `logging.getLogger("langchain_wasmsh")` namespace.
- The JS-side `node-host.mjs` and the launcher-side Python helper
  (`lib/ptc-helper.mjs`) duplicate the launcher's globals-pickle / value
  coercion logic with the file-based `_launcher.py` shipped by
  `langchain-wasmsh`. The skip-lists are kept in sync via a cross-pointing
  comment. Full source-sharing across the JS/Py boundary is deferred.

## References

- Full design notes: [`docs/explanation/ptc-suspend-resume.md`](../explanation/ptc-suspend-resume.md)
- Reference implementation: `langchain-quickjs` host-function bridge in
  the deepagents partners repo
- Phase 1 commit: `feat(langchain-wasmsh,pyodide): interpreter middleware
  + PTC suspend/resume`
- Related: ADR-0017 (shared runtime extraction), ADR-0018 (Pyodide
  same-module architecture), ADR-0021 (network capability model — analogous
  allowlist + budget + audit shape for PTC tool gating).
