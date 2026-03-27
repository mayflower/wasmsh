# ADR-0020: E2E-First Testing Policy

## Status

Accepted

## Context

Adding the Pyodide build introduced multiple integration boundaries: Rust → Emscripten linking, Emscripten module → JS host, JS host → browser worker, Python ↔ shell filesystem sharing. Unit tests at each layer would miss the interactions that cause real failures.

## Decision

Adopt an E2E-first testing policy for the browser and Pyodide integration layers:

1. **Every new integration capability starts with a failing E2E test** (red-green-refactor).
2. **Real browser tests** via Playwright — no mocked workers, no fake FS, no simulated browser APIs.
3. **Real Node tests** for Pyodide — loading the actual custom-built wasm module, not mocking the C ABI.
4. **Protocol parity tests** verify that both standalone and Pyodide paths produce identical event shapes for the same commands.

Test matrix:

| Layer | Runner | Location |
|-------|--------|----------|
| Rust unit/integration | `cargo test` | `crates/*/tests/`, inline `#[cfg(test)]` |
| TOML shell semantics | `cargo test -p wasmsh-testkit` | `tests/suite/` |
| Standalone browser E2E | Playwright | `e2e/standalone/tests/` |
| Emscripten build contract | `node:test` | `e2e/build-contract/tests/` |
| Pyodide Node E2E | `node:test` | `e2e/pyodide-node/tests/` |
| Pyodide browser E2E | Playwright | `e2e/pyodide-browser/tests/` |
| Repo structure checks | `node:test` | `e2e/repo-checks/` |

## Consequences

- Integration bugs are caught before code review
- New features require proving they work in the real target environment
- Build times are longer (Pyodide build ~2 min cached) but failures are caught early
- Developers can skip Pyodide tests locally via `SKIP_PYODIDE=1`
