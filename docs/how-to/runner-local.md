# Local Runner How-To

The scalable runner uses exactly one sandbox variant: `wasmsh + Pyodide`. It keeps one template snapshot per runner process and starts a fresh worker thread for every user session.

## Local workflow

1. Build the linked Pyodide runtime with `just build-pyodide`.
2. Produce the immutable snapshot layout with `just build-snapshot`.
3. Run the runner contracts with `just test-e2e-runner-node`.
4. Run the restore smoke benchmark with `just bench-runner-restore`.

## Readiness model

`/readyz` must stay false until the runner has:

- loaded the snapshot bytes
- booted the template worker
- validated the template selftests

There is no warm pool for user sandboxes. Capacity comes from restore slots, not pre-restored sessions.
