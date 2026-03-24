//! High-level intermediate representation for the wasmsh shell.
//!
//! HIR normalizes AST quirks into execution-friendly structures:
//! - Assignment-only commands are distinguished from commands with argv
//! - Redirection-only commands are explicit
//! - Compound commands use HIR bodies, not AST bodies
//! - Spans and source references are preserved
//!
//! HIR reuses AST types for `Word`, `WordPart`, `RedirectionOp`,
//! `HereDocBody`, and `Span` since they need no transformation.

mod lower;

pub use lower::lower;

use smol_str::SmolStr;
use wasmsh_ast::{CaseTerminator, HereDocBody, RedirectionOp, Span, Word};

/// A lowered shell program.
#[derive(Debug, Clone, PartialEq)]
pub struct HirProgram {
    pub items: Vec<HirCompleteCommand>,
}

/// A complete command: one or more and/or lists (separated by `;`).
#[derive(Debug, Clone, PartialEq)]
pub struct HirCompleteCommand {
    pub list: Vec<HirAndOr>,
    pub span: Span,
}

/// A chain of pipelines joined by `&&` or `||`.
#[derive(Debug, Clone, PartialEq)]
pub struct HirAndOr {
    pub first: HirPipeline,
    pub rest: Vec<(HirAndOrOp, HirPipeline)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirAndOrOp {
    And,
    Or,
}

/// A pipeline of commands connected by `|`.
#[derive(Debug, Clone, PartialEq)]
pub struct HirPipeline {
    pub negated: bool,
    pub commands: Vec<HirCommand>,
    /// Per-stage flags: `pipe_stderr[i]` is true when stage `i` uses `|&`.
    pub pipe_stderr: Vec<bool>,
}

/// A normalized command.
#[derive(Debug, Clone, PartialEq)]
pub enum HirCommand {
    /// A command with argv, optional env-var prefixes, and redirections.
    Exec(HirExec),
    /// Assignment(s) without a command word — modifies the shell environment.
    Assign(HirAssign),
    /// Redirection(s) without a command word or assignments.
    RedirectOnly(HirRedirectOnly),
    /// `if / elif / else / fi`
    If(HirIf),
    /// `while condition; do body; done`
    While(HirLoop),
    /// `until condition; do body; done`
    Until(HirLoop),
    /// `for var in words; do body; done`
    For(HirFor),
    /// `( ... )`
    Subshell(HirBlock),
    /// `{ ... }`
    Group(HirBlock),
    /// Function definition.
    FunctionDef(HirFunctionDef),
    /// `case word in ... esac`
    Case(HirCase),
    /// `[[ expression ]]`
    DoubleBracket(HirDoubleBracket),
    /// C-style `for (( init; cond; step )) do body done`
    ArithFor(HirArithFor),
    /// `(( expr ))` arithmetic command
    ArithCommand(HirArithCommand),
    /// `select var in words; do body; done`
    Select(HirSelect),
}

/// Extended test command `[[ ... ]]`.
#[derive(Debug, Clone, PartialEq)]
pub struct HirDoubleBracket {
    pub words: Vec<Word>,
    pub span: Span,
}

/// C-style for loop `for (( init; cond; step )) do body done`.
#[derive(Debug, Clone, PartialEq)]
pub struct HirArithFor {
    pub init: SmolStr,
    pub cond: SmolStr,
    pub step: SmolStr,
    pub body: Vec<HirCompleteCommand>,
    pub span: Span,
}

/// Arithmetic command `(( expr ))`.
#[derive(Debug, Clone, PartialEq)]
pub struct HirArithCommand {
    pub expr: SmolStr,
    pub span: Span,
}

/// Select loop.
#[derive(Debug, Clone, PartialEq)]
pub struct HirSelect {
    pub var_name: SmolStr,
    pub words: Option<Vec<Word>>,
    pub body: Vec<HirCompleteCommand>,
    pub redirections: Vec<HirRedirection>,
    pub span: Span,
}

/// A command to execute with its argv, environment overrides, and redirections.
#[derive(Debug, Clone, PartialEq)]
pub struct HirExec {
    pub argv: Vec<Word>,
    pub env: Vec<HirAssignment>,
    pub redirections: Vec<HirRedirection>,
    pub span: Span,
}

/// Shell variable assignment(s) without a command.
#[derive(Debug, Clone, PartialEq)]
pub struct HirAssign {
    pub assignments: Vec<HirAssignment>,
    pub redirections: Vec<HirRedirection>,
    pub span: Span,
}

/// Redirection(s) without a command word.
#[derive(Debug, Clone, PartialEq)]
pub struct HirRedirectOnly {
    pub redirections: Vec<HirRedirection>,
    pub span: Span,
}

/// A variable assignment.
#[derive(Debug, Clone, PartialEq)]
pub struct HirAssignment {
    pub name: SmolStr,
    pub value: Option<Word>,
    pub span: Span,
}

/// A redirection in HIR form.
#[derive(Debug, Clone, PartialEq)]
pub struct HirRedirection {
    pub fd: Option<u32>,
    pub op: RedirectionOp,
    pub target: Word,
    pub here_doc_body: Option<HereDocBody>,
    pub span: Span,
}

/// If / elif / else construct.
#[derive(Debug, Clone, PartialEq)]
pub struct HirIf {
    pub condition: Vec<HirCompleteCommand>,
    pub then_body: Vec<HirCompleteCommand>,
    pub elifs: Vec<HirElif>,
    pub else_body: Option<Vec<HirCompleteCommand>>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HirElif {
    pub condition: Vec<HirCompleteCommand>,
    pub then_body: Vec<HirCompleteCommand>,
}

/// While or until loop (same structure).
#[derive(Debug, Clone, PartialEq)]
pub struct HirLoop {
    pub condition: Vec<HirCompleteCommand>,
    pub body: Vec<HirCompleteCommand>,
    pub span: Span,
}

/// For loop.
#[derive(Debug, Clone, PartialEq)]
pub struct HirFor {
    pub var_name: SmolStr,
    pub words: Option<Vec<Word>>,
    pub body: Vec<HirCompleteCommand>,
    pub span: Span,
}

/// A block of commands (subshell or group).
#[derive(Debug, Clone, PartialEq)]
pub struct HirBlock {
    pub body: Vec<HirCompleteCommand>,
    pub span: Span,
}

/// Function definition.
#[derive(Debug, Clone, PartialEq)]
pub struct HirFunctionDef {
    pub name: SmolStr,
    pub body: Box<HirCommand>,
    pub span: Span,
}

/// Case statement.
#[derive(Debug, Clone, PartialEq)]
pub struct HirCase {
    pub word: Word,
    pub items: Vec<HirCaseItem>,
    pub span: Span,
}

/// A single arm of a case statement.
#[derive(Debug, Clone, PartialEq)]
pub struct HirCaseItem {
    pub patterns: Vec<Word>,
    pub body: Vec<HirCompleteCommand>,
    pub terminator: CaseTerminator,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lower_source(source: &str) -> HirProgram {
        let ast = wasmsh_parse::parse(source).unwrap();
        lower(&ast)
    }

    fn first_cmd(source: &str) -> HirCommand {
        let hir = lower_source(source);
        hir.items[0].list[0].first.commands[0].clone()
    }

    // ---- Simple commands ----

    #[test]
    fn exec_command() {
        let cmd = first_cmd("echo hello");
        let HirCommand::Exec(exec) = cmd else {
            panic!("expected Exec");
        };
        assert_eq!(exec.argv.len(), 2);
        assert!(exec.env.is_empty());
    }

    #[test]
    fn exec_with_env() {
        let cmd = first_cmd("FOO=1 BAR=2 env");
        let HirCommand::Exec(exec) = cmd else {
            panic!("expected Exec");
        };
        assert_eq!(exec.argv.len(), 1);
        assert_eq!(exec.env.len(), 2);
        assert_eq!(exec.env[0].name.as_str(), "FOO");
    }

    #[test]
    fn assign_only() {
        let cmd = first_cmd("FOO=bar");
        let HirCommand::Assign(assign) = cmd else {
            panic!("expected Assign, got {cmd:?}");
        };
        assert_eq!(assign.assignments.len(), 1);
        assert_eq!(assign.assignments[0].name.as_str(), "FOO");
    }

    #[test]
    fn redirect_only() {
        let cmd = first_cmd("> file");
        let HirCommand::RedirectOnly(ro) = cmd else {
            panic!("expected RedirectOnly");
        };
        assert_eq!(ro.redirections.len(), 1);
    }

    #[test]
    fn exec_with_redirections() {
        let cmd = first_cmd("cat < in > out");
        let HirCommand::Exec(exec) = cmd else {
            panic!("expected Exec");
        };
        assert_eq!(exec.argv.len(), 1);
        assert_eq!(exec.redirections.len(), 2);
    }

    // ---- Pipelines ----

    #[test]
    fn pipeline_lowered() {
        let hir = lower_source("a | b | c");
        let pipeline = &hir.items[0].list[0].first;
        assert_eq!(pipeline.commands.len(), 3);
        assert!(!pipeline.negated);
    }

    #[test]
    fn negated_pipeline() {
        let hir = lower_source("! a | b");
        assert!(hir.items[0].list[0].first.negated);
    }

    // ---- And/Or ----

    #[test]
    fn and_or_lowered() {
        let hir = lower_source("a && b || c");
        let and_or = &hir.items[0].list[0];
        assert_eq!(and_or.rest.len(), 2);
        assert_eq!(and_or.rest[0].0, HirAndOrOp::And);
        assert_eq!(and_or.rest[1].0, HirAndOrOp::Or);
    }

    // ---- Compound commands ----

    #[test]
    fn if_lowered() {
        let cmd = first_cmd("if true; then echo yes; else echo no; fi");
        let HirCommand::If(if_cmd) = cmd else {
            panic!("expected If");
        };
        assert_eq!(if_cmd.condition.len(), 1);
        assert_eq!(if_cmd.then_body.len(), 1);
        assert!(if_cmd.else_body.is_some());
    }

    #[test]
    fn while_lowered() {
        let cmd = first_cmd("while true; do echo loop; done");
        assert!(matches!(cmd, HirCommand::While(_)));
    }

    #[test]
    fn for_lowered() {
        let cmd = first_cmd("for x in a b; do echo x; done");
        let HirCommand::For(f) = cmd else {
            panic!("expected For");
        };
        assert_eq!(f.var_name.as_str(), "x");
        assert_eq!(f.words.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn subshell_lowered() {
        let cmd = first_cmd("(echo hi)");
        assert!(matches!(cmd, HirCommand::Subshell(_)));
    }

    #[test]
    fn group_lowered() {
        let cmd = first_cmd("{ echo hi; }");
        assert!(matches!(cmd, HirCommand::Group(_)));
    }

    #[test]
    fn function_def_lowered() {
        let cmd = first_cmd("greet() { echo hello; }");
        let HirCommand::FunctionDef(fd) = cmd else {
            panic!("expected FunctionDef");
        };
        assert_eq!(fd.name.as_str(), "greet");
        assert!(matches!(*fd.body, HirCommand::Group(_)));
    }

    // ---- Multiple commands ----

    #[test]
    fn semicolon_list() {
        let hir = lower_source("echo a; echo b");
        assert_eq!(hir.items[0].list.len(), 2);
    }

    #[test]
    fn multiline() {
        let hir = lower_source("echo a\necho b\necho c");
        assert_eq!(hir.items.len(), 3);
    }

    #[test]
    fn arith_command_lowered() {
        let cmd = first_cmd("(( 1 + 2 ))");
        let HirCommand::ArithCommand(ac) = cmd else {
            panic!("expected ArithCommand, got {cmd:?}");
        };
        assert_eq!(ac.expr.as_str(), "1 + 2");
    }

    #[test]
    fn arith_for_lowered() {
        let cmd = first_cmd("for ((i=0; i<5; i++)) do echo $i; done");
        let HirCommand::ArithFor(af) = cmd else {
            panic!("expected ArithFor, got {cmd:?}");
        };
        assert_eq!(af.init.as_str(), "i=0");
        assert_eq!(af.cond.as_str(), "i<5");
        assert_eq!(af.step.as_str(), "i++");
        assert!(!af.body.is_empty());
    }
}
