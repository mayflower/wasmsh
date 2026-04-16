# Snapshot Runner Architecture

The scalable path is intentionally narrow:

- one sandbox variant: `wasmsh + Pyodide`
- one template snapshot per runner process
- a single template instance per runner
- a fresh worker per session
- no warm pool of user sandboxes
- dispatcher routing based on restore capacity only

## Flow

1. The baseline boot path stays offline and deterministic.
2. A template worker builds a single immutable snapshot and runs selftests.
3. Every new session restores from that snapshot into a new worker thread.
4. Network access goes through the broker and is checked against `allowed_hosts`.
5. The dispatcher keeps session affinity but never routes by runtime type or capability family.

This keeps restore behavior measurable while preserving strong isolation between sessions.
