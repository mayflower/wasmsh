# kind end-to-end tests — scalable dispatcher + runner

This suite boots a real Kubernetes cluster via [kind](https://kind.sigs.k8s.io),
loads locally built dispatcher and runner images, installs the
`deploy/helm/wasmsh` chart with test-friendly overrides, and exercises the
scalable path through the public dispatcher API.

It is the counterpart to the single-process tests under
`e2e/runner-node/` and `e2e/pyodide-node/`; those cover the runner in
isolation while this suite verifies the K8s-level behaviour — service
discovery, readiness gating, rolling restarts and scaling.

## Prerequisites

| Tool    | Reason                             | Install                 |
|---------|------------------------------------|-------------------------|
| docker  | build + load images                | docker.com / Docker Desktop |
| kind    | cluster lifecycle                  | `brew install kind`     |
| kubectl | talk to the cluster                | `brew install kubectl`  |
| helm    | install the chart                  | `brew install helm`     |
| node ≥ 22 | run the orchestrator + tests     | project default         |

The runner image also needs the Pyodide asset bundle. Run `just build-pyodide`
(requires emcc) at least once before invoking the E2E suite.

## Run the suite

```bash
just build-pyodide        # once per Pyodide version bump
just test-e2e-kind        # build images + cluster + tests + teardown
```

Useful variants while iterating:

```bash
just test-e2e-kind-keep   # leave the cluster up after the run
just test-e2e-kind-reuse  # reuse an existing cluster (skip cluster rebuild)
just kind-down            # destroy the cluster manually
```

Under the hood `just test-e2e-kind` calls `node e2e/kind/scripts/run.mjs`.
Flags passed there:

- `--keep` — never tear the cluster down
- `--keep-on-failure` — keep only if any test fails
- `--reuse` — assume a cluster named `wasmsh-e2e` already exists
- `--skip-build` — do not rebuild an image if it already exists locally
- `--tests <substring>` — restrict the run to matching test files

## What the tests cover

- `tests/cluster-health.test.mjs` — `/healthz`, `/readyz` plumbing plus
  deployment + pod readiness assertions via the Kubernetes API.
- `tests/session-lifecycle.test.mjs` — end-to-end session create → `run` →
  `write-file` → `read-file` → close through the dispatcher, plus a
  concurrency check that two sessions can be created in parallel.
- `tests/runner-resilience.test.mjs` — scaling the runner deployment up and
  back down, and verifying the dispatcher still answers requests after a
  runner pod is deleted out-of-band.
- `tests/langchain-wasmsh-sandbox.test.mjs` — exercises the
  `@mayflowergmbh/langchain-wasmsh` **`WasmshRemoteSandbox`** client
  (the LangChain Deep Agents adapter) against the live dispatcher:
  `execute`, binary `uploadFiles`/`downloadFiles` round-trip, non-zero
  exit propagation, and `initialFiles` seeding.

After the Node suites complete, the orchestrator also runs the
`@mayflowergmbh/langchain-wasmsh` **Python** adapter's full
`SandboxIntegrationTests` (`test_remote_integration.py`) through the
same port-forward — `uv` must be on PATH, otherwise the Python step is
skipped cleanly.

Pyodide snapshot restore is slow on cold pods (30–60 s on a laptop); the
tests apply generous per-request timeouts accordingly.

## Layout

```
e2e/kind/
├── kind-config.yaml      # kind cluster config (single control-plane)
├── values-e2e.yaml       # helm overrides for the test run
├── lib/                  # orchestration helpers (kubectl, helm, kind, docker,
│                         # port-forward, dispatcher HTTP client)
├── scripts/run.mjs       # top-level orchestrator
└── tests/                # node:test suites run inside the orchestrator
```
