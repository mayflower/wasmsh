# ADR-0007: Command Model = Builtin / Function / Utility / Virtual External

## Status
Accepted

## Context
In the browser, shell commands cannot rely on a classic fork/exec model.

## Decision
Each command is classified into one of four categories during resolution:
- Builtin
- Shell Function
- Bundled Utility
- Virtual External (host profile only)

## Consequences
- Browser profile remains realistic
- Shell state remains correctly mutable
- Utilities can be developed separately
