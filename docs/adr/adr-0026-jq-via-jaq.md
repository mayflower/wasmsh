# ADR-0026: jq via jaq-core

## Status

Accepted

## Context

Before this ADR, wasmsh's `jq` utility was a from-scratch
reimplementation of the jq filter language in `jq_ops.rs`:

- A JSON parser (`JsonParser`) with its own error recovery and
  number-formatting quirks.
- A JSON value model (`JqValue`) with custom hashing and ordering.
- A jq filter-language lexer + parser (`parse_filter`,
  `JqFilter` AST) supporting a subset of real jq syntax.
- A filter evaluator (`execute_jq_inputs`, `JqEnv`) that walked the
  AST against input values.
- A pretty-printer.
- Option parsing for `-r`, `-c`, `-e`, `-n`, `-s`, `--arg`,
  `--argjson`.

The whole thing came to **8032 lines** — about 10% of the entire
`wasmsh-utils` crate — and it was the largest single piece of
handwritten code in the workspace.  It implemented a subset of jq's
filter language; common idioms like `def foo(x): …;` recursive
definitions, more sophisticated path expressions, and many
stdlib-level filters (`@base64`, `@uri`, `ascii_downcase`,
`fromdate`, `strftime`, `fromjson`, `tojson`, `test`, `match`,
`capture`, `sub`, `gsub`, `walk`, `paths`, `leaf_paths`, `tostream`,
`limit`, `first`, `last`, `nth`, `until`, `repeat`, `group_by`,
`unique`, `min_by`, `max_by`, …) were either absent or subtly
different from real jq.

jq is not a simple language.  It's a concatenative functional
combinator language with path expressions, backtracking semantics,
variadic destructuring, and a 200-entry standard library.  Real jq
agents regularly write filters that exercise features the
handwritten implementation did not cover, and silently producing
the wrong answer (instead of an error) is the worst possible
failure mode for a JSON transformation tool.

[`jaq`](https://github.com/01mf02/jaq) is an audited pure-Rust
reimplementation of jq that aims to be a drop-in replacement and
that **beats real jq on most benchmarks**.  Its properties:

- Pure Rust, no C dependencies.  Compiles cleanly for
  `wasm32-unknown-unknown`.
- Licensed **MIT** — compatible with ADR-0001.
- Split into narrow companion crates (`jaq-core`, `jaq-std`,
  `jaq-json`, `jaq-syn`) so embedders can pick exactly what they
  need.
- **Security audited** by Radically Open Security under an NLnet
  grant.  Fuzzing targets included.
- Actively maintained (2.2 released 2025).
- Used in production embedded in several other tools.

## Decision

Replace the entire `jq_ops.rs` with a thin adapter (~400 lines of
option parsing, input collection, output formatting) over
`jaq-core` + `jaq-std` + `jaq-json`.  The CLI surface that wasmsh
exposes to shell scripts is unchanged: same flags, same stdin/file
handling, same exit codes.

### Dependencies added

| Crate         | Version | License     | Purpose                                 |
|---------------|---------|-------------|-----------------------------------------|
| `jaq-core`    | 2       | MIT         | Filter interpreter core                 |
| `jaq-std`     | 2       | MIT         | Standard library filters                |
| `jaq-json`    | 1       | MIT         | JSON value type + parser integration    |
| `hifijson`    | 0.2     | MIT         | Streaming JSON lexer (used by jaq-json) |

Transitively: `typed-arena`, `memchr`, `once_cell`, `regex-lite`,
`log`, `equivalent`, `hashbrown`, `urlencoding`, `foldhash`,
`aho-corasick`, `chrono`, `num-traits`, `indexmap`, `dyn-clone`,
`libm`.  All MIT/Apache-2/BSD — already on the `deny.toml`
allowlist.

### Architecture

```
                   ┌──────────────────┐
  argv ───► parse_jq_option  ◄─── JqOpts (flags, --arg, --argjson)
                   │
                   ▼
           extract_filter_arg
                   │
                   ▼
           compile_filter(source, var_names)
                   │    │
                   │    └─► Loader::load + Compiler::compile
                   │         (jaq-core + jaq-std + jaq-json defs/funs)
                   ▼
              jaq_core::Filter<Native<Val>>
                   │
                   ▼
           collect_jq_input_texts (stdin/files)
                   │
                   ▼
           parse_jq_input_values
                   │        └─► hifijson::SliceLexer → Val::parse
                   ▼
           execute_filter
                   │    └─► filter.run((Ctx, input))
                   ▼
           emit_value → format_value → format_pretty (wasmsh's
                                                       jq-style
                                                       indenter)
```

### What the wasmsh wrapper still owns

- **Option parsing**.  wasmsh supports a specific subset of jq's
  CLI flags and must not silently accept or reject flags the way
  jaq's binary does.  We parse the flags ourselves and hand only
  the filter source and the var map to jaq-core.
- **stdin / file input collection**.  Reading from `ctx.stdin` or
  `ctx.fs` is wasmsh's responsibility.  jaq-core is
  file-system-agnostic.
- **Pretty printing**.  `jaq-json`'s `Display` impl is
  compact-only — it does not honour the `{:#}` alternate format
  flag — so the wrapper implements a jq-style 2-space indenter
  that walks the `Val` tree directly.  Matching real jq's output
  byte-for-byte matters for the TOML conformance suite.
- **Error diagnostics**.  wasmsh translates jaq's error types into
  the human-readable `"jq: …"` format that existing scripts and
  the TOML conformance tests depend on.

### Regex semantics note

jaq's `test`, `match`, `capture`, `sub`, `gsub` filters use
`regex-lite` (leftmost-first PCRE-style).  This is **intentionally
different** from wasmsh's `sed`/`grep`/`awk` regex engine (POSIX
BRE/ERE via `posix-regex`, see ADR-0022 / ADR-0023).  Real jq uses
Oniguruma, which is also PCRE-style.  So:

- `sed 's/[0-9]+/N/'` → POSIX ERE leftmost-longest
- `jq '.field | sub("[0-9]+"; "N")'` → PCRE leftmost-first

This matches real jq and real sed, and is what users expect.

## Alternatives Considered

### Keep the handwritten implementation

Rejected.  8000 lines of hand-rolled jq was the biggest piece of
NIH code in wasmsh, it only supported a subset, and new features
would need to be ported one by one.  The maintenance cost is
unbounded.

### Use `xq` (pure-rust jq, MiSawa)

Rejected.  `xq` is another pure-Rust jq port, but it's less
actively maintained than jaq and has not been security-audited.
jaq is the ecosystem-consensus choice.

### Shell out to an external jq binary

Not possible.  wasmsh runs inside a WASM sandbox with no process
spawning.  jq must be linked in.

### Skip migration until a bug is reported

Rejected by the design principle established in ADR-0023 and
ADR-0024: prefer established libraries over reinvention, not just
when a bug surfaces.

## Consequences

- **~7300 lines of hand-rolled code deleted** from `jq_ops.rs`
  (8032 → 732).  The remaining 732 lines are: wrapper code for the
  wasmsh CLI surface (~400 lines of flag parsing, input
  collection, output formatting, pretty printer) and 24 focused
  unit tests that verify the wrapper at the `util_jq` level.
- **Full jq filter language**.  The complete jq 1.7-ish syntax and
  standard library is now available:
  - All stdlib filters (`@base64`, `@uri`, `fromdate`,
    `strftime`, `test`, `match`, `capture`, `sub`, `gsub`,
    `walk`, `paths`, `limit`, `group_by`, etc.)
  - `def name(x): …;` user-defined filters
  - `reduce` / `foreach`
  - Path expressions like `.a[1:3].b?`
  - Destructuring pattern matching
- **Performance**.  jaq is faster than real jq on most benchmarks;
  no hand-optimisation required.
- **Security**.  Audited and fuzzed.  The old handwritten parser
  was not.
- **New workspace deps**: `jaq-core`, `jaq-std`, `jaq-json`,
  `hifijson`.  All MIT, no additional license review needed.
- **Binary size**: modest increase.  The wasm build still links
  cleanly and the standalone browser E2E suite (15/15) passes.
- **Tests**:
  - 305 handwritten jq unit tests removed (they tested the
    handwritten implementation, not jq semantics).
  - 24 new focused wrapper tests verify the CLI surface
    (`util_jq` entry point, option parsing, pretty vs compact,
    `-r` raw output, `-e` exit status, `--slurp`, `-n`, `--arg`,
    `--argjson`, stdin/file input, error paths).
  - All TOML conformance tests pass (544/550, 5 skipped on
    missing features, 0 failing).
- **Regression-safety**: the `rw48_jq_transform.toml` real-world
  conformance test (which exercises pretty-printed output of a
  filtered array) passes unchanged.
