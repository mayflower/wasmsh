function percentile(samples, value) {
  if (samples.length === 0) {
    return 0;
  }
  const sorted = [...samples].sort((left, right) => left - right);
  const index = Math.min(sorted.length - 1, Math.ceil((value / 100) * sorted.length) - 1);
  return sorted[index];
}

export const REQUIRED_RUNNER_METRICS = Object.freeze([
  "wasmsh_session_restore_duration_ms",
  "wasmsh_restore_stage_duration_ms",
  "wasmsh_active_sessions",
  "wasmsh_inflight_restores",
  "wasmsh_restore_queue_depth",
  "wasmsh_snapshot_restore_failures_total",
  "wasmsh_allowed_host_denied_total",
  "wasmsh_broker_fetch_errors_total",
]);

export function renderPrometheusMetrics(snapshot) {
  const lines = [
    "# HELP wasmsh_session_restore_duration_ms Restore latency summary for session creation.",
    "# TYPE wasmsh_session_restore_duration_ms gauge",
    `wasmsh_session_restore_duration_ms{quantile="p95"} ${snapshot.wasmsh_session_restore_duration_ms.p95}`,
    "# HELP wasmsh_active_sessions Active user sessions on this runner.",
    "# TYPE wasmsh_active_sessions gauge",
    `wasmsh_active_sessions ${snapshot.wasmsh_active_sessions}`,
    "# HELP wasmsh_inflight_restores Restores currently in progress.",
    "# TYPE wasmsh_inflight_restores gauge",
    `wasmsh_inflight_restores ${snapshot.wasmsh_inflight_restores}`,
    "# HELP wasmsh_restore_queue_depth Restore queue depth observed by the runner.",
    "# TYPE wasmsh_restore_queue_depth gauge",
    `wasmsh_restore_queue_depth ${snapshot.wasmsh_restore_queue_depth}`,
    "# HELP wasmsh_snapshot_restore_failures_total Total snapshot restore failures.",
    "# TYPE wasmsh_snapshot_restore_failures_total counter",
    `wasmsh_snapshot_restore_failures_total ${snapshot.wasmsh_snapshot_restore_failures_total}`,
    "# HELP wasmsh_allowed_host_denied_total Broker denied-host decisions.",
    "# TYPE wasmsh_allowed_host_denied_total counter",
    `wasmsh_allowed_host_denied_total ${snapshot.wasmsh_allowed_host_denied_total}`,
    "# HELP wasmsh_broker_fetch_errors_total Broker fetch failures by reason.",
    "# TYPE wasmsh_broker_fetch_errors_total counter",
  ];
  for (const [reason, count] of Object.entries(snapshot.wasmsh_broker_fetch_errors_total)) {
    lines.push(`wasmsh_broker_fetch_errors_total{reason="${reason}"} ${count}`);
  }

  const stageMetrics = snapshot.wasmsh_restore_stage_duration_ms;
  lines.push(
    "# HELP wasmsh_restore_stage_duration_ms Restore stage latency summary.",
    "# TYPE wasmsh_restore_stage_duration_ms gauge",
  );
  for (const [stage, values] of Object.entries(stageMetrics)) {
    lines.push(`wasmsh_restore_stage_duration_ms{stage="${stage}",quantile="p95"} ${values.p95}`);
  }
  return `${lines.join("\n")}\n`;
}

export function createRunnerMetrics() {
  const restoreDurations = [];
  const restoreStageDurations = new Map();
  let inflightRestores = 0;
  let restoreQueueDepth = 0;
  let activeSessions = 0;
  let restoreFailures = 0;
  let deniedHosts = 0;
  const brokerFetchErrors = new Map();

  return {
    startRestore(queueDepth) {
      inflightRestores += 1;
      restoreQueueDepth = queueDepth;
      const startedAt = performance.now();
      const stageStarts = new Map();
      const stageDurations = new Map();
      return {
        beginStage(name) {
          stageStarts.set(name, performance.now());
        },
        endStage(name) {
          const started = stageStarts.get(name);
          if (started !== undefined) {
            stageDurations.set(name, performance.now() - started);
          }
        },
        finish() {
          inflightRestores = Math.max(0, inflightRestores - 1);
          restoreQueueDepth = Math.max(0, restoreQueueDepth - 1);
          const total = performance.now() - startedAt;
          restoreDurations.push(total);
          for (const [name, duration] of stageDurations.entries()) {
            const samples = restoreStageDurations.get(name) ?? [];
            samples.push(duration);
            restoreStageDurations.set(name, samples);
          }
          return {
            total,
            stages: Object.fromEntries(stageDurations),
          };
        },
        fail() {
          inflightRestores = Math.max(0, inflightRestores - 1);
          restoreQueueDepth = Math.max(0, restoreQueueDepth - 1);
          restoreFailures += 1;
        },
      };
    },
    sessionOpened() {
      activeSessions += 1;
    },
    sessionClosed() {
      activeSessions = Math.max(0, activeSessions - 1);
    },
    hostDenied() {
      deniedHosts += 1;
    },
    brokerFetchError(reason) {
      const key = typeof reason === "string" && reason.length > 0 ? reason : "unknown";
      brokerFetchErrors.set(key, (brokerFetchErrors.get(key) ?? 0) + 1);
    },
    snapshot() {
      return {
        wasmsh_session_restore_duration_ms: {
          samples: [...restoreDurations],
          p95: percentile(restoreDurations, 95),
        },
        wasmsh_restore_stage_duration_ms: Object.fromEntries(
          Array.from(restoreStageDurations.entries(), ([name, samples]) => [
            name,
            {
              samples: [...samples],
              p95: percentile(samples, 95),
            },
          ]),
        ),
        wasmsh_active_sessions: activeSessions,
        wasmsh_inflight_restores: inflightRestores,
        wasmsh_restore_queue_depth: restoreQueueDepth,
        wasmsh_snapshot_restore_failures_total: restoreFailures,
        wasmsh_allowed_host_denied_total: deniedHosts,
        wasmsh_broker_fetch_errors_total: Object.fromEntries(brokerFetchErrors),
      };
    },
  };
}
