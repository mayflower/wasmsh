# ADR-0021: Network Capability Model

## Status

Accepted

## Context

wasmsh is a fully sandboxed shell runtime with no network access by default. LLM agents using the sandbox frequently need to fetch data from APIs or download files. Rather than opening unrestricted network access, we need a controlled mechanism where the sandbox creator specifies exactly which hosts are reachable.

## Decision

Network access uses a **capability-based allowlist model**. The sandbox creator provides a list of permitted hosts at initialization. The default is an empty list, meaning no network access — preserving full backward compatibility.

### Architecture

A `NetworkBackend` trait in `wasmsh-utils` provides the network capability to `curl` and `wget` utilities via `UtilContext`, mirroring how `BackendFs` provides filesystem access via `UtilContext.fs`.

```
HostCommand::Init { step_budget, allowed_hosts: ["api.example.com"] }
    │
    ▼
WorkerRuntime.network: Option<Box<dyn NetworkBackend>>
    │
    ▼
UtilContext.network: Option<&dyn NetworkBackend>
    │
    ▼
curl/wget → backend.fetch(HttpRequest) → HttpResponse
```

### Allowlist patterns

- Exact hostname: `api.example.com`
- Wildcard subdomain: `*.example.com` (matches `foo.example.com` and bare `example.com`)
- IP address: `192.168.1.100`
- Host with port: `api.example.com:8080` (only matches that specific port)

### Defense in depth

URL validation happens at three layers:

1. **Rust `HostAllowlist`** in `NetworkBackend::fetch()` — primary enforcement, runs before any I/O
2. **JS-side validation** in the worker's fetch implementation — secondary check
3. **Browser CORS policy** — inherent to `XMLHttpRequest`, provides tertiary enforcement

### Synchronous HTTP

Utilities are synchronous (`fn -> i32`). The HTTP call uses **synchronous `XMLHttpRequest`** which is available in Web Worker contexts (both standalone wasm-bindgen and Pyodide Emscripten workers). While sync XHR is deprecated on the main thread, it remains fully functional in Web Workers and is the only viable synchronous I/O mechanism for WASM-in-worker without requiring `SharedArrayBuffer` and cross-origin isolation.

## Alternatives Considered

### Protocol extension (bidirectional messaging)

Adding `HostCommand::Fetch` / `WorkerEvent::FetchResult` would require restructuring the protocol from request-response-per-command to a bidirectional stream where the worker can send requests to the host mid-execution. This is a much larger architectural change for a single feature.

### External command handler

Routing `curl`/`wget` through the existing `ExternalCommandHandler` would bypass the utility registry pattern and lose access to `UtilContext` (filesystem for `-o` output, shell state for variable expansion). The handler also has a different ownership model that doesn't fit.

## Consequences

- `curl` and `wget` are available as utilities (88 total, up from 86)
- No network access without explicit opt-in via `allowed_hosts`
- Existing sandboxes are unaffected (empty allowlist is the default)
- The `url` crate is added as a dependency (~50KB compiled) for RFC-compliant URL parsing
- The `HostCommand::Init` protocol message gains an `allowed_hosts` field (serde-defaulted, backward compatible)
- Platform backends (standalone, Pyodide, Node) each implement the synchronous fetch mechanism appropriate to their environment
