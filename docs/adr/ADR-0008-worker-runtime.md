# ADR-0008: Browser Execution in Web Worker

## Status
Accepted

## Context
Parsing, expansion, and execution must not block the UI.

## Decision
wasmsh always runs in a Worker in the browser and communicates via a versioned message protocol.

## Consequences
- Good UI responsiveness
- Clean decoupling between shell core and frontend
- Message protocol must be kept stable
