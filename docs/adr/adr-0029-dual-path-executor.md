# ADR-0029: Dual-Path Executor (Runtime Interpreter + VM Subset)

## Status

Accepted

## Context

ADR-0005 established `AST -> HIR -> IR -> VM` as the target execution
architecture. In practice, `wasmsh-runtime` still executes most shell
semantics by interpreting HIR directly. Prompt-driven executor work added
three concrete pieces that changed the runtime shape:

- a command-scoped fd / pre-exec I/O model
- a resumable progressive executor (`StartRun` / `PollRun`)
- a bounded HIR subset that lowers through `wasmsh-ir` into `wasmsh-vm`

Without an explicit record, repository docs drifted into describing the
target architecture as if it were already the universal execution path.

## Decision

The current executor architecture is explicitly **dual-path**:

1. All shell input parses to AST and lowers to HIR.
2. `wasmsh-runtime` is the primary executor and interprets HIR directly.
3. Before executing a top-level `and/or` list, the runtime may validate it
   against a supported subset.
4. When the shape is supported, the runtime lowers that subset through
   `wasmsh-ir` and executes it through `wasmsh-vm`.
5. When the shape is not supported, the runtime falls back to the direct
   HIR interpreter.

The current VM subset is intentionally narrow:

- scalar assignments
- builtin command execution with literal builtin names
- selected stdout file redirections
- top-level `&&` / `||` short-circuiting

Pipelines, aliases, env-prefix execution, complex word shapes, and the
rest of the shell grammar continue to run through the fallback path.

## Consequences

- Docs must describe the IR/VM path as a subset path, not the universal
  executor.
- Runtime, IR, and VM parity tests are required anywhere the subset path
  overlaps with fallback execution.
- The progressive protocol is defined in terms of the resumable runtime
  executor, regardless of whether a given command segment uses fallback or
  VM-subset execution internally.
- Future IR/VM expansion remains incremental: new HIR shapes may move into
  the subset only when parity tests land first.
