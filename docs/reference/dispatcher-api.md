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

- `GET /healthz`
- `GET /readyz`

Runner pods additionally expose:

- `GET /healthz`
- `GET /readyz`
- `GET /metrics`
- `GET /runner/snapshot`
