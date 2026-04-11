# ADR-0030: WASI P2 Component Model transport

## Status

Accepted

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
**WASI P2 Component Model component** exporting a custom WIT interface
rather than a CLI entrypoint.

The WIT world lives in `crates/wasmsh-component/wit/world.wit`:

```wit
package wasmsh:component@0.1.0;

interface sandbox {
  record runtime-config { step-budget: u64, allowed-hosts: list<string> }
  enum diagnostic-level { info, warning, error, trace }
  variant event { stdout, stderr, exit, yielded, diagnostic, fs-changed, version }

  resource session {
    constructor(config: runtime-config);
    run: func(input: string) -> list<event>;
    read-file: func(path: string) -> list<event>;
    write-file: func(path: string, data: list<u8>) -> list<event>;
    list-dir: func(path: string) -> list<event>;
    mount: func(path: string) -> list<event>;
  }

  run-once: func(config: runtime-config, input: string) -> list<event>;
}

world wasmsh { export sandbox; }
```

Key properties:

- **Stateful `session` resource.** One instance owns one initialised
  `WorkerRuntime`. Session state lives in the resource — no process-wide
  singleton, no `static mut`, no sandbox-id registry.
- **Close to `HostCommand` / `WorkerEvent`.** `runtime-config` mirrors
  `HostCommand::Init`; `event` mirrors `WorkerEvent` variant-for-variant.
  The WIT surface is a typed projection of the existing protocol, not a
  parallel contract.
- **Host-testable core.** The work happens in a plain Rust `SessionCore`
  type inside `crates/wasmsh-component` that is unit-tested on the native
  host. The wit-bindgen-generated bindings and the thin resource glue
  live under a `component-export` feature flag so `cargo test -p
  wasmsh-component` links on any target without pulling wit-bindgen.
- **Convenience `run-once`.** For smoke tests and simple hosts, an
  ephemeral session is built, the input is executed, and the combined
  event list is returned. The build-contract test exercises exactly this
  path with `wasmtime run --invoke` where available.
- **No network backend today.** `allowed_hosts` is carried through to
  `HostCommand::Init` for forward compatibility but no network backend is
  installed on the component target. The host-side wasmCloud integration
  will wire that in its own layer.

## Consequences

- A Component Model component is the natural seam for the later wasmCloud
  host-plugin and DeepAgents adapter — both are host-side consumers of
  WIT, not of a hand-rolled JSON framing.
- The existing Standalone and Pyodide transports are untouched. Browser
  and Pyodide E2E behavior is not affected.
- The toolchain gains `wasm32-wasip2` in `rust-toolchain.toml`. CI
  installs `wasmtime` and `wasm-tools` so the build-contract test can
  verify both the WIT export shape and a live `run-once` invocation.
- Multi-tenant session registries, sandbox IDs, and the wasmCloud host
  plugin are explicitly deferred. The component contract is deliberately
  minimal so those layers can be designed in their own ADRs on top of
  this one.
- Python/Pyodide parity for the component target is also deferred. The
  Pyodide transport already covers the "embedded in a Python runtime"
  use case; the component target's audience is Component Model hosts.
- The `wasmsh-protocol` crate remains canonical. The component WIT is a
  typed projection, not a replacement. `wasmsh-protocol/wit/worker-protocol.wit`
  (the `package wasmsh:protocol` typed mirror from ADR-0029's prompt 11)
  stays in place as the reference schema for the protocol-level enums;
  `wasmsh:component/sandbox` is a different package targeting a different
  consumer shape (session-resource, not raw command/event enums).

## Out of scope

- DeepAgents adapter
- wasmCloud host plugin / lattice wiring
- Multi-tenant sandbox registry / sandbox IDs
- Python parity for the component target
- Progressive execution (`StartRun` / `PollRun`) on the component surface
- Cancellation on the component surface beyond dropping the session
  resource
