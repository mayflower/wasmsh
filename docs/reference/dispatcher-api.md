# Dispatcher API

The scalable sandbox control plane is exposed by the dispatcher HTTP service.

## Endpoints

### `POST /sessions`

Creates a new session and binds it to one runner pod.

Request body:

```json
{
  "session_id": "optional-stable-id",
  "allowed_hosts": ["files.pythonhosted.org", "pypi.org"],
  "step_budget": 1000000,
  "initial_files": [
    {
      "path": "/workspace/input.txt",
      "content_base64": "aGVsbG8K"
    }
  ]
}
```

### `POST /sessions/{session_id}/init`

Runs session initialization on the already selected runner.

### `POST /sessions/{session_id}/run`

Executes a one-shot command.

Request body:

```json
{
  "command": "python - <<'PY'\nprint('hello')\nPY",
  "timeout_ms": 60000
}
```

`timeout_ms` is optional. It is a wall-clock deadline for this one command,
enforced by the **runner** — not by the caller's socket. Pyodide runs the
shell synchronously inside the worker and offers no cancellation point, so
the runner terminates the worker when the deadline passes.

Two consequences follow, and both are deliberate:

- The response is a normal `200` carrying a structured result rather than a
  transport error, so a caller can tell "the command ran out of time" from
  "the dispatcher is unreachable":

  ```json
  {
    "ok": true,
    "sessionId": "…",
    "result": {
      "output": "Error: command timed out after 60s and was terminated: …",
      "exitCode": 124,
      "timedOut": true
    }
  }
  ```

  `124` is GNU `timeout(1)`'s convention.

- **The session is destroyed.** An abandoned evaluation leaves interpreter
  state no one should read, so the session is closed and every later request
  for it returns `404`. Use `step_budget` when you want a bound that leaves
  the session alive.

Values are clamped to one hour. A missing, zero, negative, or non-numeric
value means "no explicit deadline" and falls back to the runner's own
request ceiling.

The dispatcher widens its own upstream socket timeout past `timeout_ms` so
the runner's authoritative answer is never cut off in transit.

### `POST /sessions/{session_id}/write-file`

Writes a file into the sandbox.

### `POST /sessions/{session_id}/read-file`

Reads a file from the sandbox. The response includes `contentBase64` because the runner API keeps binary-safe file transport in base64.

### `POST /sessions/{session_id}/list-dir`

Lists directory entries at an absolute sandbox path.

### `POST /sessions/{session_id}/close`

Closes the session and releases dispatcher affinity.

### `DELETE /sessions/{session_id}`

Deletes the session and releases dispatcher affinity.

## Operational endpoints

The dispatcher also exposes:

- `GET /healthz` — always returns 200 once the process is up
- `GET /readyz` — 200 once the dispatcher has discovered at least one
  ready runner via `RUNNER_SERVICE_URLS`; 503 otherwise

Runner pods additionally expose (not proxied through the dispatcher;
intended for platform operators and the dispatcher itself):

- `GET /healthz`
- `GET /readyz`
- `GET /metrics` — Prometheus exposition including
  `wasmsh_inflight_restores`, `wasmsh_restore_queue_depth`,
  `wasmsh_session_restore_duration_ms`, `wasmsh_active_sessions`,
  `wasmsh_broker_fetch_errors_total`
- `GET /runner/snapshot` — routing metadata consumed by the dispatcher
  (`inflight_restores`, `restore_slots`, `draining`, selftest results)
- `POST /runner/drain` — flip the pod into drain mode so the dispatcher
  stops sending new sessions; existing affinity-pinned sessions
  continue. Invoked automatically on `SIGTERM`

## Kubernetes service names

When deployed via `deploy/helm/wasmsh` with release name `wasmsh` and
namespace `wasmsh`:

| Service | Target | Consumers |
|-|-|-|
| `wasmsh-dispatcher` (ClusterIP, 8080) | the endpoints above | external clients |
| `wasmsh-runner-headless` (headless, 8787) | operational endpoints | the dispatcher only |

Outside-cluster callers must reach the dispatcher through their own
Ingress / LoadBalancer; the chart does not provision one.
