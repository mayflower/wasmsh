# Architecture

How wasmsh is structured and why.

## Pipeline Overview

```
source text
    → Lexer (wasmsh-lex)          stateful, multi-mode tokenizer
    → Parser (wasmsh-parse)       handwritten recursive descent
    → AST (wasmsh-ast)            structured word model, no premature stringification
    → HIR (wasmsh-hir)            normalized: Exec vs Assign vs RedirectOnly
    → Expand (wasmsh-expand)      parameter, arithmetic, tilde, brace, glob
    → Execution (wasmsh-browser)  dispatches builtins, utilities, functions
    → Output (wasmsh-protocol)    structured events back to host
```

## Crate Map

```
wasmsh-ast          AST types: Word, WordPart, Command, Redirection, etc.
wasmsh-lex          Stateful lexer with quoting modes
wasmsh-parse        Recursive-descent parser + word parser
wasmsh-expand       Expansion engine (params, arithmetic, glob, brace, tilde)
wasmsh-hir          High-level IR: normalizes AST into Exec/Assign/RedirectOnly
wasmsh-ir           Low-level IR instruction set (SetVar, PushArg, CallBuiltin)
wasmsh-vm           Cooperative VM with step budgets, cancellation, pipe buffers
wasmsh-state        Shell state: variables, scopes, positional params, cwd
wasmsh-builtins     18 builtins (echo, test, cd, export, trap, eval, ...)
wasmsh-utils        38 utilities (cat, grep, sed, sort, find, ...)
wasmsh-fs           VFS abstraction: MemoryFs, OpfsFs (stub), path normalization
wasmsh-browser      WorkerRuntime: end-to-end pipeline, command dispatch
wasmsh-protocol     Host↔Worker message types
wasmsh-testkit      TOML test runner, feature gates, oracle comparison
```

## Key Design Decisions

### No Parser Generators

The lexer and parser are handwritten. This gives full control over:
- Context-sensitive reserved word handling
- Here-document deferred body resolution
- Error recovery and diagnostic quality
- No GPL-licensed grammar files

See [ADR-0003](../adr/ADR-0003-handwritten-parser.md).

### Structured Word Model

Words are not flattened to strings during parsing. `WordPart` preserves:
- Literal text vs quoted regions
- Expansion boundaries (`$var`, `${...}`, `$(...)`, `$((...))`)
- Quoting context (single, double, ANSI-C)

This enables correct multi-phase expansion without re-parsing.

See [ADR-0004](../adr/ADR-0004-word-ast-and-expansion-phases.md).

### No OS Processes

In the browser, there are no processes, pipes, or signals. wasmsh models everything in-process:

- **Builtins** modify shell state directly
- **Utilities** operate on the VFS through the same API
- **Functions** share the parent scope (bash behavior)
- **Pipelines** use `PipeBuffer` — buffered data flow between stages
- **Subshells** get isolated variable scope via `push_scope/pop_scope`

See [ADR-0002](../adr/ADR-0002-browser-first-wasm-target.md).

### Cooperative Execution

The VM is cooperative, not preemptive:
- Step budget limits per execution
- Cancellation tokens checked at each step
- Output byte tracking
- No infinite loops in the browser's event loop

See [ADR-0009](../adr/ADR-0009-budgets-cancellation.md).

### Clean-Room Implementation

No code is copied from Bash, BusyBox, or their test suites. Compatibility is a behavioral goal achieved through:
- POSIX specification reading
- Black-box comparison against reference shells
- Original test cases

See [ADR-0001](../adr/ADR-0001-clean-room-boundary.md).

## Execution Flow

When `HostCommand::Run { input }` is received:

1. **Parse**: `wasmsh_parse::parse(input)` → `Program` AST
2. **Lower**: `wasmsh_hir::lower(&ast)` → `HirProgram`
3. **For each command**:
   a. Resolve command substitutions (`$(...)`) by recursive execution
   b. Expand words (parameter, arithmetic, tilde)
   c. Apply brace expansion
   d. Apply glob expansion against VFS
   e. Dispatch: builtin → direct call, utility → UtilContext, function → recursive execute
   f. Apply output redirections (capture stdout → write to VFS file)
4. **Collect events**: stdout, stderr, diagnostics → `Vec<WorkerEvent>`
5. **Fire traps**: EXIT trap if exit was requested
