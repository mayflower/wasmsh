# ADR-0027: yq via saphyr + jaq

## Status

Accepted

## Context

Before this ADR, wasmsh's `yq` utility was a from-scratch
reimplementation of both a YAML parser and a jq-subset filter
language, totalling ~2000 lines in `yaml_ops.rs`:

- A handwritten YAML 1.1-ish parser (`YamlParser`) that walked
  line-by-line, tracking indentation and scalar types.  It worked
  for the common subset of YAML used in CI configs and agent
  scripts but missed features (merge keys `<<`, YAML 1.2 specifics,
  complex keys, anchors/aliases beyond the trivial case, tag
  handling) and had its own bugs.
- A handwritten filter-language lexer / parser / evaluator (the
  `Filter` enum and `eval_filter` function) supporting 17 filter
  variants: `.`, `.key`, `.[N]`, `.[]`, `f | g`, `keys`, `values`,
  `length`, `type`, `select()`, `first`, `last`, `flatten`,
  `map()`, `not`, and dot chains.  Everything beyond that was a
  "unsupported filter" error.
- A YAML/JSON emitter pair.

The filter language was a **pure duplicate** of work already done
in ADR-0026: jaq-core (via `jq_ops`) implements the full jq filter
language and standard library.  The YAML parser was similarly a
duplicate of work well-served by `saphyr` — a YAML 1.2–compliant
pure-Rust parser that already compiles cleanly for wasm32.

Since the design principle established in ADR-0023 is "prefer
established libraries over reinvention", yq was the most obvious
next candidate: migrating it means **zero new semantics** (the
filter language is the same one jq speaks) combined with a
well-tested YAML parser.

## Decision

Adopt `saphyr` as the YAML parser and reuse the jaq-core
infrastructure (via a new `jaq_runner` helper module) for the
filter language.

### Dependencies added

| Crate         | Version | License            | Purpose                          |
|---------------|---------|--------------------|----------------------------------|
| `saphyr`      | 0.0.6   | MIT / Apache-2     | YAML 1.2 parser                  |
| `foldhash`    | 0.1     | Zlib / Apache-2    | Matches jaq-json's IndexMap hasher |

Transitively (via saphyr): `saphyr-parser`, `hashlink`, `encoding_rs`,
`arraydeque`, `ordered-float`, `autocfg`.  All MIT / Apache-2 /
Zlib — already on `deny.toml`'s allowlist.

### Architecture

```
                        ┌─────────────────────────┐
   argv ──► parse_yq_flags  ◄──────── YqOptions (raw, compact, json)
                        │
                        ▼
                extract filter_src + file_args
                        │
                        ▼
           jaq_runner::compile_filter(filter_src, [])
                        │       │
                        │       └─► jaq-core + jaq-std + jaq-json defs/funs
                        │
                        ▼
                 read_yq_input (stdin/files)
                        │
                        ▼
                 saphyr::Yaml::load_from_str
                        │
                        ▼
                    yaml_to_val(&Yaml)    ◄──── recursive convert
                        │                       to jaq_json::Val
                        ▼
           jaq_runner::run_filter(filter, input, [])
                        │
                        ▼
                  format_yq_result
                   │    │     │
                   │    │     └─ raw_string (-r)
                   │    └─────── jaq_runner::format_json (-j)
                   └──────────── format_yaml (default)
```

The whole thing is ~600 lines including 16 unit tests, down from
2000+ lines of hand-rolled code.

### Why a shared `jaq_runner` module

Both `jq_ops` and `yaml_ops` need the same compile/execute/format
boilerplate: `Loader::new(...)`, `Arena::default()`, the
`.with_global_vars(...)` fiddle, the `RcIter` empty inputs, the
`Ctx::new` dance, and the pretty-printer.  Extracting this into
`jaq_runner.rs` keeps both call sites small and ensures they
produce byte-for-byte identical JSON output when the user asks for
it.

### YAML → jaq `Val` conversion

YAML's type system is richer than JSON's:

- YAML has integers with no upper bound; we store oversized values
  as `Val::Num(Rc<String>)`, which is jaq's own lossless
  representation.
- YAML has float variants (NaN, ±Infinity); `Val::Float` handles
  the finite cases.
- YAML mapping keys can be any type; we stringify non-string keys
  via a simple `yaml_key_to_string` helper so filters can still
  reach them with `.key_name` syntax.
- Tagged nodes and aliases unwrap to their underlying value —
  wasmsh's yq is a data processor, not a schema validator.

### YAML output formatter

jaq-json's `Display` impl is compact JSON only.  The default yq
output mode is YAML, so we keep a small handwritten YAML emitter
(`format_yaml_node` / `yaml_format_array` / `yaml_format_object`)
that walks a `Val` directly.  Rules:

- Strings without YAML-significant characters emit bare, no quotes.
- Strings containing `:`, `#`, `\n`, or that parse as another type
  (bool, null, number) get double-quoted with `\\` / `\"` / `\n`
  escaping.
- Arrays use dash-prefixed block style.
- Mappings use `key: value` block style.
- Nested composites indent 2 spaces.

This matches the previous handwritten emitter's output, so the
TOML conformance suite's `yq_*` cases pass without modification.

### CLI surface unchanged

The flags (`-r`, `-e`, `-c`, `-j`) and their combined forms (`-rj`,
`-jc`, etc.) behave identically to the previous implementation.
Filter language coverage is now the full jq (`-2`, `def`, path
expressions, destructuring, reduce/foreach, the entire stdlib),
replacing the 17-variant hand-rolled subset.

## Alternatives Considered

### Keep handwritten YAML parser

Rejected.  YAML is a complex spec, and the handwritten parser was
a maintenance burden that was silently wrong on any non-trivial
document.  saphyr passes the YAML 1.2 conformance suite and is
actively maintained.

### Use `serde_yaml` / `serde_yaml_bw` with serde-json round-trip

Considered but rejected.  serde_yaml is unmaintained (the crate
author archived it).  serde_yaml_bw is a fork but adds a serde
dependency we'd otherwise not need in this crate.  saphyr is a
direct replacement with a smaller surface.

### Share the YAML parser only, keep the filter language

Possible but strictly worse.  The filter language duplication
between jq_ops and yaml_ops was the bigger cost, and merging it
with `jaq_runner` costs almost nothing.

### Use a different jq-subset crate for yq specifically

Not worth it.  jaq-core is already in the tree for jq; reusing it
costs one new module (`jaq_runner`) and gives yq full jq filter
coverage.

## Consequences

- **~1500 lines of hand-rolled code deleted** from `yaml_ops.rs`
  (2056 → 593).
- **Combined with ADR-0026, ~8700 lines of jq+yq code deleted**.
  The full YAML-to-JQ-to-output pipeline now goes through audited
  libraries end-to-end.
- **Full YAML 1.2 support** via saphyr — anchors, aliases, merge
  keys, complex types, tag handling all work correctly.
- **Full jq filter language** in yq — `def name(x): …`, reduce,
  foreach, path expressions, destructuring, and the full stdlib
  are now available.  Previously yq only supported a 17-variant
  subset and rejected everything else with "unsupported filter".
- **New shared `jaq_runner` module** (~170 lines) that both
  `jq_ops` and `yaml_ops` use.  Produces byte-for-byte identical
  JSON output between `jq` and `yq -j`.
- **New dependencies**: `saphyr` (+ saphyr-parser, hashlink,
  encoding_rs, arraydeque, ordered-float, autocfg) and `foldhash`.
  All permissively licensed.
- **Tests**: 70 handwritten yq unit tests removed (they tested
  the handwritten parser and filter engine, not yq semantics).
  16 new focused wrapper tests verify the CLI surface (identity,
  field access, pipe composition, length, keys, iterate, select,
  type filter, `-r` raw, `-j` JSON output, `-c` compact,
  `-e` exit status, parse error handling, nested object output).
  All 550 TOML conformance tests pass without modification
  (including all `yq_*` cases).
- **wasm32 verification**: both `wasmsh-utils` and `wasmsh-browser`
  compile cleanly for `wasm32-unknown-unknown`.  Standalone
  browser E2E (15/15) passes against a wasm build with saphyr +
  jaq-core + jaq-std + jaq-json + hifijson + foldhash all linked
  in.

## Scope boundaries

- **Complex YAML features** (anchors across documents, schema
  validation, custom tag handlers) are handled by saphyr but are
  not exercised by wasmsh's current tests.  If an agent needs
  them, they should Just Work — but we haven't added regression
  tests for them.
- **TOML** (`@toml` filter, etc.) is provided by `jaq-json`'s
  standard library additions but not specifically verified by
  wasmsh tests.  TOML output is not a wasmsh use case today.
- **The shared `jaq_runner` module** is crate-private.  It is not
  part of the public `wasmsh-utils` API and its signatures may
  change freely across versions.
