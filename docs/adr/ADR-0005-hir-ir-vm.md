# ADR-0005: HIR/IR/VM Instead of Direct AST Interpretation

## Status
Accepted

Clarified by [ADR-0029](adr-0029-dual-path-executor.md).

## Context
Direct AST execution mixes syntax with runtime semantics and makes limits, tracing, and optimization harder.

## Decision
The long-term execution direction is:
`AST -> HIR -> IR -> VM`

At the time of this ADR, that described the target architecture rather
than a completed migration. See ADR-0029 for the current adopted model:
`wasmsh-runtime` interprets HIR directly and lowers only a bounded subset
through IR/VM.

## Consequences
- Better budgeting and cancellation
- Clearer builtin/utility boundaries
- More architectural effort, but better scalability
- The repository must distinguish between the long-term IR/VM direction
  and the currently implemented dual-path executor
