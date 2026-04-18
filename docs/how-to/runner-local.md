# Local Runner How-To

The scalable runner uses exactly one sandbox variant: `wasmsh + Pyodide`. It keeps one template snapshot per runner process and starts a fresh worker thread for every user session.

## Local workflow

1. Build the linked Pyodide runtime with `just build-pyodide`.
2. Produce the immutable snapshot layout with `just build-snapshot`.
3. Run the runner and dispatcher contracts with `just test-e2e-runner-node`.
4. Run the restore smoke benchmark with `just bench-runner-restore`.
5. Run the concurrent restore/load benchmark with `just bench-runner-load` (defaults to `n=2000`, `c=100`) or `node tools/perf/runner-restore-bench.mjs -n 10 -c 16`.

## Local API surface

The scalable sandbox is reached through the dispatcher HTTP API. In local tests that API exposes:

- `POST /sessions`
- `POST /sessions/{session_id}/init`
- `POST /sessions/{session_id}/run`
- `POST /sessions/{session_id}/write-file`
- `POST /sessions/{session_id}/read-file`
- `POST /sessions/{session_id}/list-dir`
- `POST /sessions/{session_id}/close`
- `DELETE /sessions/{session_id}`

The runner process itself is not the public ingress. It serves:

- `/healthz`
- `/readyz`
- `/metrics`
- `/runner/snapshot`

## Readiness model

`/readyz` must stay false until the runner has:

- loaded the snapshot bytes
- booted the template worker
- validated the template selftests
- completed the bounded startup warm restores used to warm worker/module caches

There is no warm pool for user sandboxes. Capacity comes from restore slots, not pre-restored sessions.

## Cluster wiring

Use a headless runner Service for dispatcher discovery. A normal load-balanced runner Service will break per-session affinity because follow-up requests can land on a different runner pod than the one that created the session.
