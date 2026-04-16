# Runner Metrics

The scalable runner exports Prometheus-compatible metrics on `/metrics`.

## Required metrics

- `wasmsh_session_restore_duration_ms`
- `wasmsh_restore_stage_duration_ms`
- `wasmsh_active_sessions`
- `wasmsh_inflight_restores`
- `wasmsh_restore_queue_depth`
- `wasmsh_snapshot_restore_failures_total`
- `wasmsh_allowed_host_denied_total`

## Interpretation

- `wasmsh_session_restore_duration_ms` tracks end-to-end session restore latency. Use the `p95` series for the readiness SLO.
- `wasmsh_restore_stage_duration_ms` breaks restore into stages such as worker spawn and sandbox restore.
- `wasmsh_active_sessions` shows live user sandboxes.
- `wasmsh_inflight_restores` and `wasmsh_restore_queue_depth` show restore pressure and are suitable for autoscaling.
- `wasmsh_snapshot_restore_failures_total` counts failed restores from the immutable snapshot artifact.
- `wasmsh_allowed_host_denied_total` counts broker-side `allowed_hosts` denials.
