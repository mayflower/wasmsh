# ADR-0005: HIR/IR/VM Instead of Direct AST Interpretation

## Status
Accepted

## Context
Direct AST execution mixes syntax with runtime semantics and makes limits, tracing, and optimization harder.

## Decision
Pipeline:
`AST -> HIR -> IR -> VM`

## Consequences
- Better budgeting and cancellation
- Clearer builtin/utility boundaries
- More architectural effort, but better scalability
