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

## Local Kubernetes validation (kind)

The `e2e/kind/` suite provisions a real Kubernetes cluster on the
developer box using [kind](https://kind.sigs.k8s.io), loads locally
built container images, installs `deploy/helm/wasmsh` with the
`values-e2e.yaml` overlay, and exercises the scalable path through the
public dispatcher API.

Prerequisites: `docker`, `kind`, `kubectl`, `helm`, Node ≥ 22, and one
prior `just build-pyodide` so the runner image has its wasm assets.

```bash
just build-pyodide        # once per Pyodide version bump
just test-e2e-kind        # build images + cluster + tests + teardown
just test-e2e-kind-keep   # leave cluster up after the run
just test-e2e-kind-reuse  # reuse an existing cluster (fast iteration)
just kind-down            # destroy the cluster manually
```

The suite itself lives in `e2e/kind/tests/`:

- `cluster-health.test.mjs` — dispatcher `/healthz` + `/readyz`, runner pod readiness
- `session-lifecycle.test.mjs` — create → run → write-file → read-file → close through the dispatcher
- `runner-resilience.test.mjs` — scale the runner deployment up and down, delete a runner pod mid-run

See `e2e/kind/README.md` for harness internals and flag details.

## Container images

Both images are published to GHCR on every `v*` tag by the `Release`
workflow (see `.github/workflows/release.yml`):

- `ghcr.io/mayflower/wasmsh-dispatcher:vX.Y.Z` — Rust dispatcher binary on `debian:bookworm-slim`
- `ghcr.io/mayflower/wasmsh-runner:vX.Y.Z` — Node 22 + baked Pyodide assets

A manually-dispatchable `Dev Images` workflow
(`.github/workflows/images-dev.yml`) publishes `dev-<sha>` tags for
integration testing without cutting a release. That workflow never
publishes to npm, PyPI, or crates.io and only needs `packages: write`
for GHCR push.
