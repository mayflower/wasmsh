# docker-compose end-to-end tests — `WasmshRemoteSandbox`

Fast counterpart to the [`e2e/kind`](../kind) suite: builds the same
dispatcher + runner images, brings them up via
[`deploy/docker/compose.dispatcher-test.yml`](../../deploy/docker/compose.dispatcher-test.yml),
and exercises the LangChain Deep Agents adapter's `WasmshRemoteSandbox`
client (TypeScript) plus the Python adapter's `SandboxIntegrationTests`
suite against a localhost dispatcher.

No Kubernetes involved — use this loop while iterating on the sandbox
client or dispatcher HTTP contract, then run `just test-e2e-kind` for
the production-parity K8s variant before landing.

## Prerequisites

| Tool     | Reason                                     | Install                       |
|----------|--------------------------------------------|-------------------------------|
| docker   | build + run the dispatcher/runner stack    | docker.com / Docker Desktop   |
| node ≥ 22| run the orchestrator + Node test           | project default               |
| uv       | *(optional)* run the Python pytest step    | `brew install uv`             |

The runner image also needs the Pyodide asset bundle. Run
`just build-pyodide` (requires emcc) at least once, or stage the assets
from the published npm tarball as the CI workflow does.

## Run the suite

```bash
just build-pyodide                    # once per Pyodide version bump
just test-e2e-dispatcher-compose      # build images + compose + tests + teardown
```

Useful variants while iterating:

```bash
just test-e2e-dispatcher-compose-keep    # leave the stack up after the run
just test-e2e-dispatcher-compose-reuse   # skip image build, reuse locally-built images
```

Under the hood that calls `node e2e/dispatcher-compose/scripts/run.mjs`.
Flags:

- `--keep` — do not tear down the compose stack after the run
- `--skip-build` — reuse existing `wasmsh-{dispatcher,runner}:e2e` images
- `--no-python` — TS only (skip the pytest step)
- `--tests <substring>` — restrict to matching test files

## What the tests cover

- `tests/langchain-wasmsh-sandbox.test.mjs` — TypeScript
  `WasmshRemoteSandbox.create(...)` end-to-end: `execute`, binary
  `uploadFiles`/`downloadFiles` round-trip, non-zero exit propagation,
  `initialFiles` seeding.
- After the Node suite, the orchestrator runs the Python
  `langchain-wasmsh` adapter's full `SandboxIntegrationTests`
  (`packages/python/langchain-wasmsh/tests/integration_tests/test_remote_integration.py`)
  via `uv run pytest`. Skipped cleanly if `uv` isn't on PATH.

## Layout

```
e2e/dispatcher-compose/
├── scripts/run.mjs   # build + compose up + node test + pytest + teardown
└── tests/            # node:test suites against WASMSH_E2E_DISPATCHER_URL
```

The orchestrator reuses the [`e2e/kind`](../kind/lib) library helpers
(`runCommand`, `buildImages`, `createDocker`) so the two suites stay in
lock-step.
