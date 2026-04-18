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

### Container images

Two images back the chart, both built by multi-stage Dockerfiles under
`deploy/docker/`:

- `ghcr.io/mayflower/wasmsh-dispatcher` — Rust release binary on
  `debian:bookworm-slim`, tini as PID 1, non-root UID 10001.
- `ghcr.io/mayflower/wasmsh-runner` — Node 22 slim with `tools/runner-node`
  + the prebuilt `packages/npm/wasmsh-pyodide/assets/` baked in. The
  Dockerfile fails the build if `pyodide.asm.wasm` is missing, so
  `just build-pyodide` must precede `just build-images`.

The `Release` GitHub Actions workflow publishes both on every `v*` tag
with `:vX.Y.Z`, `:X.Y.Z`, and `:latest` tags plus SLSA provenance and
SBOM attestations. The resulting digests are captured into
`image-digests.json` and attached to the GitHub Release.

### Services and ports

With default release name `wasmsh` and namespace `wasmsh`:

| Service | Type | Port | Purpose |
|-|-|-|-|
| `wasmsh-dispatcher` | ClusterIP | 8080/TCP | The only client-facing endpoint; HTTP API described below |
| `wasmsh-runner-headless` | headless (`clusterIP: None`) | 8787/TCP | DNS-based pod enumeration consumed by the dispatcher |

No Ingress or LoadBalancer is provisioned by the chart — the platform is
internal-only. Put an Ingress with authn/rate-limiting in front of the
dispatcher service if you need external access.

### Scaling

- The runner Deployment is autoscaled via a `HorizontalPodAutoscaler` on
  the custom metric `wasmsh_inflight_restores` (`type: Pods`,
  `averageValue: 2`). Requires prometheus-adapter (or KEDA) to expose
  the metric through the `custom.metrics.k8s.io` API.
- Per-pod capacity is bounded by `WASMSH_RESTORE_SLOTS` (default 4) and
  pre-warmed by `WASMSH_STARTUP_WARM_RESTORES` (default 2).
- Dispatcher routing is `restore-capacity-only`: pick the least-loaded
  runner with `free_restore_slots > 0`.
- Scale-down is graceful via SIGTERM → runner drain → dispatcher stops
  routing new sessions → in-flight work finishes within
  `terminationGracePeriodSeconds`.

### Local validation (kind)

The `e2e/kind/` suite boots a real single-node kind cluster, loads
locally built images, installs the chart with `values-e2e.yaml`, and
exercises the scalable path end-to-end (health, session lifecycle,
scaling, pod-delete resilience):

```bash
just build-pyodide           # once per Pyodide version
just test-e2e-kind           # full cycle with teardown
```

See `e2e/kind/README.md` for `--keep` / `--reuse` modes and filter flags.
