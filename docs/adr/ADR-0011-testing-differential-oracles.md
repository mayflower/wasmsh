# ADR-0011: Original Tests Plus Differential Oracles

## Status
Accepted (updated 2026-03)

## Context
Compatibility must be measurable without pulling GPL test corpora into the repository.

## Decision
The repository contains only original tests. Additionally, an optional differential harness can run locally/in CI against installed reference shells.

## Current State

960 tests total: 506 unit tests + 454 TOML integration tests.

### Unit Tests (506)
- **wasmsh-utils**: 400 unit tests covering all 86 utilities
- **wasmsh-parse**: Parser tests including property-based fuzzing (proptest)
- **wasmsh-lex**, **wasmsh-expand**, **wasmsh-vm**, etc.: Crate-specific tests

### TOML Integration Tests (454)
- Declarative `[[test]]` tables in TOML files
- Shell semantics, utility behavior, redirections, expansions
- **60 real-world integration tests** (rw01-rw60): Multi-tool pipelines from practice (CI/CD, log analysis, ETL pipelines, deployment automation, schema validation, crontab management, etc.)

### Property-Based Fuzzing
- `proptest` in `wasmsh-parse/tests/property_tests.rs`
- Lexer and parser never panic on arbitrary input
- Fuzz generators produce both syntactically structured and purely random inputs

### Benchmarks
- **Criterion benchmarks** for parser (`wasmsh-parse/benches/parse_bench.rs`), expansion (`wasmsh-expand/benches/expand_bench.rs`), and pipeline execution (`wasmsh-browser/benches/pipeline_bench.rs`)
- Regression detection via CI (optional)

## Consequences
- Clean provenance -- no test is copied from GPL projects
- More effort for test design
- Still high practical relevance through real-world scenarios
- Property tests ensure robustness against arbitrary inputs
- Benchmarks enable performance tracking across releases
