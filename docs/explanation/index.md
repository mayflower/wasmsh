# Explanation

Explanation pages cover *why* wasmsh is built the way it is. They are the
right place to look when you want to understand the architecture, the
trade-offs, or the constraints that shaped a particular decision.

Explanation is for understanding. For step-by-step learning see
[Tutorials](../tutorials/index.md). For recipes see [How-to Guides](../guides/index.md).
For exact surfaces see [Reference](../reference/index.md).

## Pages

| Page | Contents |
|------|----------|
| [Architecture](architecture.md) | Pipeline, crate map, the three deployment shapes (standalone, Pyodide, scalable), execution flow, key implementation choices |
| [Design decisions](design-decisions.md) | The "why" behind each major choice, with links to the corresponding ADRs |
| [Snapshot runner](snapshot-runner.md) | The scalable deployment path: dispatcher + runner pool, session affinity, restore-capacity routing, graceful drain, the client surface shared by both adapters |

## Going Deeper

For the complete record of design decisions, see the
[Architectural Decision Records (ADRs)](../adr/). Each ADR captures the
context, decision, and consequences of one architectural choice and is
the canonical source for the long-form rationale.
