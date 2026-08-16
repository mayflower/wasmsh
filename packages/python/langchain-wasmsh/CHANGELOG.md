# Changelog

## 0.8.0

Explicit, tested compatibility with LangChain Deep Agents **0.7.4**. The
version deliberately does not mirror upstream's — this is the adapter's own
line, and matching numbers would imply a lockstep that does not exist.

### Breaking

- **Dependency window is now explicit.** `deepagents>=0.7.4,<0.8.0`, with
  matching bounds on `langchain`, `langchain-core`, `langgraph`, `pydantic`,
  and `httpx`. The previous `deepagents>=0.5.0a2` floor let a clean install
  resolve arbitrarily far ahead of what the code and tests encoded.
- **`wasmsh-pyodide-runtime>=0.7.0`** — the first host that advertises the
  `host_call` capability programmatic tool calling needs.
- **The redundant `deepagents` optional extra is gone.** Deep Agents is a
  required dependency; the extra only offered a second way to state the same
  thing.
- **`execute(timeout=N)` now does something.** It used to log that it was
  advisory. It is now a real wall-clock deadline, and enforcing it destroys
  the session (see below). Callers that passed a timeout expecting it to be
  ignored will see behaviour change.
- **`WasmshInterpreterMiddleware(snapshot_between_turns=...)` is
  deprecated** in favour of `mode="thread" | "turn" | "call"`. The old
  argument still works for one more minor release; passing both raises.

### Backend protocol

Every operation the transport allows now runs upstream Deep Agents code
unchanged. That was verified by removing each override and running the
`langchain-tests` conformance suite against real Node and Deno hosts, not by
inspection.

- **`read` / `aread`**: the custom pagination is deleted. `ReadResult` now
  carries the full 0.7.4 metadata — `total_lines`, `start_line`, `end_line`,
  `next_offset`, `no_lines_requested` — along with exact bounds clamping,
  binary classification by extension, the 500 KiB preview cap, and the
  empty-file reminder.
- **`edit` / `aedit`**: still overridden, but now only to pick upstream's own
  temp-file route. Upstream's default route feeds its payload through a
  heredoc and wasmsh's in-process `python3` never receives the shell's
  stdin. The replacement algorithm, CRLF handling, occurrence counting, and
  error strings are upstream's. Editing a known binary/media extension is
  refused instead of corrupting bytes the model only ever saw base64-encoded.
- **`grep` / `agrep`**: wasmsh's `grep` silently ignores `-Z`, so upstream's
  NUL-delimited parser reported the *matches* as an error. An in-sandbox
  script now emits exactly the records upstream expects; parsing, `max_count`
  and `truncated` remain upstream's.
- **`delete` / `adelete`**: exposed through `WasmshFilesystemBackend`, which
  previously dropped it entirely.
- **`WasmshFilesystemBackend`** gained async variants of every operation,
  keyword-only `max_count` on grep, `glob(path: str | None)`, `truncated`
  preservation, and namespace-relative paths in successful write/edit/delete
  results.

### Timeouts

`execute(timeout=N)` is enforced end to end. Pyodide runs synchronously
inside the WebAssembly module with no cancellation point, so the deadline is
enforced the only honest way available: the local sandbox kills its host, the
remote runner terminates its worker, both report exit code `124` (GNU
`timeout(1)`), and the session refuses further work. Use `step_budget` when
you want a bound that leaves the session alive.

The dispatcher accepts and forwards `timeout_ms` on
`POST /sessions/{id}/run`, clamped to one hour, and widens its own upstream
socket past the command deadline so the runner's authoritative answer is
never cut off in transit.

### Interpreter and programmatic tool calling

- **Nested PTC calls receive a real runtime.** The old bridge issued a bare
  `tool.invoke(args)`, which skips everything LangGraph's `ToolNode` wires
  up. Tools declaring `ToolRuntime`, `InjectedState`, `InjectedStore`, or
  `InjectedToolCallId` now all work, against a child runtime derived from the
  `py_eval` invocation with its own `tool_call_id`. Arguments the generated
  program supplies for an injected parameter are discarded before injection.
- **Sync and async tools** both run, from sync and async agent invocations.
- **Results keep their structure**: `ToolMessage` and `Command` envelopes are
  unwrapped to their payload, Pydantic models keep their field shape, and
  stringification is confined to leaves with no JSON counterpart.
- **`max_ptc_calls`** (default 256, per evaluation) bounds runaway loops in
  generated code.
- **Prompt injection is block-aware.** Rebuilding `SystemMessage.content` as
  one string dropped the structured blocks memory, skills, harness profiles
  and prompt caching assemble — including `cache_control`, whose loss would
  silently re-bill the entire cached prefix.
- **`serialized_name`** pins the middleware's public identity, so harness
  profiles can exclude it by class and round-trip that through
  `HarnessProfileConfig`.
- Async lifecycle hooks no longer run blocking sandbox transfers on the event
  loop, and a checkpointer-less run no longer keys its REPL by a
  process-global fallback shared with every other concurrent run.

### Skills

The optional `import skills.<name>` bridge read `metadata["module"]`, a field
exact 0.7.4 `SkillMetadata` does not have.

- The entrypoint hint moved to `metadata["wasmsh.python_module"]`, namespaced
  so it cannot collide with a future upstream field.
- **Importability is opt-in.** A skill name that does not map to a valid
  Python identifier is not importable unless it declares
  `metadata["wasmsh.python_package"]`; upstream discovery and progressive
  disclosure keep working regardless.
- **Every regular file is staged**, not a `.py/.md/.json` allowlist that
  silently dropped scripts, SQL, templates and binary assets. Nested
  structure and bytes are preserved, bounded by explicit per-file, per-bundle
  and file-count limits that fail loudly.
- Paths are re-checked for containment after normalisation, so a backend that
  resolved a symlink out of the skill tree is rejected before upload. Both
  backend path shapes are handled — `BaseSandbox.glob` reports relative
  paths, `StoreBackend` absolute ones, and the old code only worked with the
  latter.
- Bundles are cached by content fingerprint rather than by name.

### Documentation corrected

Claims that had drifted from what the code does:

- The local VFS is an execution workspace, not durable memory. Profiles,
  memory, skills and policies belong in a `StoreBackend`.
- `namespace=` is a routing prefix, not tenant isolation — it is lexical and
  does not survive symlinks.
- Filesystem permissions require `CompositeBackend` route prefixes when the
  backend can execute; upstream refuses the graph otherwise.
- PTC allowlists are the permission boundary; `interrupt_on` does not fire
  for nested calls.
- `write_file` overwrites in 0.7, and `delete` is recursive and classified as
  a write.

A `create_deep_agent` compatibility matrix now lists every constructor input
as supported, intentionally limited, or unsupported, with each row backed by
a test that builds a real graph.

### Not included

- **Remote PTC.** `WasmshRemoteSandbox.run_ptc` still raises
  `NotImplementedError` pending ADR-0031 Phase 2. It fails loudly rather than
  degrading.
- **Deep Agents Code sandbox provider.** Blocked on a `bash -c` setup-shell
  mismatch; adding a `bash` that silently accepts unsupported syntax would be
  worse than not shipping it.
- **Capture/offload** stays disabled (`enable_capture_offload=False`).
