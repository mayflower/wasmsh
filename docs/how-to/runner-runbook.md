# Runner Runbook

## Failure Modes

### `/readyz` returns `503`

- verify the snapshot artifact exists and matches the configured digest
- verify the template worker booted successfully
- inspect the selftest payload reported by the runner

### Restore latency climbs

- inspect `wasmsh_session_restore_duration_ms`
- inspect `wasmsh_restore_stage_duration_ms`
- compare `wasmsh_restore_queue_depth` with `wasmsh_inflight_restores`

### Network access is denied unexpectedly

- inspect `wasmsh_allowed_host_denied_total`
- confirm the session `allowed_hosts` policy sent by the dispatcher
- confirm the Kubernetes egress policy still allows required broker traffic such as DNS

## Kubernetes-specific failure modes

### `ImagePullBackOff` on dispatcher or runner pods

- the chart's `values.yaml` ships sentinel digests by default; they must
  be overridden at install time with the real digests from the release
  artifact `image-digests.json`, or via a values overlay that sets
  `dispatcher.image.digest` and `runner.image.digest`
- alternatively set `image.digest: ""` and `image.tag: "vX.Y.Z"` to pull
  by tag (loses immutable-reference guarantees)
- verify the cluster can reach `ghcr.io` and has `imagePullSecrets`
  configured if pulling from a private repository mirror

### HPA stays at `minReplicas` under load

- verify prometheus-adapter (or KEDA) is installed and exposes
  `wasmsh_inflight_restores` via `custom.metrics.k8s.io`
- run `kubectl get --raw "/apis/custom.metrics.k8s.io/v1beta1/namespaces/<ns>/pods/*/wasmsh_inflight_restores"`
  — if the adapter returns 404, the metric is not being scraped
- confirm the `ServiceMonitor` is enabled (`monitoring.serviceMonitor.enabled=true`)
  and that the Prometheus Operator picks it up in the right namespace

### Pod drain exceeds `terminationGracePeriodSeconds`

- inspect `wasmsh_active_sessions` at the moment of SIGTERM; long-running
  sessions can outlive the grace period
- confirm the dispatcher stopped routing to the draining pod: its
  `/runner/snapshot` should report `draining: true` on the next refresh
  tick (5 s)
- if sessions regularly need >60 s to finish, raise
  `runner.terminationGracePeriodSeconds` in values rather than letting
  kubelet SIGKILL mid-session

### `NetworkPolicy` blocks dispatcher → runner traffic

- default policy allows ingress on `:8787` only from dispatcher pods by
  label — a renamed or relabeled dispatcher Deployment will not match
- egress is locked to DNS (UDP/TCP 53) by default; add
  `networkPolicy.extraEgress` entries for any external hosts the
  runner's fetch broker needs to reach (pypi mirrors, etc.)
- kind's kindnetd does not enforce NetworkPolicy; rules are silently
  permitted there. To validate enforcement use Cilium or Calico

### Session creation returns 503 with `E_RUNNER_DRAINING`

- every runner pod in the deployment is draining simultaneously; this
  is usually a rollout bug (`maxUnavailable` too high) rather than load
- confirm `updateStrategy.rollingUpdate.maxUnavailable=0` in values; the
  default is `maxSurge: 1, maxUnavailable: 0`, which keeps all existing
  pods serving during a rollout
