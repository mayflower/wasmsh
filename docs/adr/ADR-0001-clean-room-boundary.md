# ADR-0001: Clean-Room Compatibility Instead of Code Adoption

## Status
Accepted

## Context
wasmsh aims for high shell compatibility but must not adopt any BusyBox or Bash code into the core.

## Decision
The project pursues **behavioral compatibility** as a goal, but **no adoption of source code, help text, tables, or test suites** from BusyBox/Bash.

## Consequences
- We implement parser, expansions, and runtime ourselves
- We use POSIX as the normative baseline
- Bash/BusyBox serve only as black-box references at most
- Provenance must be documented per major PR
