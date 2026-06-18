# ADR-0032: Filesystem Change Notification via a Backend Change Log

## Status

Proposed

## Context

The `WorkerEvent::FsChanged(path)` event tells a host (the browser worker
embedding, the Pyodide host, or the scalable runner) that a VFS path changed
so it can re-read or re-render it. The protocol documentation
(`docs/reference/protocol.md`) promised this would also fire "when scripts
touch the FS".

In practice it did not. `FsChanged` was emitted from exactly one place —
`handle_write_file_command`, which backs the explicit `WriteFile` protocol
command. In-shell writes that go through the executor instead
(`echo > f`, `touch`, `mkdir`, `cp`, `rm`, `tee`, redirections) produced only
`[Exit]` even though the file was really written (read-back confirmed). The
`Run`/`StartRun`/`PollRun` path assembles its event batch from
`drain_io_events` / `drain_diagnostic_events` and never inspected filesystem
mutations.

The common factor is that every write — from the `WriteFile` command, from a
builtin, or from a utility — ultimately flows through the `Vfs` backend
(`MemoryFs` on the standalone/native path, `EmscriptenFs` on the Pyodide and
runner paths). That makes the FS backend the single natural chokepoint for
detecting changes, rather than instrumenting the many command/redirection
call sites in the runtime.

Two complications shaped the design:

1. **Owned write sinks.** Redirections and `tee` stream through
   `Box<dyn VfsWriteSink>` objects that are written to *outside* the FS
   object, so a naive "record inside `write_file`" hook would miss them.
2. **Internal scratch files.** The runtime stages heredocs/pipes and process
   substitution through reserved VFS paths (`/tmp/_wasmsh_*`,
   `/tmp/_proc_subst_*`). These are real VFS writes but must never surface to
   the host as user-visible changes.

Three approaches were considered:

1. **Instrument every runtime call site** that can mutate the FS. Rejected:
   many sites, fragile, easy to miss new ones (e.g. a future builtin).
2. **Diff the VFS before/after each run.** Rejected: O(filesystem) per run,
   and `EmscriptenFs` is backed by libc with no cheap enumeration hook.
3. **A change log owned by the FS backend, drained by the runtime.**
   Chosen — one chokepoint, works identically for both backends and every
   command shape.

## Decision

Add a shared, ordered, de-duplicating **change log** to the `wasmsh-fs` crate
and drain it into `FsChanged` events at the runtime's event-emission points.

### `FsChangeLog` (wasmsh-fs)

An `Rc<RefCell<…>>`-backed handle that is cheap to clone (clones share the same
underlying log):

- `record(path)` — appends a path, preserving first-seen order and skipping
  duplicates within the current drain window.
- `take()` — drains and returns the recorded paths in order, resetting the log
  so the next run starts empty.

A new defaulted trait method exposes it:

```rust
trait Vfs {
    // …
    fn change_log(&self) -> Option<&FsChangeLog> { None }
}
```

Backends that do not track changes (e.g. the `OpfsFs` stub) inherit the
`None` default and are unaffected.

### Recording in the backends

`MemoryFs` and `EmscriptenFs` each hold an `FsChangeLog` and record the
affected path on every mutating operation:

- write-intent `open` (`write | append | create | truncate`)
- `write_file`
- `open_write_sink` (covers redirections and `tee` — recorded at sink
  creation time, since that is when the file is created/truncated, so the
  owned sink need not carry the log handle)
- `create_dir`, `remove_file`, `remove_dir`

Recording at *open/sink-creation* time (not on each byte) is sufficient: a
write-intent open always creates or truncates, which is itself a change.
`install_stream_reader` is deliberately **not** recorded — its only uses are
internal streaming (process substitution, download piping); the real
destination write goes through `open_write_sink`/`write_file` and is recorded
there.

Read-only opens are never recorded, so `cat`, `ls`, and friends emit no
`FsChanged`.

### Draining in the runtime

A single `drain_fs_change_events` helper takes the backend log, **emits one
`FsChanged(path)` per distinct path**, and skips internal scratch paths
(`/tmp/_wasmsh_*`, `/tmp/_proc_subst_*`). It is called at the three batch
boundaries:

- both branches of `poll_active_run` (the terminal `Done` batch and each
  intermediate `Yield` batch, so long runs surface changes incrementally)
- `finish_idle_signal_exit`

`FsChanged` events are placed after I/O and diagnostic events and immediately
before the terminal `Exit`.

`handle_write_file_command` was collapsed onto the same helper: its
`open`+`write_file` already record the path, so it now drains through
`drain_fs_change_events` instead of pushing a hand-built `FsChanged`. The
`WriteFile` command and in-shell writes therefore share one emission
mechanism.

### Semantics

- **Per path, de-duplicated, ordered.** A file written several times in one
  run surfaces once; paths appear in first-touch order. The single-path
  `FsChanged(String)` event shape is unchanged.
- **Scope.** Only mutations that flow through the shell `Vfs` are reported.
  Files written by the in-process Python interpreter go through Emscripten's
  libc directly (not the `Vfs`) and are not reported via `FsChanged`.

## Consequences

- In-shell writes (`echo >`, `touch`, `mkdir`, `cp`, `rm`, `tee`,
  redirections) now notify the host, matching the long-standing protocol
  promise. This is purely additive to the event stream — existing hosts that
  already handle `FsChanged` from `WriteFile` need no changes.
- The behaviour is shared across all deployment shapes (standalone browser
  worker, Pyodide, scalable runner) because it lives in the shared
  `wasmsh-fs`/`wasmsh-runtime` core.
- This change is not expressible in the TOML differential-oracle suite (real
  bash emits no such event), so per ADR-0020 it is covered E2E-first:
  `wasmsh-fs` unit tests for the log, runtime protocol tests for the in-shell
  paths and scratch suppression, and E2E additions in
  `e2e/standalone/tests/file-ops.spec.ts` and
  `e2e/pyodide-node/tests/protocol-parity.test.mjs`.
- A write-intent `open` counts as a change even when the resulting file is
  empty (`> f` truncates). This matches filesystem reality.
- New mutating surface area must remember to flow through the `Vfs` backend
  (it already must, for sandboxing) to be reported. Internal scratch paths
  must stay within the reserved `/tmp/_wasmsh_*` / `/tmp/_proc_subst_*`
  namespaces to remain suppressed.

## References

- Protocol reference: [`docs/reference/protocol.md`](../reference/protocol.md)
  (ordering guarantees + `FsChanged` semantics)
- Related: ADR-0006 (capability-based VFS), ADR-0008 (worker runtime),
  ADR-0017 (shared runtime extraction), ADR-0018 (Pyodide same-module
  architecture), ADR-0020 (E2E-first testing policy).
