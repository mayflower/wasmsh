# ADR-0010: POSIX Baseline Plus Explicit Bash Extensions

## Status
Accepted

## Context
"Bash-compatible" is too vague without further specification.

## Decision
The normative baseline is the POSIX Shell Command Language. Beyond that, Bash extensions are adopted explicitly per profile and feature flag.

## Consequences
- Clearer roadmap
- Fewer implicit compatibility promises
- Better test and documentation structure
