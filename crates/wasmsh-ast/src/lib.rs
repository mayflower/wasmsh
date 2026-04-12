//! AST types for the wasmsh shell.
//!
//! This crate defines the abstract syntax tree produced by the parser.
//! Words remain structured (no premature stringification) so that
//! expansion phases can operate on typed segments.

#![warn(missing_docs)]

use smol_str::SmolStr;

/// A span marking the byte range of a syntax element in source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Byte offset of the first character (inclusive).
    pub start: u32,
    /// Byte offset past the last character (exclusive).
    pub end: u32,
}

/// A complete shell program (list of commands).
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    /// The top-level complete commands in the program.
    pub commands: Vec<CompleteCommand>,
}

/// A complete command terminated by a newline or `;`.
#[derive(Debug, Clone, PartialEq)]
pub struct CompleteCommand {
    /// The and/or lists that make up this command.
    pub list: Vec<AndOrList>,
    /// Source span of the complete command.
    pub span: Span,
}

/// A chain of pipelines joined by `&&` or `||`.
#[derive(Debug, Clone, PartialEq)]
pub struct AndOrList {
    /// The first pipeline in the chain.
    pub first: Pipeline,
    /// Subsequent pipelines paired with their connecting operator.
    pub rest: Vec<(AndOrOp, Pipeline)>,
}

/// `&&` or `||` operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AndOrOp {
    /// `&&` — run the right side only if the left side succeeded.
    And,
    /// `||` — run the right side only if the left side failed.
    Or,
}

/// A pipeline of one or more commands connected by `|`.
#[derive(Debug, Clone, PartialEq)]
pub struct Pipeline {
    /// True when the pipeline is prefixed with `time`.
    pub timed: bool,
    /// True when `time -p` was used.
    pub time_posix: bool,
    /// True when the pipeline is prefixed with `!` (logical negation).
    pub negated: bool,
    /// The commands in the pipeline.
    pub commands: Vec<Command>,
    /// Per-stage flags: `pipe_stderr[i]` is true when stage `i` uses `|&`
    /// (its stderr should also be piped to the next stage's stdin).
    pub pipe_stderr: Vec<bool>,
}

/// A single command in the AST.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Command {
    /// A simple command with optional assignments, words, and redirections.
    Simple(SimpleCommand),
    /// A `( compound_list )` subshell.
    Subshell(SubshellCommand),
    /// A `{ compound_list ; }` brace group.
    Group(GroupCommand),
    /// An `if / elif / else / fi` construct.
    If(IfCommand),
    /// A `while condition; do body; done` loop.
    While(WhileCommand),
    /// An `until condition; do body; done` loop.
    Until(UntilCommand),
    /// A `for name in words; do body; done` loop.
    For(ForCommand),
    /// A C-style `for (( init; cond; step )) do body done` loop.
    ArithFor(ArithForCommand),
    /// A function definition.
    FunctionDef(FunctionDef),
    /// A `case word in ... esac` statement.
    Case(CaseCommand),
    /// A `[[ expression ]]` extended test.
    DoubleBracket(DoubleBracketCommand),
    /// A `(( expr ))` arithmetic command.
    ArithCommand(ArithCommandNode),
    /// A `select name in words; do body; done` menu loop.
    Select(SelectCommand),
}

/// A C-style `for (( init; cond; step )) do body done` command.
#[derive(Debug, Clone, PartialEq)]
pub struct ArithForCommand {
    /// The initializer expression.
    pub init: SmolStr,
    /// The loop condition expression.
    pub cond: SmolStr,
    /// The step expression evaluated after each iteration.
    pub step: SmolStr,
    /// The loop body.
    pub body: Vec<CompleteCommand>,
    /// Source span.
    pub span: Span,
}

/// A `(( expr ))` arithmetic command.
#[derive(Debug, Clone, PartialEq)]
pub struct ArithCommandNode {
    /// The arithmetic expression text.
    pub expr: SmolStr,
    /// Source span.
    pub span: Span,
}

/// A `select name [in word ...]; do body; done` command.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectCommand {
    /// The loop variable name.
    pub var_name: SmolStr,
    /// `None` means iterate over `"$@"` (no `in` clause).
    pub words: Option<Vec<Word>>,
    /// The loop body.
    pub body: Vec<CompleteCommand>,
    /// Trailing redirections (e.g., `done <<< "input"`).
    pub redirections: Vec<Redirection>,
    /// Source span.
    pub span: Span,
}

/// A `[[ expression ]]` extended test command.
#[derive(Debug, Clone, PartialEq)]
pub struct DoubleBracketCommand {
    /// The words inside `[[ ... ]]`.
    pub words: Vec<Word>,
    /// Source span.
    pub span: Span,
}

/// A subshell command `( compound_list )`.
#[derive(Debug, Clone, PartialEq)]
pub struct SubshellCommand {
    /// The commands inside the subshell.
    pub body: Vec<CompleteCommand>,
    /// Source span.
    pub span: Span,
}

/// A brace group `{ compound_list ; }`.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupCommand {
    /// The commands inside the brace group.
    pub body: Vec<CompleteCommand>,
    /// Source span.
    pub span: Span,
}

/// An `if` / `elif` / `else` / `fi` command.
#[derive(Debug, Clone, PartialEq)]
pub struct IfCommand {
    /// The condition commands.
    pub condition: Vec<CompleteCommand>,
    /// The body to run when the condition is true.
    pub then_body: Vec<CompleteCommand>,
    /// Zero or more `elif` clauses.
    pub elifs: Vec<ElifClause>,
    /// Optional `else` body.
    pub else_body: Option<Vec<CompleteCommand>>,
    /// Source span.
    pub span: Span,
}

/// A single `elif condition; then body` clause.
#[derive(Debug, Clone, PartialEq)]
pub struct ElifClause {
    /// The condition commands.
    pub condition: Vec<CompleteCommand>,
    /// The body to run when the condition is true.
    pub then_body: Vec<CompleteCommand>,
}

/// A `while condition; do body; done` command.
#[derive(Debug, Clone, PartialEq)]
pub struct WhileCommand {
    /// The loop condition.
    pub condition: Vec<CompleteCommand>,
    /// The loop body.
    pub body: Vec<CompleteCommand>,
    /// Source span.
    pub span: Span,
}

/// An `until condition; do body; done` command.
#[derive(Debug, Clone, PartialEq)]
pub struct UntilCommand {
    /// The loop condition (runs until this is true).
    pub condition: Vec<CompleteCommand>,
    /// The loop body.
    pub body: Vec<CompleteCommand>,
    /// Source span.
    pub span: Span,
}

/// A `for name in words; do body; done` command.
#[derive(Debug, Clone, PartialEq)]
pub struct ForCommand {
    /// The loop variable name.
    pub var_name: SmolStr,
    /// `None` means iterate over `"$@"` (no `in` clause).
    pub words: Option<Vec<Word>>,
    /// The loop body.
    pub body: Vec<CompleteCommand>,
    /// Source span.
    pub span: Span,
}

/// A `case word in pattern) body ;; ... esac` command.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseCommand {
    /// The word being tested.
    pub word: Word,
    /// The list of pattern arms.
    pub items: Vec<CaseItem>,
    /// Source span.
    pub span: Span,
}

/// Terminator for a case item arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseTerminator {
    /// `;;` — stop matching after this arm.
    Break,
    /// `;&` — fall through to the next arm's body unconditionally.
    Fallthrough,
    /// `;;&` — continue testing remaining patterns.
    ContinueTesting,
}

/// A single `pattern) body ;;` arm in a case statement.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseItem {
    /// One or more glob patterns for this arm.
    pub patterns: Vec<Word>,
    /// The body to execute when a pattern matches.
    pub body: Vec<CompleteCommand>,
    /// How to proceed after this arm executes.
    pub terminator: CaseTerminator,
}

/// A function definition: `name() body` or `function name body`.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionDef {
    /// The function name.
    pub name: SmolStr,
    /// The function body (typically a `Group` command).
    pub body: Box<Command>,
    /// Source span.
    pub span: Span,
}

/// A simple command: optional assignments, words (argv), and redirections.
#[derive(Debug, Clone, PartialEq)]
pub struct SimpleCommand {
    /// Variable assignments prefixed before the command (e.g., `FOO=1`).
    pub assignments: Vec<Assignment>,
    /// The command name and arguments.
    pub words: Vec<Word>,
    /// Redirections attached to this command.
    pub redirections: Vec<Redirection>,
    /// Source span.
    pub span: Span,
}

/// A variable assignment (`name=value`).
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    /// The variable name.
    pub name: SmolStr,
    /// The assigned value (`None` for `name=` with empty value).
    pub value: Option<Word>,
    /// Source span.
    pub span: Span,
}

/// A structured word composed of parts that preserve quoting and expansion boundaries.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    /// The constituent parts of this word.
    pub parts: Vec<WordPart>,
    /// Source span.
    pub span: Span,
}

/// A segment of a word — literals, quoted strings, expansions, etc.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
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
    /// `<(cmd)` process substitution (input). Stores the inner command text.
    ProcessSubstIn(SmolStr),
    /// `>(cmd)` process substitution (output). Stores the inner command text.
    ProcessSubstOut(SmolStr),
    // Glob and tilde expansion handled at runtime/expansion layers.
}

/// A redirection (`>`, `<`, `>>`, `<<`, etc.).
#[derive(Debug, Clone, PartialEq)]
pub struct Redirection {
    /// Explicit file descriptor number (e.g., `2>` has `fd = Some(2)`).
    pub fd: Option<u32>,
    /// The redirection operator.
    pub op: RedirectionOp,
    /// The target word (filename, fd number, or here-string content).
    pub target: Word,
    /// For here-doc redirections, the body content (filled in after the command line).
    pub here_doc_body: Option<HereDocBody>,
    /// Source span.
    pub span: Span,
}

/// Redirection operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RedirectionOp {
    /// `<`
    Input,
    /// `>`
    Output,
    /// `>>`
    Append,
    /// `>|`
    Clobber,
    /// `&>>`
    AppendBoth,
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
    /// The literal here-doc text (after delimiter stripping).
    pub content: SmolStr,
    /// Source span of the here-doc body.
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
