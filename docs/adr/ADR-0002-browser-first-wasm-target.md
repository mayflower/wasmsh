# ADR-0002: Browser-First on wasm32-unknown-unknown

## Status
Accepted

## Context
wasmsh must run in the browser. `wasm32-unknown-unknown` is the minimal Rust WebAssembly target without standardized host imports.

## Decision
The primary target is `wasm32-unknown-unknown`. Browser integration is done via `wasm-bindgen`/`web-sys` and a Web Worker.

## Consequences
- No reliance on `std::fs`
- No host process model
- No implicit thread usage
- Platform access exclusively through explicit adapters
