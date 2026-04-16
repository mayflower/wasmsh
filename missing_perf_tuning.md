# Missing Performance Tuning

## Status

The scalable snapshot runner is implemented, but the restore latency target is not yet met.

- target: restore `<100 ms` p95 on a ready pod
- latest isolated local smoke bench: `p95 ~= 269.6 ms`
- benchmark command:

```sh
just bench-runner-restore
```

This means the architecture is in place, but the performance-tuning phase is still incomplete.

## What Is Already Done

- baseline boot is offline and deterministic
- a single template worker builds one immutable snapshot
- every session restores into a fresh worker
- restore stages are exported through runner metrics
- the dispatcher routes by restore capacity instead of runtime variants

The remaining work is about latency reduction, not missing functionality.

## Likely Cost Centers

The current implementation suggests these are the most likely contributors to the remaining p95 gap:

1. worker-thread startup cost for each fresh session
2. snapshot deserialize and restore cost inside Pyodide/Emscripten
3. JS host boot work that still happens after restore
4. file seeding and init RPC overhead on the session path
5. CPU contention and scheduling variance during restore

## Missing Tuning Work

### 1. Measure Restore Stages On An Idle Ready Pod

The local bench is useful, but the acceptance target is explicitly pod-scoped.

Missing work:

- run repeated restore benches against a ready Kubernetes pod
- capture stage-level histograms from `wasmsh_restore_stage_duration_ms`
- separate cold pod startup from steady-state restore latency
- record p50, p95, and p99 under `1`, `2`, `4`, and `8` concurrent creates

### 2. Reduce Post-Restore Work

The restore path should do as little as possible after snapshot load.

Missing work:

- audit `session-worker.mjs` and `restore.mjs` for setup that can move into snapshot build time
- precompute any remaining deterministic JS/Python initialization into the template snapshot
- avoid repeated manifest or package-index work during session init
- avoid repeated object allocation on the hot path where a stable shared structure is enough

### 3. Shrink Snapshot Restore Cost

The snapshot may still be larger or noisier than necessary.

Missing work:

- measure snapshot artifact size and restore cost correlation
- remove anything from the baseline image that is not required for first-command readiness
- confirm no unnecessary Python modules, JS refs, or filesystem entries are captured
- test whether alternate compression or memory layout choices reduce restore time

### 4. Tune Worker Creation Overhead

A fresh worker per session is a hard constraint, so the tuning target is worker creation cost, not pooling.

Missing work:

- measure pure `Worker` spawn time separately from snapshot restore time
- reduce module-load work in `session-worker.mjs`
- ensure large immutable state stays in the template process or shared host memory where possible
- check whether worker startup is paying avoidable URL/path resolution or asset probing costs

### 5. Tune Pod Resources For Steady Restore Latency

The `<100 ms` target is unlikely to hold without stable CPU allocation.

Missing work:

- benchmark with explicit CPU requests/limits tuned for low-variance restore work
- test whether one template worker plus active restore workers causes CPU starvation
- validate restore-slot settings against actual available cores
- check whether HPA behavior introduces latency spikes during scale transitions

### 6. Add A Real Performance Gate

The current bench is informative, but it is not yet an enforcement gate for the target SLO.

Missing work:

- add a pod-level benchmark job that runs against a ready runner deployment
- fail CI or release promotion if ready-pod restore p95 exceeds the agreed threshold
- store benchmark history so regressions are obvious instead of anecdotal

## Recommended Tuning Order

1. benchmark a ready pod with stage timings enabled
2. identify whether startup, restore, or post-restore init dominates p95
3. move deterministic init into snapshot build time
4. trim snapshot contents until restore cost stops dropping
5. retune restore slots and pod CPU sizing
6. turn the measured threshold into an automated gate

## Definition Of Done

Performance tuning for the scalable runner is complete only when all of the following are true:

- ready-pod restore p95 is below `100 ms`
- the result is reproducible across repeated runs, not a one-off best case
- the benchmark excludes cold pod startup
- the runner still preserves:
  fresh worker per session
  single template instance per runner
  no warm pool of user sandboxes
  broker-enforced `allowed_hosts`

## Notes

- The current implementation should be treated as functionally complete but not performance-complete.
- The local `269.6 ms` p95 result is the clearest remaining gap.
- Any future change that improves p95 by weakening isolation or introducing warm user pools is out of scope.
