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
wasmsh-state        Shell state: variables (scalar/array/assoc), scopes, positional params, cwd
wasmsh-builtins     22 builtins (echo, test, cd, export, trap, eval, printf, read, ...)
wasmsh-utils        43 utilities (cat, grep, sed, sort, find, md5sum, base64, ...)
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

### Arrays and Variable Types

Variables support three value types via `VarValue` enum:
- `Scalar(SmolStr)` — default string values
- `IndexedArray(IndexMap<usize, SmolStr>)` — `arr=(a b c)`, sparse indexing
- `AssocArray(IndexMap<SmolStr, SmolStr>)` — `declare -A map`

Variable attributes: `exported`, `readonly`, `integer` (auto-arithmetic), `nameref` (indirect reference).

### Full Arithmetic Engine

The arithmetic evaluator (`eval_arithmetic`) is a proper recursive-descent parser supporting all bash arithmetic operators with correct precedence: assignment, ternary, logical, bitwise, comparison, shift, arithmetic, exponentiation, unary, and postfix. Supports hex (`0xFF`), octal (`077`), binary (`0b1010`), and arbitrary base (`N#digits`) literals.

### Extended Test `[[ ]]`

`[[ ]]` is a full compound command with its own evaluation engine:
- Glob pattern matching on `==`/`!=` RHS
- Regex matching with `=~` and `BASH_REMATCH` capture groups
- String ordering with `<`/`>`
- No word splitting or globbing on variables
- `&&`/`||`/`!` logical operators with grouping

### Extended Globbing

When `extglob` is enabled (default): `?(pat)`, `*(pat)`, `+(pat)`, `@(pat)`, `!(pat)`. Implemented via recursive backtracking matcher. `globstar` enables `**` recursive directory traversal.

### Dynamic Variables

Several variables are evaluated on read rather than storing a fixed value in `ShellState`:
- `$RANDOM` — 16-bit value from an XorShift PRNG seeded at shell init; writable to reseed the generator
- `$LINENO` — current source line, tracked by the VM from span information
- `$SECONDS` — elapsed seconds since shell initialisation; writable to reset the origin
- `$PIPESTATUS` — indexed array of exit codes from the most recent pipeline
- `$FUNCNAME` / `$BASH_SOURCE` — call-stack arrays updated by the function call frame machinery

### Alias Expansion

Alias lookup and recursive expansion occurs in `wasmsh-browser`'s dispatch layer after
tokenisation but before builtin/utility/function resolution. The `alias` and `unalias`
builtins manage the alias table stored in `ShellState`. Alias expansion is suppressed
for commands that are themselves alias definitions and for quoted command names, matching
bash behaviour.

## Execution Flow

When `HostCommand::Run { input }` is received:

1. **Parse**: `wasmsh_parse::parse(input)` → `Program` AST
2. **Lower**: `wasmsh_hir::lower(&ast)` → `HirProgram`
3. **Update `$LINENO`** from span position
4. **For each command**:
   a. Resolve command substitutions (`$(...)`) by recursive execution
   b. Expand words (parameter, arithmetic, tilde, case modification, indirect)
   c. Check `nounset` (`set -u`) for unbound variables
   d. Apply brace expansion
   e. Apply glob expansion against VFS (respects `noglob`, `nullglob`, `dotglob`, `extglob`, `globstar`)
   f. Expand aliases before dispatch
   g. Dispatch: runtime command (declare, alias, let, shopt, ...) → builtin → function → utility
   h. Apply output redirections (capture stdout → write to VFS file)
   i. Trace output if `xtrace` (`set -x`) is active
5. **Collect events**: stdout, stderr, diagnostics → `Vec<WorkerEvent>`
6. **Set `$PIPESTATUS`** array after pipeline execution
7. **Fire traps**: EXIT trap if exit was requested
