# ADR-0033: Lazy Copy-on-Write VFS via an Overlay Backend

## Status

Accepted

## Scope

This ADR is implemented for the **standalone web build only** (`wasm32-unknown-unknown`
via `wasmsh-browser`, and the native test target — both use `MemoryFs`). The
Emscripten/Pyodide overlay and the scalable snapshot/fork integration described
as motivations below are **deferred** and explicitly out of scope for this cut:

- The Emscripten target keeps `EmscriptenFs` and the existing `Mount` warning
  stub untouched; no overlay is compiled there.
- No changes to `tools/runner-node`, `wasmsh-dispatcher`, or the kind/compose
  E2E suites.

The cheap snapshot/fork payoff remains the longer-term motivation but is not
delivered here.

## Context

The VFS today is **eager**. The default backend, `MemoryFs`
(`crates/wasmsh-fs/src/memfs.rs`), stores every file as an `Arc<[u8]>` in a flat
`HashMap<String, FsNode>` keyed by normalized absolute path. A session only has
the files that were explicitly pushed in — through `HostCommand::WriteFile`,
through the JS/Python adapters' `initialFiles` loop, or through shell commands.
There is no notion of a read-only base that a session reads *through*, and no way
to defer materializing a file until it is touched.

This is fine for small seed trees but is a poor fit for two things we already
want to do:

1. **Snapshot/fork.** The scalable runner restores sessions from a template.
   Today a fork would have to copy the whole tree; the dispatcher's value
   proposition is cheap session creation (~6 ms restore), and a deep VFS copy
   per fork works against that.
2. **Mounting a shared base.** ADR-0006 named `OverlayFs` and the protocol
   reserves `HostCommand::Mount`, but both are stubs — `Mount` currently just
   emits a `mount not yet implemented` warning
   (`crates/wasmsh-runtime/src/lib.rs`). A read-only base bundle (a project
   skeleton, a fixture set, a remote bundle) shared across many sessions has no
   home.

The whole runtime, every utility, and every builtin reach the filesystem only
through the `Vfs` trait (`crates/wasmsh-fs/src/lib.rs`), and the backend is
chosen by the compile-time `BackendFs` type alias. That means a new layering
backend can be introduced **without touching any call site** — the same property
that ADR-0032 relied on for the change log.

Two facts make copy-on-write cheap to build here rather than expensive:

- File content is already `Arc<[u8]>`, so reads can be handed out of a base with
  zero copy and shared until a write forces materialization.
- `MemoryFs` already owns quota accounting, the handle table, virtual readers,
  and the `FsChangeLog`. An overlay can **reuse `MemoryFs` as its writable upper
  layer** instead of reimplementing storage.

What is genuinely new is the bookkeeping a union filesystem needs: three-way
resolution (upper / deleted / base), directory-listing merges, and *whiteouts*
to represent "deleted in the overlay but still present in the base".

## Decision

Add an `OverlayFs` backend to `wasmsh-fs` that composes a **lazy, read-only
base** with a **writable upper layer**, materializing a path into the upper layer
only on first write (copy-on-write).

### `LazyBase` — the read-only, fetch-on-demand source

```rust
pub trait LazyBase {
    fn stat(&self, path: &str) -> Result<Metadata, FsError>;
    fn read(&self, path: &str) -> Result<Arc<[u8]>, FsError>;
    fn read_dir(&self, path: &str) -> Result<Vec<DirEntry>, FsError>;
}
```

This is where "lazy" lives. A base is free to hold only an index (path → offset
into a compressed bundle, a remote manifest, an Emscripten/libc delegate) and
fetch + decode bytes on first `read`. A base **may** keep an internal
read-through cache (`RefCell<HashMap<String, Arc<[u8]>>>`) so repeated reads of an
un-mutated file do not re-fetch, but the cache is an implementation detail of the
base, not part of the contract. The base is never mutated.

### `OverlayFs` — the union backend

```rust
pub struct OverlayFs<B: LazyBase> {
    upper: MemoryFs,            // writable COW layer (reuses existing storage)
    base: B,                    // read-only, lazy
    state: Rc<RefCell<OverlayState>>,
}

struct OverlayState {
    whiteouts: HashSet<String>, // paths deleted in upper but present in base
    handles: HashMap<u64, OverlayHandleKind>, // overlay owns the handle namespace
    next_handle: u64,
}

enum OverlayHandleKind {
    Upper(FileHandle),   // delegates to the upper MemoryFs handle
    BaseRead(Arc<[u8]>), // read-only snapshot of base content, zero-copy
}
```

`OverlayFs` owns its own file-handle namespace so a read-only open of a base
file can return a handle backed by the base `Arc<[u8]>` without materializing it
into the upper layer. The change log is the **upper layer's** log
(`change_log()` forwards to `upper.change_log()`); base-only mutations (a
whiteout for a removed base entry) record onto that same log so ADR-0032's
`FsChanged` emission is unaffected. Resolution for any path is three-way:

1. if present in `upper` → use upper;
2. else if in `whiteouts` → `NotFound`;
3. else → fall through to `base` (lazy fetch).

Per-operation semantics:

- **Reads** (`open(read)`, `read_file`, `stream_file`, `stat`) resolve
  three-way. A base hit hands back the base's `Arc<[u8]>` with no copy.
- **Writes are the COW trigger.** `open(write|append|create|truncate)`,
  `open_write_sink`, and `write_file` first *materialize* the path: if it exists
  only in the base and the open is not a full truncate, the base bytes are read
  once and written into `upper`; the operation then proceeds entirely against
  `upper`. A truncating/creating open skips the read. Any whiteout on the path is
  cleared.
- **`create_dir`** writes into `upper` and clears any whiteout.
- **`remove_file` / `remove_dir`** remove from `upper` if present; if the entry
  also exists in the base, a whiteout is recorded (the base cannot be mutated).
- **`read_dir`** unions base entries and upper entries, subtracts whiteouts, and
  dedupes by name with **upper winning** on type. `MemoryFs::read_dir` is already
  an O(all-nodes) prefix scan, so the merge does not change the asymptotic cost.
- **`change_log`** forwards the overlay's log; every mutation `record()`s on it,
  using the same shared-`Rc` pattern as `MemoryFs`/`EmscriptenFs`. This keeps
  ADR-0032's `FsChanged` emission working unchanged.
- **`install_stream_reader` / virtual readers** stay **upper-only**. Their uses
  are ephemeral (`<(cmd)` process substitution, download piping) and do not
  belong in a shared base.

### Integration: the `BackendFs` lever

Rather than make `OverlayFs` opt-in, the non-emscripten `BackendFs` type alias is
**redefined** from `MemoryFs` to `OverlayFs<InMemoryBase>`:

```rust
// non-emscripten (web + native test targets)
pub type BackendFs = OverlayFs<InMemoryBase>;
// emscripten target: unchanged
pub type BackendFs = EmscriptenFs;
```

`OverlayFs` constructed with an **empty base** behaves identically to its
`MemoryFs` upper layer (the *transparency* property). That means
`wasmsh-utils` (`UtilContext { fs: &mut BackendFs }`), `wasmsh-builtins`, and
`wasmsh-runtime` all compile unchanged, and the **entire existing test suite
passing is the parity guarantee**. A base is installed later via `Mount`.

`HostCommand::Mount` becomes the front door: it carries an in-memory base file
list and installs it at the root (single-root overlay). The runtime handler is
target-gated — on the emscripten target it keeps returning the existing
`mount not yet implemented` warning, since `OverlayFs` is not the `BackendFs`
there.

The **Emscripten** backend is deferred (see Scope). When an overlay over
Emscripten is eventually wanted, the base would be a thin `LazyBase` that
delegates `read`/`stat`/`read_dir` to libc read-only, with the upper layer still
a Rust `MemoryFs`; we would not reach into Emscripten/MEMFS internals.

### `Clone` and isolation

`MemoryFs::clone` is intentionally shallow (it aliases the same
`Rc<RefCell<…>>`), and `clone_for_isolated_process_subst` relies on that so an
isolated subshell shares the parent store. To preserve that exact behavior,
`OverlayFs::clone` is also **shallow**: the upper `MemoryFs`, the base (`Rc`-backed),
and the overlay `state` (whiteouts + handle table, in `Rc<RefCell<…>>`) are all
shared by a clone. The truly-isolated fork variant (share base, fresh empty
upper) remains a future option for the snapshot/fork work and is intentionally
**not** adopted now, to avoid changing process-substitution semantics.

### Out of scope (for the first cut)

- A real bundle/remote `LazyBase` implementation. This cut ships only
  `InMemoryBase` (a path → bytes map handed in via `Mount`) to prove the COW
  semantics. A compressed-bundle or remote base follows later.
- `rename` remains absent from the `Vfs` trait; `mv` stays copy+delete, which
  composes correctly over the overlay (materialize source, write dest, whiteout
  source) and is covered by a test.

## Consequences

- Reads from a base are zero-copy and lazy; bytes are only materialized — and
  only then counted against `MemoryFs` quotas — when a path is first written.
- Snapshot/fork can share an immutable base and carry only the per-session upper
  delta, which is the cheap-fork story the scalable path wants.
- `OverlayFs` reuses `MemoryFs` for storage, quotas, handles, virtual readers,
  and the change log, so the new surface is the union bookkeeping
  (`read_dir`/`stat` merge + whiteouts), not a second storage engine.
- The two places to spend design care are the `read_dir` merge with whiteouts and
  the `Clone`/isolation decision; both are exercised directly by tests.
- Because the COW/mount behavior is not expressible against a real-bash oracle
  (bash has no overlay), the *observable* equivalence is covered two ways: the
  whole existing TOML suite runs on the now-overlay-backed `BackendFs` (empty
  base = transparency), and `wasmsh-fs` unit tests cover `OverlayFs`/`LazyBase`
  internals. Per ADR-0020, the mount capability is exposed E2E-first via a
  standalone Playwright test in `e2e/standalone/`.
- The laziness contract — base read only on first touch, never for untouched
  paths, never again after materialization, and short-circuited by whiteouts —
  is pinned by the `CountingBase` test double in `crates/wasmsh-fs/src/overlay.rs`,
  which records every base `stat`/`read`/`read_dir` and asserts the access
  pattern. A future disk- or bundle-backed `LazyBase` can reuse the same
  access-log technique in a native integration test.
- ADR-0006 is updated: `OverlayFs` is no longer purely aspirational.

## References

- ADR-0006 (capability-based VFS — names `MemoryFs`/`OverlayFs`/`OpfsFs`)
- ADR-0009 (budgets and cancellation — quota accounting lives in the upper layer)
- ADR-0017 (shared runtime extraction — `Vfs`/`BackendFs` is the shared seam)
- ADR-0018 (Pyodide same-module architecture — why Emscripten stays opaque)
- ADR-0020 (E2E-first testing policy)
- ADR-0032 (FS change notification — `change_log` forwarding contract)
- Protocol reference: [`docs/reference/protocol.md`](../reference/protocol.md)
  (`HostCommand::Mount`, `FsChanged`)
