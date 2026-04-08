# Design Decisions

This page collects the major design decisions in wasmsh and the rationale
behind each. Most decisions have a corresponding ADR with the long-form
discussion; the entries below are short summaries with links.

## Why a New Shell?

Existing shells (bash, zsh, ash) are designed for Unix systems with
processes, signals, filesystems, and TTYs. They cannot run in a browser.
Porting them would require emulating the entire POSIX process model.

wasmsh takes the opposite approach: build a shell runtime from scratch
that is native to the WebAssembly constraints. The result is smaller,
easier to control, and does not carry Unix baggage into the browser.

See [ADR-0002: Browser-first WASM target](../adr/ADR-0002-browser-first-wasm-target.md).

## Why Bash Syntax?

Bash is the de facto scripting language. Developers write bash scripts
without thinking about it. Supporting bash syntax lets wasmsh run the
scripts people already have — CI snippets, deployment scripts, data
processing, configuration management.

We do not aim for 100% bash compatibility. We aim for the subset that real
scripts actually use, and we treat divergence from bash as a bug worth
investigating. The supported surface is enumerated in
[`SUPPORTED.md`](../../SUPPORTED.md).

See [ADR-0010: Compatibility baseline](../adr/ADR-0010-compatibility-baseline.md).

## Why Not WebAssembly System Interface (WASI)?

WASI provides a POSIX-like system interface for WebAssembly. We considered
it and rejected it for the standalone target:

- WASI shells still need a process model (fork/exec) to run external commands.
- WASI filesystem access is host-dependent and not supported in browsers.
- WASI doesn't solve the cooperative execution problem — there's still no
  way to interrupt a runaway script in a browser without WebWorker tricks.
- Browser deployment of WASI is still experimental and inconsistent.

wasmsh targets `wasm32-unknown-unknown` directly (with its own VFS and
execution model) and `wasm32-unknown-emscripten` for the Pyodide build (so
it can share libc with CPython). Neither path involves WASI.

See [ADR-0002: Browser-first WASM target](../adr/ADR-0002-browser-first-wasm-target.md).

## Why a Handwritten Lexer and Parser?

The lexer is a multi-mode state machine. The parser is hand-rolled
recursive descent. We do not use a parser generator (yacc, lalrpop,
nom, pest, etc.) for several reasons:

- Bash needs context-sensitive reserved word handling. Generators handle
  this poorly.
- Here-document deferred body resolution does not fit a pure grammar.
- Error recovery and diagnostic quality matter, and generators tend to
  produce unhelpful error messages.
- No GPL-licensed grammar files end up in the build.

The downside is that the lexer and parser are larger than they would be
with a generator, and they require more discipline when adding new
constructs.

See [ADR-0003: Handwritten parser](../adr/ADR-0003-handwritten-parser.md).

## Why a Structured Word Model?

Words are not flattened to strings during parsing. `WordPart` preserves
literal text, expansion boundaries (`$var`, `${...}`, `$(...)`,
`$((...))`), and quoting context (single, double, ANSI-C). This enables
correct multi-phase expansion — every expansion phase needs to see the
original quoting to do the right thing — without re-parsing.

See [ADR-0004: Word AST and expansion phases](../adr/ADR-0004-word-ast-and-expansion-phases.md).

## Why HIR and IR (Two Intermediate Forms)?

Going directly from AST to execution would tangle three concerns:
classification (Exec vs Assign vs RedirectOnly), expansion ordering, and
control-flow lowering. We split it into two passes:

- **HIR** normalises the AST. Every command becomes one of a small set of
  shapes (`Exec`, `Assign`, `RedirectOnly`, `Function`, `If`, `While`, ...)
  with explicit redirection lists. This makes the dispatcher trivial.
- **IR** is a linear instruction stream with explicit jumps (`SetVar`,
  `PushArg`, `CallBuiltin`, `Jump`, `JumpIfFalse`, ...). This is what the
  cooperative VM steps through. Linear IR is much easier to budget,
  cancel, and trace than tree-walking the HIR.

The two-stage lowering also keeps each stage testable in isolation.

See [ADR-0005: HIR / IR / VM](../adr/ADR-0005-hir-ir-vm.md).

## Cooperative VM with Step Budgets

The VM is cooperative, not preemptive. Every IR instruction increments a
step counter; when it exceeds the budget the VM exits with a diagnostic.
A cancellation token is checked at the same boundary. This means:

- Runaway scripts cannot hang the host page or block the JS event loop.
- Cancellation is observable and predictable (it happens between
  instructions, not in the middle of one).
- A host can rate-limit by simply choosing a smaller budget.
- Output bytes are tracked separately, so a script that produces gigabytes
  of stdout can be cut off even if the step budget is generous.

The downside is that there is no way to interrupt a single very expensive
instruction (a giant glob expansion, say). In practice this has not been a
problem.

See [ADR-0009: Budgets and cancellation](../adr/ADR-0009-budgets-cancellation.md).

## Capability-Based VFS

There is no host filesystem. All file operations go through `BackendFs`,
which is a type alias resolved at compile time:

- `MemoryFs` for the standalone target — pure in-memory VFS, isolated to
  this runtime instance.
- `EmscriptenFs` for the Pyodide target — backed by libc, which routes
  through the Emscripten POSIX FS. The Python interpreter sees the same
  files because they call the same libc.
- `OpfsFs` is a feature-gated stub for browser persistence.

The runtime is *not* generic over the FS type; the alias swaps at the
crate boundary based on a Cargo feature. This avoids monomorphisation
explosions and keeps `WorkerRuntime` simple.

See [ADR-0006: Capability VFS and OPFS](../adr/ADR-0006-capability-vfs-and-opfs.md).

## Network Capability Model

Network access for `curl`/`wget` is opt-in. The host passes an
`allowed_hosts` list at `Init` time. Patterns support exact host
(`api.example.com`), wildcard (`*.example.com`), IP (`192.168.1.100`), and
host with port (`api.example.com:8080`). An empty list disables all
network access.

The model is allowlist-only by design. There is no denylist, no "off by
default but easy to enable", no implicit access to anything. A host that
forgets to populate `allowed_hosts` ends up with a sandbox that cannot
reach the network at all — which is the safe default.

See [ADR-0021: Network capability](../adr/adr-0021-network-capability.md).

## Pyodide Same-Module Architecture

The Pyodide integration links wasmsh into the *same* WebAssembly module
as CPython. We considered separate modules and rejected it:

- Sharing libc means the shell and Python see the same files, not copies.
  `python3 script.py` can read a file the shell just wrote without
  serialising bytes back and forth.
- A single module means a single `loadPyodide()` call. Hosts already pay
  the cost of loading Pyodide; tacking wasmsh onto that module is free.
- `python` commands dispatch through `ExternalCommandHandler` straight to
  `PyRun_SimpleString`. There is no IPC and no marshalling.

The downside is that wasmsh has to be built with `wasm32-unknown-emscripten`
and pinned to specific Pyodide and Emscripten versions
(`tools/pyodide/versions.env`).

See [ADR-0017: Shared runtime extraction](../adr/ADR-0017-shared-runtime-extraction.md)
and [ADR-0018: Pyodide same-module](../adr/ADR-0018-pyodide-same-module.md).

## Dual-Target Packaging

We ship two distinct artifacts from one source tree:

- A `wasm-pack` bundle for the standalone Web Worker target, plus the
  `wasmsh-runtime` Rust crate on crates.io.
- A custom Pyodide build with wasmsh linked in, packaged as
  `@mayflowergmbh/wasmsh-pyodide` (npm) and `wasmsh-pyodide-runtime`
  (PyPI). Both packages contain the same assets; the npm package adds a
  thin Node and browser host adapter.

The shared runtime makes this possible. Any feature added to
`wasmsh-runtime` is automatically available in both targets.

See [ADR-0019: Dual-target packaging](../adr/ADR-0019-dual-target-packaging.md).

## Function Scope Model

In bash, functions share the parent scope by default. Only `local`
creates isolation. This is different from most programming languages but
critical for bash compatibility:

```sh
COUNT=0
increment() { COUNT=$((COUNT + 1)); }
increment
echo $COUNT  # prints 1, not 0
```

wasmsh implements this faithfully. Function calls do not push a new
variable scope; `local` saves the old value and restores it when the
function returns.

## Pipeline Execution Model

Real shells fork processes for each pipeline stage and connect them with
OS pipes. wasmsh cannot fork. Instead:

1. Each stage runs to completion.
2. Its stdout is captured into a `PipeBuffer`.
3. The buffer is provided as stdin to the next stage.

This works correctly for all non-streaming commands and matches bash for
the vast majority of real scripts. It does not work for genuinely
streaming pipelines (`yes | head -n 1000000`) — wasmsh caps these at a
sandbox-friendly limit. A future version may add coroutine-based
interleaving for true streaming.

## Virtual System Commands

Commands like `id`, `whoami`, `uname`, and `hostname` return deterministic
virtual values. This is intentional:

- Sandboxed execution should not leak host information to scripts.
- Tests should be reproducible.
- The browser has no concept of "users" or "hostnames".

## Deterministic Date

`date` returns a fixed value (`2026-01-01 00:00:00 UTC`) by default. This
ensures reproducibility. Scripts that need a different date can set the
`$WASMSH_DATE` environment variable; see
[Sandbox and Capabilities](../reference/sandbox-and-capabilities.md).

## Clean-Room Implementation

No code is copied from Bash, BusyBox, GNU coreutils, or their test
suites. Compatibility is a behavioural goal, achieved through:

- POSIX specification reading.
- Black-box comparison against reference shells.
- Original test cases written from scratch.

We track which features come from observation rather than original
specification work via the test oracle setup (ADR-0011).

See [ADR-0001: Clean-room boundary](../adr/ADR-0001-clean-room-boundary.md)
and [ADR-0015: Clean-room hashes](../adr/ADR-0015-clean-room-hashes.md).

## E2E-First Testing for New Integrations

New integration capabilities — talking to a new host, exposing a new
protocol command, adding a new build target — must start with a failing
end-to-end test in `e2e/`. Unit tests do not catch integration regressions;
the cost of an unnoticed E2E break is much higher than the cost of writing
the test up front.

See [ADR-0020: E2E-first testing](../adr/ADR-0020-e2e-first-testing.md).

## What wasmsh Will Not Do

It is at least as useful to know what wasmsh deliberately rejects:

- **Real processes.** No `fork`, no `exec` of host binaries, no signals
  beyond `EXIT` and `ERR` traps.
- **Interactive TTY.** No line editing, no completion, no terminal control
  sequences. The shell is for running scripts, not for sitting at a
  prompt.
- **Coreutils-completeness.** wasmsh ships the 88 utilities that real
  scripts actually use. It will not chase every GNU extension.
- **Bash compatibility for its own sake.** If a feature is rare in real
  scripts and expensive to implement, we leave it out and document the
  gap in `SUPPORTED.md`.
- **Loadable modules.** No `enable -f`, no plug-in architecture for
  arbitrary native code in the runtime. New commands go in
  `wasmsh-builtins`, `wasmsh-utils`, or via `ExternalCommandHandler` from
  the host.

These boundaries keep the runtime small, predictable, and auditable.
