# ADR-0006: Capability-Based VFS with OPFS Backend

## Status
Accepted

## Context
There is no normal POSIX filesystem in the browser. At the same time, wasmsh needs a virtual, sandbox-capable FS.

## Decision
wasmsh uses its own VFS interface with `MemoryFs`, `OverlayFs`, and optionally `OpfsFs`.

## Consequences
- Deterministic FS behavior
- Isolatable sessions
- Good foundation for sandboxing and snapshots
