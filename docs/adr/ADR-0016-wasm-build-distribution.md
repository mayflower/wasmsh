# ADR-0016: WASM Build and Distribution Pipeline

## Status
Accepted

## Context
wasmsh targets browser environments via wasm32-unknown-unknown. The compiled WASM binary needs to be usable from JavaScript/TypeScript bundlers (webpack, vite), ES modules (browser `<script type="module">`), and Node.js (CommonJS). A standardized build and distribution pipeline is needed.

## Decision
Use wasm-pack to build three targets from the wasmsh-browser crate:

| Target | Format | Use Case |
|--------|--------|----------|
| `bundler` | ES module + .wasm | webpack, vite, Rollup |
| `web` | ES module + .wasm | Browser `<script type="module">` |
| `nodejs` | CommonJS + .wasm | Node.js, testing, Python bridge |

The wasmsh-browser crate uses `crate-type = ["cdylib", "rlib"]`:
- `cdylib` produces the .wasm binary via wasm-bindgen
- `rlib` allows other Rust crates to depend on it

### Build optimization
wasm-opt post-processes all .wasm files with `-Os` and feature flags:
`--enable-bulk-memory --enable-mutable-globals --enable-sign-ext --enable-nontrapping-float-to-int`

### Distribution
- **GitHub Releases**: Tagged pushes (`v*`) trigger the Build WASM workflow which creates a GitHub Release with bundler/web/nodejs tarballs
- **GitHub Actions artifacts**: Every push to main uploads ephemeral artifacts (90-day retention)
- **npm**: Future (not yet published)
- **crates.io**: Future (workspace deps have `version = "0.1.0"` ready)

## Consequences
- Three build targets cover all JavaScript ecosystem use cases
- wasm-bindgen generates TypeScript type definitions automatically
- u64 parameters map to BigInt in JavaScript (e.g., step_budget)
- serde JSON serialization bridges the protocol types (WorkerEvent, HostCommand) between Rust and JS
- Binary size tracked via CI size-budget check (2 MB limit)
