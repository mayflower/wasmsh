# ADR-0014: Embedded Interpreters for Complex Data-Processing Utilities

## Status
Accepted

## Context
Several essential agent utilities (jq, awk, yq, bc) require their own expression languages, parsers, and evaluators. These cannot be implemented as simple stdin→stdout text transformations. The question is whether to depend on external interpreter libraries or implement clean-room versions.

## Decision
Complex utilities are implemented as self-contained embedded interpreters within the wasmsh-utils crate:

- **jq** (5,400 lines): Handwritten JSON parser, filter language parser (recursive descent), and tree-walking evaluator with 90+ built-in functions
- **awk** (3,950 lines): Handwritten lexer, recursive-descent parser, and interpreter with associative arrays, user-defined functions, and regex engine
- **yq** (1,300 lines): Handwritten YAML parser with indentation tracking, jq-compatible filter subset
- **bc** (1,085 lines): Handwritten expression parser and evaluator with variables, control flow, and user-defined functions

Each interpreter follows the same architecture: tokenizer → parser → AST → evaluator, matching the shell's own pipeline pattern.

## Consequences
- **Clean-room guarantee maintained**: No external interpreter dependencies, no GPL code
- **Full control over safety**: Each interpreter has iteration limits (awk: 1M, jq: depth 1000) and allocation guards
- **Self-contained regex**: awk and rg each contain their own regex engine supporting `.`, `*`, `+`, `?`, `^`, `$`, `[...]`, `\d`, `\w`, `\s`
- **Maintenance burden**: ~12,000 lines of interpreter code to maintain
- **No std dependency**: All interpreters operate on VFS abstractions, not filesystem calls
- **Consistent error model**: All interpreters emit errors via the UtilOutput stderr channel and return appropriate exit codes
