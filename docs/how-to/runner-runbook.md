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
