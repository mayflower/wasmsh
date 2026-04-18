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
  "command": "python - <<'PY'\nprint('hello')\nPY"
}
```

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
