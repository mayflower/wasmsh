# ADR-0004: Structured Word ASTs and Late Expansion

## Status
Accepted

## Context
Shell words must not collapse into strings prematurely, otherwise quoting and expansion properties are lost.

## Decision
Words are modeled as structured `WordPart` trees. Expansion occurs in clearly separated phases only after parsing.

## Consequences
- Correct behavior for quotes, field splitting, and globs
- More internal types
- Better testability
