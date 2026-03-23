//! AST types for the wasmsh shell.
//!
//! This crate defines the abstract syntax tree produced by the parser.
//! Words remain structured (no premature stringification) so that
//! expansion phases can operate on typed segments.

use smol_str::SmolStr;

/// A span marking the byte range of a syntax element in source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// A complete shell program (list of commands).
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub commands: Vec<CompleteCommand>,
}

/// A complete command terminated by a newline or `;`.
#[derive(Debug, Clone, PartialEq)]
pub struct CompleteCommand {
    pub list: Vec<AndOrList>,
    pub span: Span,
}

/// A chain of pipelines joined by `&&` or `||`.
#[derive(Debug, Clone, PartialEq)]
pub struct AndOrList {
    pub first: Pipeline,
    pub rest: Vec<(AndOrOp, Pipeline)>,
}

/// `&&` or `||` operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AndOrOp {
    And,
    Or,
}

/// A pipeline of one or more commands connected by `|`.
#[derive(Debug, Clone, PartialEq)]
pub struct Pipeline {
    pub negated: bool,
    pub commands: Vec<Command>,
}

/// A single command in the AST.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Simple(SimpleCommand),
    Subshell(SubshellCommand),
    Group(GroupCommand),
    If(IfCommand),
    While(WhileCommand),
    Until(UntilCommand),
    For(ForCommand),
    FunctionDef(FunctionDef),
    Case(CaseCommand),
}

/// A subshell command `( compound_list )`.
#[derive(Debug, Clone, PartialEq)]
pub struct SubshellCommand {
    pub body: Vec<CompleteCommand>,
    pub span: Span,
}

/// A brace group `{ compound_list ; }`.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupCommand {
    pub body: Vec<CompleteCommand>,
    pub span: Span,
}

/// An `if` / `elif` / `else` / `fi` command.
#[derive(Debug, Clone, PartialEq)]
pub struct IfCommand {
    pub condition: Vec<CompleteCommand>,
    pub then_body: Vec<CompleteCommand>,
    pub elifs: Vec<ElifClause>,
    pub else_body: Option<Vec<CompleteCommand>>,
    pub span: Span,
}

/// A single `elif condition; then body` clause.
#[derive(Debug, Clone, PartialEq)]
pub struct ElifClause {
    pub condition: Vec<CompleteCommand>,
    pub then_body: Vec<CompleteCommand>,
}

/// A `while condition; do body; done` command.
#[derive(Debug, Clone, PartialEq)]
pub struct WhileCommand {
    pub condition: Vec<CompleteCommand>,
    pub body: Vec<CompleteCommand>,
    pub span: Span,
}

/// An `until condition; do body; done` command.
#[derive(Debug, Clone, PartialEq)]
pub struct UntilCommand {
    pub condition: Vec<CompleteCommand>,
    pub body: Vec<CompleteCommand>,
    pub span: Span,
}

/// A `for name in words; do body; done` command.
#[derive(Debug, Clone, PartialEq)]
pub struct ForCommand {
    pub var_name: SmolStr,
    /// `None` means iterate over `"$@"` (no `in` clause).
    pub words: Option<Vec<Word>>,
    pub body: Vec<CompleteCommand>,
    pub span: Span,
}

/// A `case word in pattern) body ;; ... esac` command.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseCommand {
    pub word: Word,
    pub items: Vec<CaseItem>,
    pub span: Span,
}

/// A single `pattern) body ;;` arm in a case statement.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseItem {
    pub patterns: Vec<Word>,
    pub body: Vec<CompleteCommand>,
}

/// A function definition: `name() body` or `function name body`.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionDef {
    pub name: SmolStr,
    pub body: Box<Command>,
    pub span: Span,
}

/// A simple command: optional assignments, words (argv), and redirections.
#[derive(Debug, Clone, PartialEq)]
pub struct SimpleCommand {
    pub assignments: Vec<Assignment>,
    pub words: Vec<Word>,
    pub redirections: Vec<Redirection>,
    pub span: Span,
}

/// A variable assignment (`name=value`).
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub name: SmolStr,
    pub value: Option<Word>,
    pub span: Span,
}

/// A structured word composed of parts that preserve quoting and expansion boundaries.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub parts: Vec<WordPart>,
    pub span: Span,
}

/// A segment of a word — literals, quoted strings, expansions, etc.
#[derive(Debug, Clone, PartialEq)]
pub enum WordPart {
    /// Unquoted literal text.
    Literal(SmolStr),
    /// Content inside single quotes.
    SingleQuoted(SmolStr),
    /// Content inside double quotes (may contain nested expansions).
    DoubleQuoted(Vec<WordPart>),
    /// `$name` or `${...}` parameter expansion. Stores the name or full
    /// expansion text (e.g. `"var"` for `$var`, `"var:-default"` for `${var:-default}`).
    Parameter(SmolStr),
    /// `$(...)` command substitution. Stores the inner source text (not yet parsed).
    CommandSubstitution(SmolStr),
    /// `$((...))` arithmetic expansion. Stores the inner expression text.
    Arithmetic(SmolStr),
    // Glob and tilde expansion handled at runtime/expansion layers.
}

/// A redirection (`>`, `<`, `>>`, `<<`, etc.).
#[derive(Debug, Clone, PartialEq)]
pub struct Redirection {
    pub fd: Option<u32>,
    pub op: RedirectionOp,
    pub target: Word,
    /// For here-doc redirections, the body content (filled in after the command line).
    pub here_doc_body: Option<HereDocBody>,
    pub span: Span,
}

/// Redirection operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectionOp {
    /// `<`
    Input,
    /// `>`
    Output,
    /// `>>`
    Append,
    /// `<>`
    ReadWrite,
    /// `<<` (here-doc)
    HereDoc,
    /// `<<-` (here-doc with tab stripping)
    HereDocStrip,
    /// `<<<` (here-string)
    HereString,
    /// `>&N` or `N>&M` (duplicate output fd)
    DupOutput,
    /// `<&N` or `N<&M` (duplicate input fd)
    DupInput,
}

/// The body of a here-document.
#[derive(Debug, Clone, PartialEq)]
pub struct HereDocBody {
    pub content: SmolStr,
    pub span: Span,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_equality() {
        let a = Span { start: 0, end: 5 };
        let b = Span { start: 0, end: 5 };
        assert_eq!(a, b);
    }

    #[test]
    fn word_with_parts() {
        let word = Word {
            parts: vec![
                WordPart::Literal("hello".into()),
                WordPart::Parameter("USER".into()),
            ],
            span: Span { start: 0, end: 11 },
        };
        assert_eq!(word.parts.len(), 2);
    }
}
