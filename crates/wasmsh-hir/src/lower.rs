//! Lowering from AST to HIR.

use wasmsh_ast as ast;

use crate::*;

/// Lower an AST `Program` into an HIR `HirProgram`.
pub fn lower(program: &ast::Program) -> HirProgram {
    HirProgram {
        items: program.commands.iter().map(lower_complete_command).collect(),
    }
}

fn lower_complete_command(cc: &ast::CompleteCommand) -> HirCompleteCommand {
    HirCompleteCommand {
        list: cc.list.iter().map(lower_and_or).collect(),
        span: cc.span,
    }
}

fn lower_and_or(and_or: &ast::AndOrList) -> HirAndOr {
    HirAndOr {
        first: lower_pipeline(&and_or.first),
        rest: and_or
            .rest
            .iter()
            .map(|(op, pipeline)| (lower_and_or_op(*op), lower_pipeline(pipeline)))
            .collect(),
    }
}

fn lower_and_or_op(op: ast::AndOrOp) -> HirAndOrOp {
    match op {
        ast::AndOrOp::And => HirAndOrOp::And,
        ast::AndOrOp::Or => HirAndOrOp::Or,
    }
}

fn lower_pipeline(pipeline: &ast::Pipeline) -> HirPipeline {
    HirPipeline {
        negated: pipeline.negated,
        commands: pipeline.commands.iter().map(lower_command).collect(),
    }
}

fn lower_command(cmd: &ast::Command) -> HirCommand {
    match cmd {
        ast::Command::Simple(sc) => lower_simple_command(sc),
        ast::Command::Subshell(sub) => HirCommand::Subshell(HirBlock {
            body: lower_body(&sub.body),
            span: sub.span,
        }),
        ast::Command::Group(grp) => HirCommand::Group(HirBlock {
            body: lower_body(&grp.body),
            span: grp.span,
        }),
        ast::Command::If(if_cmd) => HirCommand::If(lower_if(if_cmd)),
        ast::Command::While(w) => HirCommand::While(HirLoop {
            condition: lower_body(&w.condition),
            body: lower_body(&w.body),
            span: w.span,
        }),
        ast::Command::Until(u) => HirCommand::Until(HirLoop {
            condition: lower_body(&u.condition),
            body: lower_body(&u.body),
            span: u.span,
        }),
        ast::Command::For(f) => HirCommand::For(HirFor {
            var_name: f.var_name.clone(),
            words: f.words.clone(),
            body: lower_body(&f.body),
            span: f.span,
        }),
        ast::Command::FunctionDef(fd) => HirCommand::FunctionDef(HirFunctionDef {
            name: fd.name.clone(),
            body: Box::new(lower_command(&fd.body)),
            span: fd.span,
        }),
        ast::Command::Case(c) => HirCommand::Case(HirCase {
            word: c.word.clone(),
            items: c
                .items
                .iter()
                .map(|item| HirCaseItem {
                    patterns: item.patterns.clone(),
                    body: lower_body(&item.body),
                })
                .collect(),
            span: c.span,
        }),
    }
}

/// Normalize a simple command into one of three HIR forms:
/// - `Exec`: has at least one command word (argv)
/// - `Assign`: has assignments but no command words
/// - `RedirectOnly`: has redirections but no assignments or command words
fn lower_simple_command(sc: &ast::SimpleCommand) -> HirCommand {
    let assignments: Vec<HirAssignment> = sc.assignments.iter().map(lower_assignment).collect();
    let redirections: Vec<HirRedirection> = sc.redirections.iter().map(lower_redirection).collect();

    if !sc.words.is_empty() {
        // Command with argv (and optional env vars + redirections)
        HirCommand::Exec(HirExec {
            argv: sc.words.clone(),
            env: assignments,
            redirections,
            span: sc.span,
        })
    } else if !assignments.is_empty() {
        // Assignment-only
        HirCommand::Assign(HirAssign {
            assignments,
            redirections,
            span: sc.span,
        })
    } else {
        // Redirection-only
        HirCommand::RedirectOnly(HirRedirectOnly {
            redirections,
            span: sc.span,
        })
    }
}

fn lower_if(if_cmd: &ast::IfCommand) -> HirIf {
    HirIf {
        condition: lower_body(&if_cmd.condition),
        then_body: lower_body(&if_cmd.then_body),
        elifs: if_cmd
            .elifs
            .iter()
            .map(|elif| HirElif {
                condition: lower_body(&elif.condition),
                then_body: lower_body(&elif.then_body),
            })
            .collect(),
        else_body: if_cmd.else_body.as_ref().map(|b| lower_body(b)),
        span: if_cmd.span,
    }
}

fn lower_body(commands: &[ast::CompleteCommand]) -> Vec<HirCompleteCommand> {
    commands.iter().map(lower_complete_command).collect()
}

fn lower_assignment(a: &ast::Assignment) -> HirAssignment {
    HirAssignment {
        name: a.name.clone(),
        value: a.value.clone(),
        span: a.span,
    }
}

fn lower_redirection(r: &ast::Redirection) -> HirRedirection {
    HirRedirection {
        fd: r.fd,
        op: r.op,
        target: r.target.clone(),
        here_doc_body: r.here_doc_body.clone(),
        span: r.span,
    }
}
