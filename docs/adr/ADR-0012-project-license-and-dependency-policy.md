# ADR-0012: Project License = Apache-2.0, Dependency Policy = Permissive First

## Status
Accepted (updated 2026-04-02: changed from MIT OR Apache-2.0 to Apache-2.0 only)

## Context
wasmsh should be broadly usable, commercially integrable, and compatible with MPL-2.0-licensed Pyodide. Apache-2.0 is one-way compatible with MPL-2.0 (MPL code can be combined with Apache-2.0 code in a larger work), whereas MIT lacks the patent grant that makes this combination cleaner.

## Decision
Project license for code: `Apache-2.0`.

## Consequences
- Good adoption potential
- Easy integration into many products
- Patent grant provides clearer IP safety for downstream users
- Clean compatibility when linking into Pyodide (MPL-2.0)
- GPL dependencies are kept out of the core
