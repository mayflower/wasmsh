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

## Images

The chart references two images:

- `ghcr.io/mayflower/wasmsh-dispatcher`
- `ghcr.io/mayflower/wasmsh-runner`

Both are built from `deploy/docker/Dockerfile.{dispatcher,runner}` and
pushed to GHCR by the `Release` GitHub Actions workflow on every `v*`
tag, with `:vX.Y.Z`, `:X.Y.Z`, and `:latest` tags plus SLSA provenance
and SBOM attestations.

The sentinel digests shipped in `values.yaml` are placeholders; after a
real release, override them with the real digests recorded in the
release's `image-digests.json` artifact:

```bash
helm upgrade --install wasmsh deploy/helm/wasmsh \
  --set dispatcher.image.digest=sha256:...<from image-digests.json>... \
  --set runner.image.digest=sha256:...<from image-digests.json>...
```

A manually-dispatchable `Dev Images` workflow publishes
`dev-<short-sha>` tags for integration testing without cutting a
release.

## Validation

### Local (kind)

The full chart install path is exercised end-to-end by `e2e/kind/`:

```bash
just build-pyodide          # once per Pyodide version bump
just test-e2e-kind          # build images + cluster + tests + teardown
```

This boots a real kind cluster, loads locally built images, installs
the chart with `e2e/kind/values-e2e.yaml`, and runs the test suite in
`e2e/kind/tests/`. See `e2e/kind/README.md` for details.

### Reaching the dispatcher during a manual run

```bash
kubectl -n <ns> port-forward svc/<release>-dispatcher 8080:8080
curl -sf http://127.0.0.1:8080/healthz
curl -sf http://127.0.0.1:8080/readyz
```

Inside the cluster, clients should use the in-cluster DNS name directly:

```
http://<release>-dispatcher.<ns>.svc.cluster.local:8080
```

The chart ships no Ingress / LoadBalancer; put your own behind it if
external exposure is needed.

## Clients

The dispatcher speaks plain JSON/HTTP
([`docs/reference/dispatcher-api.md`](../../../docs/reference/dispatcher-api.md))
so any language can drive it. The repo also ships two first-party
LangChain Deep Agents sandbox backends that point at this chart:

```ts
// npm — @mayflowergmbh/langchain-wasmsh
import { WasmshRemoteSandbox } from "@mayflowergmbh/langchain-wasmsh";
const sandbox = await WasmshRemoteSandbox.create({
  dispatcherUrl: "http://wasmsh-dispatcher.wasmsh.svc.cluster.local:8080",
});
```

```python
# Python — langchain-wasmsh
from langchain_wasmsh import WasmshRemoteSandbox
sandbox = WasmshRemoteSandbox("http://wasmsh-dispatcher.wasmsh.svc.cluster.local:8080")
```

See [`docs/integrations/langchain-wasmsh.md`](../../../docs/integrations/langchain-wasmsh.md)
for the full API.
