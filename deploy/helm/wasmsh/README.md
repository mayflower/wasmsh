# wasmsh Helm Chart

This chart deploys the scalable internal sandbox control plane as one release:

- dispatcher `Deployment` plus internal `ClusterIP` service
- runner `Deployment`
- runner headless discovery `Service`
- runner `NetworkPolicy`
- runner `HorizontalPodAutoscaler`
- optional runner `ServiceMonitor`
- optional runner `PrometheusRule`
- runner and dispatcher `PodDisruptionBudget`

## Installation

```bash
helm upgrade --install wasmsh deploy/helm/wasmsh
```

For a production-style render:

```bash
helm template wasmsh deploy/helm/wasmsh \
  --values deploy/helm/wasmsh/values-prod.yaml
```

## Public Values Contract

The main values surface is:

- `dispatcher.image.*`, `runner.image.*`
- `dispatcher.replicaCount`, `runner.replicaCount`
- `runner.restoreSlots`, `runner.startupWarmRestores`
- `runner.fetchBroker.*`
- `runner.workerResourceLimits.*`
- `dispatcher.resources`, `runner.resources`
- `dispatcher.service.*`, `runner.service.*`
- `networkPolicy.*`
- `autoscaling.*`
- `monitoring.serviceMonitor.*`
- `monitoring.prometheusRule.*`
- placement and security fields:
  - `priorityClassName`
  - `nodeSelector`
  - `tolerations`
  - `affinity`
  - `topologySpreadConstraints`
  - `podSecurityContext`
  - `securityContext`

## Operational Defaults

- Dispatcher stays internal-only via `ClusterIP`.
- Runner discovery uses a headless service.
- Runner autoscaling is enabled by default on `wasmsh_inflight_restores`.
- Snapshot/runtime assets are assumed to be baked into the runner image.
- No public ingress is created by default.
- Monitoring resources are opt-in because they depend on Prometheus Operator CRDs.
