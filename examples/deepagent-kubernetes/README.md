# DeepAgents + wasmsh — Kubernetes Example

Run a Deep Agent against a wasmsh dispatcher + runner pool installed via the Helm chart. Same `WasmshRemoteSandbox` client code as the Docker Compose variant — only the deployment topology changes.

This directory deliberately contains no new code: the Python and TypeScript scripts in the sibling
[`deepagent-python/remote_basic.py`](../deepagent-python/remote_basic.py) and
[`deepagent-typescript/remote-basic.ts`](../deepagent-typescript/remote-basic.ts)
examples already do the right thing. They connect to whatever `WASMSH_DISPATCHER_URL` points at; a Kubernetes port-forward is just one way of producing that URL.

## 1. Install the chart

```bash
# production defaults: 3 runner replicas, HPA on wasmsh_inflight_restores,
# PDB, NetworkPolicy, tuned V8 heap
helm upgrade --install wasmsh deploy/helm/wasmsh \
  --namespace wasmsh --create-namespace \
  --wait
kubectl -n wasmsh get pods
```

The chart is the same one exercised on every PR by the kind E2E (`just test-e2e-kind`). See [`deploy/helm/wasmsh/README.md`](../../deploy/helm/wasmsh/README.md) for the values surface (replica counts, resource limits, autoscaling, monitoring).

## 2. Reach the dispatcher from your laptop

The dispatcher service is `ClusterIP` by default — internal-only. Pick one of:

**Option A: `kubectl port-forward` (simplest, ephemeral)**

```bash
kubectl -n wasmsh port-forward svc/wasmsh-dispatcher 8080:8080 &
export WASMSH_DISPATCHER_URL=http://127.0.0.1:8080
```

**Option B: `Ingress` + DNS + auth (production)**

Deploy your cluster's ingress controller, create an `Ingress` resource pointing at `wasmsh-dispatcher:8080`, terminate TLS at the ingress, and put an auth gate (OAuth2 proxy, OIDC gateway, mTLS — whatever matches your platform) in front. The wasmsh dispatcher has no auth surface of its own; never expose it on the public internet without one.

```bash
export WASMSH_DISPATCHER_URL=https://wasmsh.your-domain.example.com
```

**Option C: in-cluster client**

If the agent itself runs inside the cluster, the dispatcher is reachable as `http://wasmsh-dispatcher.wasmsh.svc.cluster.local:8080` — no port-forward or ingress needed.

## 3. Run the agent

```bash
# Python
cd examples/deepagent-python
pip install -r requirements.txt
python remote_basic.py

# TypeScript
cd examples/deepagent-typescript
npm install
npm run remote-basic
```

Both scripts do the same thing: create a `WasmshRemoteSandbox`, seed a file, run bash + python3, print the output, and close the session.

## What this deployment gives you over Docker Compose

- **Autoscaling** — runner pods scale on `wasmsh_inflight_restores` via HPA (requires prometheus-adapter or KEDA to expose the custom metric)
- **Graceful draining** — `SIGTERM` → runner stops accepting new sessions → dispatcher rebalances → in-flight work finishes within `terminationGracePeriodSeconds`
- **Pod disruption budgets** — rolling upgrades never take the last runner down
- **Egress policy** — runner pods only have DNS egress by default; override via `networkPolicy.extraEgress` for curl/wget targets
- **Observability hooks** — optional `ServiceMonitor` + `PrometheusRule` if the cluster runs Prometheus Operator

See [`docs/explanation/snapshot-runner.md`](../../docs/explanation/snapshot-runner.md) for the full architecture and [`docs/how-to/runner-runbook.md`](../../docs/how-to/runner-runbook.md) for operations.

## Teardown

```bash
helm uninstall wasmsh --namespace wasmsh
kubectl delete namespace wasmsh
```
