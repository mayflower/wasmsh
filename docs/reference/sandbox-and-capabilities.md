# Sandbox and Capabilities

This page documents what the wasmsh sandbox prevents, what it allows, and
the knobs a host has for tuning that surface.

## Threat model

wasmsh is designed to safely execute **untrusted shell scripts** inside a
host application that is itself trusted. The host (a Node program, a
browser page, a Rust embedder) decides what the script can see; the
script cannot escape that decision through any documented surface.

In particular:

- The script has **no host filesystem access**. All file operations go
  through `BackendFs`, which is either an in-process `MemoryFs` or, in
  Pyodide builds, an `EmscriptenFs` shared with the in-sandbox Python
  interpreter. Neither backend touches the host's real filesystem.
- The script has **no network access** unless the host explicitly
  allowlists hostnames at `Init` time.
- The script has **no process model**. There is no `fork`, no `exec`, no
  POSIX signal delivery. `kill`, `wait`, `jobs` are not supported. Trap
  handlers exist for `EXIT`, `ERR`, `DEBUG`, and `RETURN`, but they run
  inside the shell runtime rather than via an OS process model.
- The script **cannot run arbitrary native code**. Builtins, utilities,
  and external command handlers are the only ways to enter native code,
  and all of them are registered statically by the host.
- The script's **execution time is bounded** by a step budget that the
  host configures.
- All system-information commands (`id`, `whoami`, `hostname`, `uname`)
  return deterministic virtual values. The script cannot fingerprint the
  host.

The sandbox is **not** designed to protect the host from:

- **Side channels in the host's network** when `allowed_hosts` is
  populated. If you allow `*.example.com`, a malicious script can
  exfiltrate via DNS or via the request shape. Allowlist carefully.
- **Resource exhaustion in the embedder.** A very large `step_budget`
  combined with a memory-hungry script can OOM the wasm module.
- **Bugs in builtins or utilities** that mishandle input. We treat any
  panic in this code as a bug.

## Step budget

The VM is cooperative. Every IR instruction increments a counter; when it
exceeds `step_budget`, execution stops with an `Exit` event and a
diagnostic. This bounds the wall-clock time of any single `Run` call by a
host-controllable factor.

Set `step_budget` via `HostCommand::Init`:

```rust
rt.handle_command(HostCommand::Init {
    step_budget: 100_000,
    allowed_hosts: vec![],
});
```

| Value     | Meaning |
|-----------|---------|
| `0`       | Unlimited (no budget enforced). Use only for trusted input. |
| `1_000`   | Tiny scripts only — useful for syntax-check style use. |
| `100_000` | Reasonable default for most short interactive commands. |
| `10_000_000` | Long-running scripts; expect noticeable wall time. |

The exact wall time per step depends on the host CPU and the instruction
mix, but as a rule of thumb 100k steps complete in well under a second on
modern hardware.

When the budget is hit the runtime emits a `Diagnostic(Warning, …)` and
an `Exit` event with a non-zero code. The next `Run` call starts fresh
(the budget is per-call, not per-session).

## Output limits

Independently of step budget, the runtime tracks total output bytes
produced by a single `Run` call. When that approaches a sandbox-friendly
ceiling, a diagnostic is emitted; if it crosses the hard limit, output is
truncated and the command exits.

This protects the host from `yes | head` style scripts that would
otherwise pin a buffer on the wire indefinitely.

## Network allowlist

wasmsh ships two networking utilities: `curl` and `wget`. Both are gated
by `allowed_hosts`. The mechanism is:

1. The host provides a list of patterns at `Init`.
2. `curl`/`wget` check the requested URL against the allowlist before any
   network call is made.
3. Denied requests fail with an error and a diagnostic; they never touch
   the network.

### Pattern syntax

| Pattern                  | Matches                                     |
|--------------------------|---------------------------------------------|
| `api.example.com`        | Exactly that host on any port               |
| `*.example.com`          | Any subdomain (but not `example.com` itself) |
| `192.168.1.100`          | That IP exactly                             |
| `api.example.com:8080`   | That host on that specific port             |

An empty list disables network access entirely. There is no wildcard
"allow everything"; if you want to permit any host you must list each
one. This is intentional — it makes review of the host configuration
mechanical.

See [ADR-0021](../adr/adr-0021-network-capability.md) for the design
rationale.

## Virtual system commands

The following commands return fixed values, regardless of the host:

| Command       | Output |
|---------------|--------|
| `whoami`      | `user` |
| `id`          | `uid=1000(user) gid=1000(user) groups=1000(user)` |
| `hostname`    | `wasmsh` |
| `uname`       | `wasmsh` |
| `uname -m`    | `wasm32` |
| `uname -a`    | `wasmsh wasmsh 0.1.0 wasm32 wasmsh` |

This is a deliberate design choice (see
[Design decisions: Virtual system commands](../explanation/design-decisions.md#virtual-system-commands)).
Reproducible output for tests and no host fingerprinting.

## Deterministic clock

`date` returns a fixed value (`2026-01-01 00:00:00 UTC`) by default. This
is also for reproducibility: the same script run twice produces the same
output.

To override the value for a particular session, set `WASMSH_DATE`:

```sh
WASMSH_DATE="2024-06-15 12:00:00 UTC" date
```

`WASMSH_DATE` is read from the shell environment, so you can set it once
in a script preamble, in the host's `initialFiles`, or per-command.

## Recognised environment variables

| Variable        | Read by    | Effect |
|-----------------|------------|--------|
| `WASMSH_DATE`   | `date`     | Override the deterministic clock value. |
| `HOME`          | `cd`, tilde expansion | Default working directory and `~` target. Defaults to `/home/user` if unset. |
| `PWD` / `OLDPWD`| `cd`       | Maintained by `cd`; readable by scripts. |
| `IFS`           | word splitting | Field separator characters. |
| `PATH`          | `command -v`, source resolution | Search path for commands and `source` lookups. |
| `BASH_REMATCH`  | `[[ … =~ … ]]` | Regex capture groups. |
| `RANDOM`        | dynamic    | 16-bit value from an internal XorShift PRNG; writable to reseed. |
| `LINENO`        | dynamic    | Current source line. |
| `SECONDS`       | dynamic    | Seconds since shell init; writable to reset the origin. |
| `FUNCNAME`      | dynamic    | Current function name (within a function call). |
| `BASH_SOURCE`   | dynamic    | Current source file (within a `source`d file). |
| `PIPESTATUS`    | dynamic    | Indexed array of exit codes from the most recent pipeline. |
| `REPLY`         | `read`     | Default destination for `read` without an explicit variable name. |
| `MAPFILE`       | `mapfile`  | Default destination for `mapfile`/`readarray`. |
| `OPTIND`        | `getopts`  | Next index to process. |

Setting `RANDOM` reseeds the PRNG; setting `SECONDS` resets the origin
the dynamic value is computed from.

## What the sandbox does *not* enforce

These items are out of scope for the sandbox itself; the host is
responsible for them:

- **Memory pressure.** A script can allocate a multi-megabyte string in
  the wasm heap. Cap your wasm module memory at the host level.
- **Wall-clock total runtime.** `step_budget` bounds steps, not wall
  time. A spinning Pyodide-side computation in a `python` external
  command is not counted against `step_budget`. Set host-level timeouts
  if needed.
- **Concurrent runtime instances.** Each `WorkerRuntime` is independent;
  if you spawn many you must manage them yourself.
- **Persistence.** State is in-memory and reset by `Init`. Use
  `WriteFile` and `ReadFile` to snapshot if you need persistence.

## See Also

- [Worker protocol reference](protocol.md) for the `Init` command and
  diagnostic events.
- [ADR-0009: Budgets and cancellation](../adr/ADR-0009-budgets-cancellation.md)
- [ADR-0021: Network capability](../adr/adr-0021-network-capability.md)
- [Design decisions: Cooperative VM](../explanation/design-decisions.md#cooperative-vm-with-step-budgets)
