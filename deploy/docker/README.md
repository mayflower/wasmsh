# wasmsh scalable stack — Docker Compose

Run the dispatcher + runner pool without Kubernetes. Clients connect to
the dispatcher's HTTP control plane via
[`WasmshRemoteSandbox`](../../packages/npm/langchain-wasmsh) or any
other JSON/HTTP client. Dispatcher routes sessions across the runner
pool with session affinity + restore-capacity-aware scheduling, same
as the Helm chart.

For Kubernetes production use the
[Helm chart](../helm/wasmsh/README.md) — the HPA on
`wasmsh_inflight_restores` and the NetworkPolicy give you capabilities
that compose cannot. Use compose for single-host deployments,
short-lived tenants, development, and self-hosted setups without k8s.

## Quickstart

```bash
# from the repo root
docker compose -f deploy/docker/compose.yml up -d --wait
curl -fsS http://127.0.0.1:8080/readyz
# use it — e.g.
python -c "from langchain_wasmsh import WasmshRemoteSandbox; \
  sb = WasmshRemoteSandbox('http://127.0.0.1:8080'); \
  print(sb.execute('echo hello').output)"
docker compose -f deploy/docker/compose.yml down
```

The dispatcher binds to `127.0.0.1:8080` by default — safe for
localhost-only use. For anything else, put an auth layer in front
(see [Reverse proxy + auth](#reverse-proxy--auth)).

## Scale the runner pool

The dispatcher resolves the `runner` service name through Docker's
embedded DNS and expands every A record into a routable runner, so
`--scale runner=N` just works — no extra config:

```bash
docker compose -f deploy/docker/compose.yml up -d --wait --scale runner=4
```

Capacity guidance: each runner exposes `WASMSH_RESTORE_SLOTS` (default
4) concurrent session-create slots. The dispatcher picks the runner
with the most free slots on each `POST /sessions`. See
[`docs/guides/performance-testing.md`](../../docs/guides/performance-testing.md)
for the sizing table.

## Configuration

Every tunable has a compose-baked default. Override by copying
[`.env.example`](.env.example) to `.env` in this directory and editing:

| Variable | Default | Purpose |
|-|-|-|
| `WASMSH_IMAGE_TAG` | `latest` | Tag for both dispatcher + runner images |
| `WASMSH_DISPATCHER_BIND` | `127.0.0.1:8080` | `host:port` the dispatcher publishes on |
| `RUST_LOG` | `info` | Dispatcher log level (`debug`/`trace` for troubleshooting) |
| `WASMSH_RESTORE_SLOTS` | `4` | Concurrent session starts per runner |
| `WASMSH_STARTUP_WARM_RESTORES` | `2` | Pre-warmed restore slots at boot |
| `WASMSH_WORKER_MAX_OLD_GENERATION_MB` | `48` | V8 old-gen heap cap per session |
| `WASMSH_WORKER_MAX_YOUNG_GENERATION_MB` | `8` | V8 young-gen heap cap |
| `WASMSH_FETCH_BROKER_REQUEST_BYTES` | `65536` | Max bytes the sandbox can send via curl/wget |
| `WASMSH_FETCH_BROKER_RESPONSE_BYTES` | `1048576` | Max bytes the sandbox can receive |
| `WASMSH_RUNNER_MEMORY_LIMIT` | `2G` | docker `deploy.resources.limits.memory` |
| `WASMSH_RUNNER_CPU_LIMIT` | `2` | docker `deploy.resources.limits.cpus` |
| `WASMSH_DISPATCHER_MEMORY_LIMIT` | `512M` | idem for dispatcher |

Full list in [`.env.example`](.env.example).

## Pin by digest for production

`:latest` moves on every merge to main. For immutable rollouts, pin by
digest from the `image-digests.json` attached to the relevant GitHub
Release:

```yaml
# edit compose.yml
services:
  runner:
    image: ghcr.io/mayflower/wasmsh-runner@sha256:<digest-from-release>
  dispatcher:
    image: ghcr.io/mayflower/wasmsh-dispatcher@sha256:<digest-from-release>
```

Alternatively pin by tag for reversible rollouts:

```
WASMSH_IMAGE_TAG=0.6.0
```

## Reverse proxy + auth

The dispatcher has no authentication surface. Anyone who can reach the
HTTP port can create sessions and execute sandboxed code. Never bind it
to anything beyond loopback without a gate in front.

The repo ships a ready-made Caddy overlay with TLS + HTTP basic auth:

```bash
# 1. generate a bcrypt hash for your password
docker run --rm caddy:2 caddy hash-password --plaintext 'your-strong-password'

# 2. fill out .env:
#      WASMSH_BASIC_AUTH_USER=alice
#      WASMSH_BASIC_AUTH_HASH='$2a$14$...'        # single-quote!
#      WASMSH_PUBLIC_DOMAIN=wasmsh.example.com    # blank -> internal CA

# 3. stack up with the overlay
docker compose \
  -f deploy/docker/compose.yml \
  -f deploy/docker/compose.caddy.yml \
  up -d --wait
```

With a real domain name Caddy auto-provisions a Let's Encrypt cert.
With `localhost` (the default) Caddy uses its internal CA — trust its
root if you want curl/browsers to accept the chain, or hit the
dispatcher directly on `127.0.0.1:8080` for local testing.

Swap Caddy for Traefik/nginx/your gateway of choice by editing
[`compose.caddy.yml`](compose.caddy.yml) and
[`Caddyfile`](Caddyfile). The dispatcher stays reachable on the
compose network as `dispatcher:8080` from any service you add.

## Relation to other compose files in this directory

| File | Purpose |
|-|-|
| [`compose.yml`](compose.yml) | Production-oriented stack — restart policies, resource limits, dispatcher `/readyz` healthcheck, loopback-only binding. This is the file to run. |
| [`compose.caddy.yml`](compose.caddy.yml) | Overlay adding Caddy for TLS + basic auth. Layer with `-f compose.yml -f compose.caddy.yml`. |
| [`compose.dispatcher-test.yml`](compose.dispatcher-test.yml) | Test harness used by `just test-e2e-dispatcher-compose`. Uses a bumped worker heap (256 MiB) to fit the langchain-tests standard suite's 10 MiB uploads — **not** what you want for real deployments. |

## Teardown

```bash
docker compose -f deploy/docker/compose.yml down          # keeps volumes
docker compose -f deploy/docker/compose.yml down -v       # also drops caddy cert volume
```

## Troubleshooting

- **`/readyz` returns 503** — dispatcher started, no runner has free
  restore capacity yet. Wait ~30 s for Pyodide boot, then retry. If it
  persists, check runner logs: `docker compose logs runner`.
- **`up --wait` times out** — usually runner cold-start (Pyodide boot +
  warm-restore fill) is slower than the 60 s `start_period`. Check
  `docker compose logs runner`; raise
  `WASMSH_STARTUP_WARM_RESTORES=0` to skip pre-warm if you want faster
  startup at the cost of first-request latency.
- **`cannot resolve runner`** — compose puts services on the same
  network by default; this only breaks if you've split them. Check
  `docker compose config`.
- **Per-session RAM higher than the sizing guide** — sessions that
  load big Python packages (pandas, numpy, PIL) allocate in the wasm
  linear memory, not the V8 heap, so `WASMSH_WORKER_MAX_OLD_GENERATION_MB`
  does not cap it. Plan 3–5× the stock 80 MiB figure.

See [`docs/how-to/runner-runbook.md`](../../docs/how-to/runner-runbook.md)
for the full operations cookbook (shared between compose and Helm).
