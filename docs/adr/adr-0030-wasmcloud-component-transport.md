# ADR-0030: WASI P2 Component Model transport

## Status

**Superseded.** The wasmCloud-facing WASI P2 Component transport was
implemented but abandoned in favour of the scalable `wasmsh-dispatcher`
+ `wasmsh-runner` path, which ships the same JSON `HostCommand` /
`WorkerEvent` protocol over plain HTTP/Kubernetes and has a
first-party LangChain Deep Agents client (`WasmshRemoteSandbox`).  The
`wasmsh-component` crate, its WIT surface, the `wasm32-wasip2` target
pin, and the associated CI job have been removed from the tree.  The
`wasmsh-json-bridge` crate — the shared JSON transport that both the
component and Pyodide embeddings used — stays, because the Pyodide
embedding still depends on it.

This ADR is retained for historical context: it documents why the
third-target Component Model path was attempted, what it cost, and
what replaced it.

---

(Original "Accepted" decision follows.)

## Context

`wasmsh` already has a canonical host protocol (`wasmsh-protocol`) and two
transports over it:

1. **Standalone** — native Rust calls into `WorkerRuntime` from the
   wasm-bindgen-backed `wasmsh-browser` adapter.
2. **Pyodide** — JSON over a C ABI, consumed by the `wasmsh-pyodide`
   adapter and surfaced as `wasmsh_runtime_handle_json(...)` to the npm
   package.

The next integration target is using `wasmsh` as the sandbox engine behind a
wasmCloud-backed DeepAgents sandbox. That integration eventually needs a
host-side plugin, a lattice topology, a DeepAgents adapter, and a
multi-tenant sandbox registry keyed by session IDs — none of which are
specific to any one language or embedder.

A minimal "third compilation target" would be to produce a `wasi:cli/run`
wrapper that accepts shell input on stdin and emits events on stdout. That
is easy to build, but it does **not** create a reusable sandbox control
surface: every embedder would still reinvent message framing on top of the
CLI, and resource-scoped state would have to be faked with subprocess
lifetimes.

## Decision

Add a third transport — `wasmsh-component` — that ships as a
**WASI P2 Component Model component** exporting a thin projection of the
existing Pyodide JSON bridge rather than a second typed sandbox API or a
CLI entrypoint.

The WIT world lives in `crates/wasmsh-component/wit/world.wit`:

```wit
package wasmsh:component@0.1.0;

interface runtime {
  resource handle {
    constructor();
    handle-json: func(input: string) -> string;
  }

  probe-version: func() -> string;
  probe-write-text: func(path: string, text: string) -> s32;
  probe-file-equals: func(path: string, expected: string) -> bool;
}

world wasmsh { export runtime; }
```

Key properties:

- **Canonical transport stays JSON.** The serde JSON form of
  `HostCommand` / `WorkerEvent` remains the contract. `handle-json` is the
  only command surface for the component target.
- **Shared implementation with Pyodide.** Both `wasmsh-pyodide` and
  `wasmsh-component` delegate to the same `wasmsh-json-bridge` crate for
  JSON parsing, runtime dispatch, and event serialization. This prevents
  transport drift.
- **Shared probe surface.** `probe-version`, `probe-write-text`, and
  `probe-file-equals` mirror the existing Pyodide probe helpers and are
  backed by the same shared helper implementation.
- **Shared libc-backed filesystem path.** On `wasm32-wasip2`,
  `wasmsh-fs::BackendFs` selects the same libc-backed backend that the
  Pyodide path already uses instead of falling back to `MemoryFs`. The
  component build-contract test proves this with real `/workspace` file I/O
  under Wasmtime preopens.
- **Deterministic network semantics.** `Init.allowed_hosts` still matters.
  The shared bridge always installs a backend on `Init`; Pyodide uses its
  host fetch backend, while the component target installs an explicit
  deterministic deny/error backend until a real host network implementation
  exists.

## Consequences

- A Component Model component is the natural seam for the later wasmCloud
  host-plugin and DeepAgents adapter — both are host-side consumers of
  WIT, but they now consume the same JSON transport already used by
  Pyodide instead of a bespoke typed session API.
- The existing Standalone and Pyodide transports are untouched. Browser
  behavior is untouched; the Pyodide transport now shares the JSON bridge
  and probe helper implementation with the component target.
- The toolchain gains `wasm32-wasip2` in `rust-toolchain.toml`. CI
  installs `wasmtime` and `wasm-tools` so the build-contract test can
  verify both the WIT export shape and live probe helper invocation.
- Multi-tenant session registries, sandbox IDs, and the wasmCloud host
  plugin are explicitly deferred. The component contract is deliberately
  minimal so those layers can be designed in their own ADRs on top of
  this one.
- The `wasmsh-protocol` crate remains canonical. `wasmsh:component/runtime`
  is an embedding contract over that schema, while
  `wasmsh-protocol/wit/worker-protocol.wit` stays as the experimental typed
  mirror of the raw enums.

## Out of scope

- DeepAgents adapter
- wasmCloud host plugin / lattice wiring
- Multi-tenant sandbox registry / sandbox IDs
- Any second typed command/event API for the component target
- Host-managed network backend for the component target
