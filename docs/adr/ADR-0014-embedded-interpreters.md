# ADR-0014: Embedded Interpreters for Complex Utilities

## Status
Accepted

## Context
Important agent utilities (jq, awk, yq, bc) require their own expression languages with parser and evaluator. Alternatives would be external crate dependencies or delegation to the host.

## Decision
Each complex utility is implemented as a standalone interpreter within `wasmsh-utils`: Tokenizer -> Parser -> AST -> Evaluator. No external interpreter, no GPL dependency.

- **jq**: JSON parser, filter language, 90+ built-in functions
- **awk**: Lexer, recursive descent, associative arrays, user functions, custom regex engine
- **yq**: YAML parser with indentation tracking, jq-compatible filter subset
- **bc**: Expression parser with variables, control flow, user functions

## Consequences
- Clean-room guarantee is preserved (no external interpreter deps)
- Safety through iteration limits (awk: 1M, jq: depth 1000) and allocation protection
- ~12,000 lines of interpreter code as maintenance burden
- Custom regex engines in awk and rg (no regex crate)
