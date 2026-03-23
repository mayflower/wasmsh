//! Property tests for the wasmsh parser.
//!
//! These tests verify that the parser never panics on arbitrary input
//! and that valid shell constructs always parse successfully.

use proptest::prelude::*;

// The parser must never panic on arbitrary byte strings.
// It may return Ok or Err, but must not crash.
proptest! {
    #[test]
    fn parse_never_panics(input in "\\PC{0,256}") {
        let _ = wasmsh_parse::parse(&input);
    }
}

// The lexer must never panic on arbitrary input.
proptest! {
    #[test]
    fn lex_never_panics(input in "\\PC{0,256}") {
        let _ = wasmsh_lex::tokenize(&input);
    }
}

const RESERVED: &[&str] = &[
    "if", "then", "else", "elif", "fi", "do", "done", "case", "esac", "while", "until", "for",
    "in", "function", "select", "time",
];

/// Strategy for generating a non-reserved word.
fn shell_word() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9]{0,7}".prop_filter("not a reserved word", |w| !RESERVED.contains(&w.as_str()))
}

/// Strategy for generating simple valid commands.
fn simple_command() -> impl Strategy<Value = String> {
    prop::collection::vec(shell_word(), 1..5).prop_map(|words| words.join(" "))
}

/// Strategy for generating pipelines.
fn pipeline() -> impl Strategy<Value = String> {
    prop::collection::vec(simple_command(), 1..4).prop_map(|cmds| cmds.join(" | "))
}

/// Strategy for generating and/or chains.
fn and_or_chain() -> impl Strategy<Value = String> {
    let ops = prop_oneof!["&&", "||"];
    (pipeline(), prop::collection::vec((ops, pipeline()), 0..3)).prop_map(|(first, rest)| {
        let mut result = first;
        for (op, cmd) in rest {
            result.push(' ');
            result.push_str(&op);
            result.push(' ');
            result.push_str(&cmd);
        }
        result
    })
}

proptest! {
    #[test]
    fn valid_simple_commands_parse(cmd in simple_command()) {
        let result = wasmsh_parse::parse(&cmd);
        prop_assert!(result.is_ok(), "Failed to parse valid command: {cmd}");
    }

    #[test]
    fn valid_pipelines_parse(cmd in pipeline()) {
        let result = wasmsh_parse::parse(&cmd);
        prop_assert!(result.is_ok(), "Failed to parse valid pipeline: {cmd}");
    }

    #[test]
    fn valid_and_or_chains_parse(cmd in and_or_chain()) {
        let result = wasmsh_parse::parse(&cmd);
        prop_assert!(result.is_ok(), "Failed to parse valid and/or chain: {cmd}");
    }

    #[test]
    fn valid_assignments_parse(
        name in "[A-Z][A-Z0-9_]{0,5}",
        value in "[a-z0-9]{0,10}",
    ) {
        let input = format!("{name}={value}");
        let result = wasmsh_parse::parse(&input);
        prop_assert!(result.is_ok(), "Failed to parse valid assignment: {input}");
    }

    #[test]
    fn valid_if_commands_parse(
        cond in simple_command(),
        body in simple_command(),
    ) {
        let input = format!("if {cond}; then {body}; fi");
        let result = wasmsh_parse::parse(&input);
        prop_assert!(result.is_ok(), "Failed to parse valid if: {input}");
    }

    #[test]
    fn valid_for_loops_parse(
        var in shell_word(),
        words in prop::collection::vec(shell_word(), 1..5),
        body in simple_command(),
    ) {
        let word_list = words.join(" ");
        let input = format!("for {var} in {word_list}; do {body}; done");
        let result = wasmsh_parse::parse(&input);
        prop_assert!(result.is_ok(), "Failed to parse valid for: {input}");
    }

    #[test]
    fn valid_while_loops_parse(
        cond in simple_command(),
        body in simple_command(),
    ) {
        let input = format!("while {cond}; do {body}; done");
        let result = wasmsh_parse::parse(&input);
        prop_assert!(result.is_ok(), "Failed to parse valid while: {input}");
    }

    #[test]
    fn valid_functions_parse(
        name in shell_word(),
        body in simple_command(),
    ) {
        let input = format!("{name}() {{ {body}; }}");
        let result = wasmsh_parse::parse(&input);
        prop_assert!(result.is_ok(), "Failed to parse valid function: {input}");
    }
}
