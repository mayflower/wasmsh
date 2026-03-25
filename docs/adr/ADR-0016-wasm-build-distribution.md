# ADR-0016: WASM Build and Distribution Pipeline

## Status
Accepted

## Context
wasmsh targets browser environments via wasm32-unknown-unknown. The compiled WASM must be usable from JS bundlers (webpack, vite), ES modules (browser), and Node.js (CommonJS).

## Decision
wasm-pack builds three targets from the wasmsh-browser crate:

- **bundler** (ES module + .wasm) -- webpack, vite, Rollup
- **web** (ES module + .wasm) -- browser `<script type="module">`
- **nodejs** (CommonJS + .wasm) -- Node.js, testing, Python bridge

Crate type is `["cdylib", "rlib"]`: `cdylib` produces the .wasm binary via wasm-bindgen, `rlib` enables Rust-internal dependency.

wasm-opt optimizes all .wasm files with `-Os` and enables the features required by Rust 1.94+ (bulk-memory, mutable-globals, sign-ext, nontrapping-float-to-int).

### Distribution
- **GitHub Releases**: Tag push (`v*`) creates a release with bundler/web/nodejs tarballs
- **GitHub Actions Artifacts**: Every push to main uploads ephemeral artifacts (90 days)
- **npm / crates.io**: Prepared, not yet published

## Consequences
- Three build targets cover the entire JS ecosystem
- wasm-bindgen automatically generates TypeScript type definitions
- u64 parameters become BigInt in JavaScript (e.g., step_budget)
- Binary size is monitored via CI budget (2 MB)
