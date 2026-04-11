# ADR-0003: Handwritten Parser

## Status
Accepted.  **Scope clarified** by ADR-0024 (see "Scope" section below).

## Context
Shell syntax is context-sensitive and tightly couples tokenization, quoting, here-docs, and expansion.

## Decision
wasmsh uses:
- A handwritten stateful lexer
- A handwritten recursive-descent parser
- Separate subparsers for `${}`, `$(( ))`, `[[ ]]`, and patterns

## Scope

This ADR governs the **shell grammar** specifically — the code that
lives in `wasmsh-lex`, `wasmsh-parse`, and the expansion-aware
sub-parsers listed above.  The rationale (context-sensitivity, tight
coupling between tokenization and quoting/here-docs/expansion) is
specific to bash-family grammars and does not generalise.

It does **not** forbid using parser libraries inside individual
utilities that parse their own sub-languages:

- awk's expression and action grammar is a standard recursive-descent
  problem that could be replaced with a parser combinator or LALR
  tool.
- jq's filter language is a well-specified functional-combinator
  grammar that a dedicated crate (`jaq-core` / `jaq-parse`) already
  implements.
- YAML is a formally specified serialization format that existing
  crates (`saphyr`, `serde_yaml`) already cover.
- sed's address and command grammar is small enough that any
  handwritten choice is fine, but nothing prevents using a helper
  crate if one is a better fit.

Future ADRs may adopt established crates for these sub-language
parsers on their own merits.  The restriction in this ADR applies
only to the shell grammar itself.

## Consequences
- Better control over shell edge cases
- Higher initial effort
- Clearer error diagnostics and better browser suitability in return
- Sub-language parsers inside utilities are *not* bound by this
  decision; they are governed by their own future ADRs (see Scope
  above).
