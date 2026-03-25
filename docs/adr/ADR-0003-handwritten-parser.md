# ADR-0003: Handwritten Parser

## Status
Accepted

## Context
Shell syntax is context-sensitive and tightly couples tokenization, quoting, here-docs, and expansion.

## Decision
wasmsh uses:
- A handwritten stateful lexer
- A handwritten recursive-descent parser
- Separate subparsers for `${}`, `$(( ))`, `[[ ]]`, and patterns

## Consequences
- Better control over shell edge cases
- Higher initial effort
- Clearer error diagnostics and better browser suitability in return
