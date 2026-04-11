# ADR-0022: POSIX Regex Engine for sed and grep

## Status

Accepted

## Context

Through 2025 the shell utilities `sed`, `grep`, and `awk` in wasmsh used
three different pattern-matching paths:

- `awk` carried a hand-written regex engine embedded in `awk_ops` that
  supports character classes, quantifiers, anchors, and escapes.
- `sed` did literal substring matching via `str::replace` and
  `grep_matches()` for address resolution. `grep_matches()` is a
  five-line helper that handles `^` and `$` anchors and otherwise
  falls back to `str::contains`.
- `grep` reused the same `grep_matches()` literal matcher.

An LLM agent running in the sandbox uncovered the gap when the command
`sed -n '/\[section\]/p'` produced no output. The literal matcher
was looking for the ten-character string `\[section\]`, not the
five-character string `[section]`. The same category of bug affected
`grep '[0-9]'`, `sed 's/foo.*bar/X/'`, `grep -E 'foo|bar'`, and every
other pattern that required any regex semantics.

POSIX and historical Unix practice are very specific about which
dialect each tool accepts:

| Tool | Default dialect | Flag for ERE |
|------|-----------------|--------------|
| `grep` | BRE | `-E` |
| `sed`  | BRE | `-E` / `-r` |
| `awk`  | ERE (always) | — |

Both dialects also specify leftmost-longest matching, which differs
from the leftmost-first semantics used by PCRE, the Rust `regex`
crate, and most modern regex engines.

## Decision

Add the **`posix-regex`** crate (version 0.1, MIT license) as a workspace
dependency and wrap it in `crates/wasmsh-utils/src/regex_posix.rs`.
Wire `sed` and `grep` through the wrapper. Leave `awk`'s embedded
engine in place for now.

### Why posix-regex

The rust-lang/regex crate and its trimmed-down `regex-lite` sibling
both use a different dialect (Rust regex, a PCRE variant) and
different matching semantics (leftmost-first). Agents and users
writing `sed -E 's/sam|samwise/X/'` expect POSIX leftmost-longest,
not leftmost-first. The rust-lang/regex maintainer has stated (in
rust-lang/regex#858) that adding POSIX BRE/ERE parsing is
out-of-scope.

`posix-regex` was written for Redox OS's `relibc` and is the only
actively-maintained Rust crate that natively parses **both** POSIX
BRE and POSIX ERE with a matching engine that implements the POSIX
semantics. Its properties:

- **Zero runtime dependencies.** No transitive crates pulled in.
- **`no_std` compatible.** Works on any wasm target.
- **MIT license** — already on the `deny.toml` allowlist.
- **Actively maintained** — 0.1.4 released 2025-05-03.
- Supports the full feature surface we need: anchors, character
  classes (including POSIX class expressions like `[[:digit:]]`),
  ranges, quantifiers (`*`, `+`, `?`, `{n}`, `{n,}`, `{n,m}`),
  groups, alternation, word boundaries `\<`/`\>`, `\d`/`\s`/`\w`
  escapes, backreferences `\1`..`\9`.
- Used in production in `relibc`.

Its one notable limitation is that it operates on byte slices (ASCII
semantics). For wasmsh this is **exactly what we want**: GNU `grep`
and `sed` under `LC_ALL=C` behave the same way, the VFS stores bytes,
and a sandbox's text processing should be deterministic across host
locales.

### Architecture

```
text_ops::util_sed ─┐
text_ops::util_grep ┼──►  regex_posix::Regex (BRE or ERE)
                    │         │
                    │         ▼
                    │    posix_regex::PosixRegex<'static>
                    │
                    ▼
        literal fallback (on compile error)
```

The wrapper exposes a small API (`is_match`, `find`,
`find_iter_offsets`, `replace`, `replace_all`, `replace_nth`). Each
caller compiles the pattern on demand and falls through to the
previous literal behaviour if compilation fails — so malformed
patterns continue to behave sensibly rather than silently failing.

### Empty-pattern semantics

`posix-regex` reports zero matches for an empty pattern. POSIX grep
defines `grep ""` as matching every line (the zero-width match at
position 0 is considered a match). The wrapper short-circuits the
empty-pattern case in `is_match`/`find` so idioms like
`grep -c "" file` continue to work.

### awk remains separate

`awk`'s regex engine is tangled with its expression evaluator and
tokenizer. Migrating it is a separate, larger project and carries
risk (awk's regex supports some GNU-specific behaviours the POSIX
engine does not). It is deferred to a future ADR.

## Alternatives Considered

### Keep a hand-written engine

We could extract the existing regex engine from `awk_ops` and expose
it as a crate module. This was the original plan until research
surfaced `posix-regex`. Pros: no new dependency, full control.
Cons: reinventing the wheel, non-POSIX semantics, incomplete feature
set (no `{n,m}`, no POSIX class expressions, no word boundaries), no
leftmost-longest guarantee.

### `rust-lang/regex`

200–600 KiB wasm binary impact. Wrong dialect (PCRE-ish syntax, no
BRE/ERE parsing). Wrong matching semantics (leftmost-first, not
leftmost-longest). Rejected.

### `regex-lite`

Same dialect / semantics mismatch as `regex`, just smaller. Rejected.

### `ere` crate

Compile-time only. Patterns in wasmsh come from user scripts at
runtime. Rejected.

## Consequences

- New dependency: `posix-regex = "0.1"` in `Cargo.toml` (MIT,
  zero transitive deps, no_std).
- `crates/wasmsh-utils/src/regex_posix.rs` is the single entry point
  for regex in text utilities. Call sites that previously did literal
  matching now get real regex semantics.
- `sed`, `grep`, `grep -E` now behave like POSIX sed/grep for the
  supported feature set. Agents and human users can write patterns
  like `\[section\]`, `[0-9]+`, `^#.*fixme`, `foo|bar` and get
  POSIX-correct results.
- Malformed patterns fall through to literal substring matching,
  preserving the historical behaviour for any existing scripts that
  accidentally relied on it.
- `awk`'s regex remains separate until a follow-up ADR migrates it.
- License check (`cargo deny check licenses`) stays clean — MIT is
  already allowed.
- Binary size impact: negligible (posix-regex is small and has no
  transitive deps).
- The wasm32 standalone build and the wasm32-unknown-emscripten
  Pyodide build both compile cleanly against the new dependency.
