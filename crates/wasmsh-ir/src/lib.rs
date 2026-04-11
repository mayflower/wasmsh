//! Linear instruction representation for the wasmsh VM subset.
//!
//! `wasmsh-runtime` still executes the full shell by interpreting HIR
//! directly. This crate currently models only the supported subset that
//! can be lowered into `wasmsh-vm`: scalar assignments, builtin command
//! execution, selected redirections, and top-level `&&` / `||`
//! short-circuiting.

use smol_str::SmolStr;
use wasmsh_ast::{HereDocBody, RedirectionOp, Word, WordPart};
use wasmsh_hir::{HirAndOr, HirAndOrOp, HirCommand, HirPipeline, HirRedirection};

/// A single IR instruction for the VM.
#[derive(Debug, Clone, PartialEq)]
pub enum Ir {
    /// Set a shell variable from a shell word.
    Assign {
        name: SmolStr,
        value: Option<Word>,
    },
    /// Invoke a builtin command with its argv and redirection plan.
    ExecuteBuiltin {
        name: SmolStr,
        argv: Vec<Word>,
        redirections: Vec<IrRedirection>,
    },
    /// Skip the following pipeline when the previous one failed.
    JumpIfFailure { target: usize },
    /// Skip the following pipeline when the previous one succeeded.
    JumpIfSuccess { target: usize },
    /// Return the current shell status.
    ReturnLastStatus,
    /// Set exit status and halt.
    Return { status: i32 },
    /// No operation (used for padding / debugging).
    Nop,
}

/// A compiled program: a sequence of IR instructions.
#[derive(Debug, Clone, PartialEq)]
pub struct IrProgram {
    pub instructions: Vec<Ir>,
}

impl IrProgram {
    #[must_use]
    pub fn new(instructions: Vec<Ir>) -> Self {
        Self { instructions }
    }
}

/// Redirection plan attached to an IR builtin command.
#[derive(Debug, Clone, PartialEq)]
pub struct IrRedirection {
    pub fd: Option<u32>,
    pub op: RedirectionOp,
    pub target: Word,
    pub here_doc_body: Option<HereDocBody>,
}

/// Explicit reason why HIR cannot be lowered into the current VM subset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoweringError {
    Unsupported(&'static str),
}

impl std::fmt::Display for LoweringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for LoweringError {}

pub fn lower_supported_and_or(and_or: &HirAndOr) -> Result<IrProgram, LoweringError> {
    let mut instructions = Vec::new();
    lower_supported_pipeline(&and_or.first, &mut instructions)?;

    for (op, pipeline) in &and_or.rest {
        let jump_index = instructions.len();
        instructions.push(match op {
            HirAndOrOp::And => Ir::JumpIfFailure { target: usize::MAX },
            HirAndOrOp::Or => Ir::JumpIfSuccess { target: usize::MAX },
        });
        lower_supported_pipeline(pipeline, &mut instructions)?;
        let target = instructions.len();
        match &mut instructions[jump_index] {
            Ir::JumpIfFailure { target: patched } | Ir::JumpIfSuccess { target: patched } => {
                *patched = target;
            }
            _ => unreachable!("jump placeholder must remain a jump"),
        }
    }

    instructions.push(Ir::ReturnLastStatus);
    Ok(IrProgram::new(instructions))
}

fn lower_supported_pipeline(
    pipeline: &HirPipeline,
    instructions: &mut Vec<Ir>,
) -> Result<(), LoweringError> {
    if pipeline.negated {
        return Err(LoweringError::Unsupported(
            "negated pipelines are outside the VM subset",
        ));
    }
    if pipeline.commands.len() != 1 {
        return Err(LoweringError::Unsupported(
            "multi-stage pipelines are outside the VM subset",
        ));
    }

    lower_supported_command(&pipeline.commands[0], instructions)
}

fn lower_supported_command(
    cmd: &HirCommand,
    instructions: &mut Vec<Ir>,
) -> Result<(), LoweringError> {
    match cmd {
        HirCommand::Assign(assign) => {
            if !assign.redirections.is_empty() {
                return Err(LoweringError::Unsupported(
                    "assignment redirections are outside the VM subset",
                ));
            }
            for assignment in &assign.assignments {
                instructions.push(Ir::Assign {
                    name: assignment.name.clone(),
                    value: assignment.value.clone(),
                });
            }
            Ok(())
        }
        HirCommand::Exec(exec) => {
            if !exec.env.is_empty() {
                return Err(LoweringError::Unsupported(
                    "command env prefixes are outside the VM subset",
                ));
            }
            let Some(name) = literal_word_text(exec.argv.first()) else {
                return Err(LoweringError::Unsupported(
                    "builtin name must be a literal word in the VM subset",
                ));
            };
            instructions.push(Ir::ExecuteBuiltin {
                name,
                argv: exec.argv.clone(),
                redirections: exec.redirections.iter().map(IrRedirection::from).collect(),
            });
            Ok(())
        }
        _ => Err(LoweringError::Unsupported(
            "command kind is outside the VM subset",
        )),
    }
}

fn literal_word_text(word: Option<&Word>) -> Option<SmolStr> {
    let word = word?;
    let mut text = String::new();
    for part in &word.parts {
        append_literal_part(part, &mut text)?;
    }
    Some(text.into())
}

fn append_literal_part(part: &WordPart, text: &mut String) -> Option<()> {
    match part {
        WordPart::Literal(segment) | WordPart::SingleQuoted(segment) => {
            text.push_str(segment);
            Some(())
        }
        WordPart::DoubleQuoted(parts) => {
            for inner in parts {
                append_literal_part(inner, text)?;
            }
            Some(())
        }
        _ => None,
    }
}

impl From<&HirRedirection> for IrRedirection {
    fn from(value: &HirRedirection) -> Self {
        Self {
            fd: value.fd,
            op: value.op,
            target: value.target.clone(),
            here_doc_body: value.here_doc_body.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmsh_ast::{Span, WordPart};
    use wasmsh_hir::lower;

    #[test]
    fn ir_program_construction() {
        let prog = IrProgram::new(vec![
            Ir::ExecuteBuiltin {
                name: "echo".into(),
                argv: vec![literal_word("echo"), literal_word("hello")],
                redirections: Vec::new(),
            },
            Ir::ReturnLastStatus,
        ]);
        assert_eq!(prog.instructions.len(), 2);
    }

    #[test]
    fn lowers_assignment_then_builtin_exec() {
        let program = lower_supported_and_or(&first_and_or("FOO=bar; echo hello"))
            .expect("simple subset should lower");
        assert!(matches!(
            program.instructions.as_slice(),
            [Ir::Assign { name, value: Some(word) }, Ir::ReturnLastStatus]
                if name == "FOO" && word_text(word) == "bar"
        ));
        let program = lower_supported_and_or(&second_and_or("FOO=bar; echo hello"))
            .expect("simple subset should lower");
        assert!(matches!(
            program.instructions.as_slice(),
            [Ir::ExecuteBuiltin {
                name,
                argv,
                redirections
            }, Ir::ReturnLastStatus]
                if name == "echo"
                    && redirections.is_empty()
                    && argv.iter().map(word_text).collect::<Vec<_>>() == vec!["echo", "hello"]
        ));
    }

    #[test]
    fn lowers_short_circuit_chain() {
        let program = lower_supported_and_or(&first_and_or("true && echo ok"))
            .expect("and/or subset should lower");
        assert!(matches!(
            program.instructions.as_slice(),
            [
                Ir::ExecuteBuiltin {
                    name: first_name,
                    argv: first_argv,
                    redirections: first_redirections
                },
                Ir::JumpIfFailure { target: 3 },
                Ir::ExecuteBuiltin {
                    name: second_name,
                    argv: second_argv,
                    redirections: second_redirections
                },
                Ir::ReturnLastStatus
            ]
                if first_name == "true"
                    && first_redirections.is_empty()
                    && first_argv.iter().map(word_text).collect::<Vec<_>>() == vec!["true"]
                    && second_name == "echo"
                    && second_redirections.is_empty()
                    && second_argv.iter().map(word_text).collect::<Vec<_>>() == vec!["echo", "ok"]
        ));
    }

    #[test]
    fn lowers_builtin_with_stdout_redirection() {
        let program = lower_supported_and_or(&first_and_or("echo hello > /out.txt"))
            .expect("redirected builtin should lower");
        assert!(matches!(
            program.instructions.as_slice(),
            [Ir::ExecuteBuiltin {
                name,
                argv,
                redirections
            }, Ir::ReturnLastStatus]
                if name == "echo"
                    && argv.iter().map(word_text).collect::<Vec<_>>() == vec!["echo", "hello"]
                    && redirections.len() == 1
                    && redirections[0].op == RedirectionOp::Output
                    && word_text(&redirections[0].target) == "/out.txt"
        ));
    }

    #[test]
    fn rejects_multi_stage_pipeline_in_vm_subset() {
        let err = lower_supported_and_or(&first_and_or("echo hello | cat")).unwrap_err();
        assert_eq!(
            err,
            LoweringError::Unsupported("multi-stage pipelines are outside the VM subset")
        );
    }

    fn first_and_or(source: &str) -> HirAndOr {
        let ast = wasmsh_parse::parse(source).unwrap();
        let hir = lower(&ast);
        hir.items[0].list[0].clone()
    }

    fn second_and_or(source: &str) -> HirAndOr {
        let ast = wasmsh_parse::parse(source).unwrap();
        let hir = lower(&ast);
        hir.items[0].list[1].clone()
    }

    fn literal_word(text: &str) -> Word {
        Word {
            parts: vec![WordPart::Literal(text.into())],
            span: Span { start: 0, end: 0 },
        }
    }

    fn word_text(word: &Word) -> String {
        word.parts
            .iter()
            .map(|part| match part {
                WordPart::Literal(text) => text.as_str(),
                _ => panic!("expected literal word part"),
            })
            .collect()
    }
}
