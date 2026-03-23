//! Test utilities, TOML test runner, and compatibility harness for wasmsh.
//!
//! This crate provides:
//! - `runner`: TOML-based declarative test runner
//! - `toml_case`: Test case schema (serde-deserializable)
//! - `features`: Feature gate registry
//! - `oracle`: Reference shell comparison (opt-in)
//! - `compat`: Legacy compatibility case format

pub mod compat;
pub mod features;
pub mod oracle;
pub mod runner;
pub mod toml_case;

/// Parse source and assert it produces a valid AST (no parse errors).
pub fn assert_parses(source: &str) {
    wasmsh_parse::parse(source).expect("expected successful parse");
}

/// Parse source and assert it produces a parse error.
pub fn assert_parse_error(source: &str) {
    assert!(
        wasmsh_parse::parse(source).is_err(),
        "expected parse error for: {source:?}"
    );
}

/// Parse source, lower to HIR, and return the HIR program.
pub fn parse_and_lower(source: &str) -> wasmsh_hir::HirProgram {
    let ast = wasmsh_parse::parse(source).expect("parse failed");
    wasmsh_hir::lower(&ast)
}
