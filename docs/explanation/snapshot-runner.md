# Snapshot Runner Architecture

The scalable path is intentionally narrow:

- one sandbox variant: `wasmsh + Pyodide`
- one template snapshot per runner process
- a single template instance per runner
- a fresh worker per session
- no warm pool of user sandboxes
- dispatcher routing based on restore capacity only
- one external control plane: the dispatcher HTTP API

## Flow

1. The baseline boot path stays offline and deterministic.
2. A template worker builds a single immutable snapshot and runs selftests.
3. Before the runner reports ready, it performs a bounded number of scratch restores to warm worker/module caches without keeping user sandboxes alive.
4. Every new session restores from that snapshot into a new worker thread.
5. External clients call the dispatcher, not the runners directly.
6. The dispatcher selects a ready runner by restore capacity and records session affinity.
7. All follow-up calls for that session are forwarded back to the same runner pod.
8. Network access goes through the broker and is checked against `allowed_hosts`.
9. The dispatcher never routes by runtime type or capability family.

This keeps restore behavior measurable while preserving strong isolation between sessions.

## External API

The dispatcher is the stable ingress surface for the scalable sandbox platform. It exposes:

- `POST /sessions`
- `POST /sessions/{session_id}/init`
- `POST /sessions/{session_id}/run`
- `POST /sessions/{session_id}/write-file`
- `POST /sessions/{session_id}/read-file`
- `POST /sessions/{session_id}/list-dir`
- `POST /sessions/{session_id}/close`
- `DELETE /sessions/{session_id}`

The runner still exposes `/healthz`, `/readyz`, `/metrics`, and `/runner/snapshot`, but those are pod-local operational endpoints for the dispatcher and platform operators.

## Kubernetes Shape

In Kubernetes the dispatcher must discover individual runner pods, not a load-balanced runner service, otherwise session affinity breaks. The supported layout is:

- a headless Service for runner pod discovery
- dispatcher pods configured with `RUNNER_SERVICE_URLS=http://wasmsh-runner-headless:8787`
- a normal `wasmsh-dispatcher` Service as the cluster entrypoint
