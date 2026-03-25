# ADR-0009: Cooperative Budgets and Cancellation

## Status
Accepted

## Context
An agentic sandbox service requires runtime limits and interruptibility.

## Decision
The VM receives:
- Step budget
- Memory budget hooks
- Cancellation token
- Event sink for progress/trace

## Consequences
- Controllable runtime
- Safe embedding in services
- Slightly more VM complexity
