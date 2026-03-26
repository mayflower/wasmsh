//! Handwritten recursive-descent parser for the wasmsh shell.
//!
//! Consumes tokens from `wasmsh-lex` and produces an AST defined
//! in `wasmsh-ast`. No parser generators are used.

mod word_parser;

use std::collections::VecDeque;

use wasmsh_ast::{
    AndOrList, AndOrOp, ArithCommandNode, ArithForCommand, Assignment, CaseCommand, CaseItem,
    CaseTerminator, Command, CompleteCommand, DoubleBracketCommand, ElifClause, ForCommand,
    FunctionDef, GroupCommand, HereDocBody, IfCommand, Pipeline, Program, Redirection,
    RedirectionOp, SelectCommand, SimpleCommand, Span, SubshellCommand, UntilCommand, WhileCommand,
    Word, WordPart,
};
use wasmsh_lex::{Lexer, Token, TokenKind};

/// Parse errors with span information.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub offset: u32,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "parse error at {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Parse a complete shell source string into a `Program` AST.
pub fn parse(source: &str) -> Result<Program, ParseError> {
    let mut parser = Parser::new(source)?;
    parser.parse_program()
}

// Words that terminate compound-list bodies (not command-starters).
const TERMINATOR_WORDS: &[&str] = &["then", "elif", "else", "fi", "do", "done", "esac", "}"];

/// A pending here-doc that needs its body read after the command line.
struct PendingHereDoc {
    delimiter: String,
    strip_tabs: bool,
}

struct Parser<'src> {
    source: &'src str,
    lexer: Lexer<'src>,
    current: Token,
    peeked: VecDeque<Token>,
    prev_end: u32,
    pending_heredocs: Vec<PendingHereDoc>,
}

impl<'src> Parser<'src> {
    fn new(source: &'src str) -> Result<Self, ParseError> {
        let mut lexer = Lexer::new(source);
        let current = lexer.next_token().map_err(lex_err)?;
        Ok(Self {
            source,
            lexer,
            current,
            peeked: VecDeque::new(),
            prev_end: 0,
            pending_heredocs: Vec::new(),
        })
    }

    fn advance(&mut self) -> Result<Token, ParseError> {
        let prev = self.current.clone();
        self.prev_end = prev.span.end;
        self.current = if let Some(tok) = self.peeked.pop_front() {
            tok
        } else {
            self.lexer.next_token().map_err(lex_err)?
        };
        Ok(prev)
    }

    /// Peek at the nth token ahead (0 = next token after current).
    fn peek_nth(&mut self, n: usize) -> Result<&Token, ParseError> {
        while self.peeked.len() <= n {
            self.peeked
                .push_back(self.lexer.next_token().map_err(lex_err)?);
        }
        Ok(&self.peeked[n])
    }

    fn at(&self, kind: &TokenKind) -> bool {
        self.current.kind == *kind
    }

    fn at_word(&self) -> bool {
        matches!(self.current.kind, TokenKind::Word { .. })
    }

    fn at_word_eq(&self, text: &str) -> bool {
        self.at_word() && self.current_text() == text
    }

    fn at_redirection(&self) -> bool {
        matches!(
            self.current.kind,
            TokenKind::Less
                | TokenKind::Greater
                | TokenKind::GreaterGreater
                | TokenKind::LessLess
                | TokenKind::LessLessDash
                | TokenKind::LessLessLess
                | TokenKind::LessGreater
                | TokenKind::AmpGreater
        )
    }

    /// Check if the current word token is a single digit and the next token
    /// is a redirection operator. If so, this is an fd-prefix redirection.
    fn at_fd_prefix_redirection(&mut self) -> bool {
        if !self.at_word() {
            return false;
        }
        let text = self.current_text();
        if text.len() != 1 || !text.as_bytes()[0].is_ascii_digit() {
            return false;
        }
        if let Ok(next) = self.peek_nth(0) {
            matches!(
                next.kind,
                TokenKind::Less
                    | TokenKind::Greater
                    | TokenKind::GreaterGreater
                    | TokenKind::LessLess
                    | TokenKind::LessLessDash
                    | TokenKind::LessLessLess
                    | TokenKind::LessGreater
            )
        } else {
            false
        }
    }

    /// Check if the next token (after current) is `LParen`.
    fn peek_is_lparen(&mut self) -> bool {
        matches!(self.peek_nth(0), Ok(tok) if tok.kind == TokenKind::LParen)
    }

    /// Parse a word like `arr=(x y z)` where the current token is `arr=`
    /// and the next token is `LParen`. Combines the name= with compound value.
    fn parse_compound_assign_word(&mut self) -> Result<Word, ParseError> {
        let tok = self.advance()?; // consume `arr=`
        let text = tok.text(self.source);
        self.advance()?; // consume '('
        let mut elements = Vec::new();
        while !self.at(&TokenKind::RParen) && !self.at(&TokenKind::Eof) {
            if self.at(&TokenKind::Newline) {
                self.advance()?;
                continue;
            }
            if self.at_word() {
                let w = self.advance()?;
                elements.push(w.text(self.source).to_string());
            } else {
                break;
            }
        }
        let end_span = if self.at(&TokenKind::RParen) {
            self.advance()?.span.end
        } else {
            self.current.span.end
        };
        // Build a synthetic word like "arr=(x y z)"
        let compound = format!("{text}({})", elements.join(" "));
        Ok(Word {
            parts: vec![WordPart::Literal(compound.into())],
            span: Span {
                start: tok.span.start,
                end: end_span,
            },
        })
    }

    /// True if the current token can start a new command.
    /// Check for `;;` (two consecutive semicolons — case item terminator).
    fn is_double_semi(&mut self) -> bool {
        if !self.at(&TokenKind::Semi) {
            return false;
        }
        matches!(self.peek_nth(0), Ok(tok) if tok.kind == TokenKind::Semi)
    }

    /// Check for `;&` (case fall-through): `;` followed by `&`.
    fn is_case_fallthrough(&mut self) -> bool {
        if !self.at(&TokenKind::Semi) {
            return false;
        }
        matches!(self.peek_nth(0), Ok(tok) if tok.kind == TokenKind::Amp)
    }

    /// Check for `;;&` (case continue-testing): `;;` followed by `&`.
    fn is_case_continue_testing(&mut self) -> bool {
        if !self.at(&TokenKind::Semi) {
            return false;
        }
        if !matches!(self.peek_nth(0), Ok(tok) if tok.kind == TokenKind::Semi) {
            return false;
        }
        matches!(self.peek_nth(1), Ok(tok) if tok.kind == TokenKind::Amp)
    }

    fn at_command_start(&self) -> bool {
        if self.at(&TokenKind::LParen) || self.at(&TokenKind::DblLBracket) {
            return true;
        }
        if self.at_word() {
            let text = self.current_text();
            return !TERMINATOR_WORDS.contains(&text);
        }
        self.at_redirection()
    }

    fn current_text(&self) -> &str {
        self.current.text(self.source)
    }

    fn skip_newlines(&mut self) -> Result<(), ParseError> {
        while self.at(&TokenKind::Newline) {
            self.advance()?;
        }
        Ok(())
    }

    fn span_from(&self, start: u32) -> Span {
        Span {
            start,
            end: self.prev_end,
        }
    }

    fn expect_word(&mut self, expected: &str) -> Result<Token, ParseError> {
        if self.at_word_eq(expected) {
            self.advance()
        } else {
            Err(ParseError {
                message: format!("expected '{}', got '{}'", expected, self.current_text()),
                offset: self.current.span.start,
            })
        }
    }

    // ---- Grammar rules ----

    fn parse_program(&mut self) -> Result<Program, ParseError> {
        self.skip_newlines()?;
        let mut commands = Vec::new();
        while self.at_command_start() {
            commands.push(self.parse_complete_command()?);
            self.skip_newlines()?;
        }
        if !self.at(&TokenKind::Eof) {
            return Err(ParseError {
                message: format!("unexpected token: {:?}", self.current.kind),
                offset: self.current.span.start,
            });
        }
        Ok(Program { commands })
    }

    /// Parse a compound list (body of compound commands).
    /// Stops at terminator words, `)`, or EOF.
    fn parse_compound_list(&mut self) -> Result<Vec<CompleteCommand>, ParseError> {
        self.skip_newlines()?;
        let mut commands = Vec::new();
        while self.at_command_start() {
            commands.push(self.parse_complete_command()?);
            self.skip_newlines()?;
        }
        Ok(commands)
    }

    fn parse_complete_command(&mut self) -> Result<CompleteCommand, ParseError> {
        let start = self.current.span.start;
        let mut list = Vec::new();
        list.push(self.parse_and_or()?);

        while self.at(&TokenKind::Semi)
            && !self.is_double_semi()
            && !self.is_case_fallthrough()
            && !self.is_case_continue_testing()
        {
            self.advance()?;
            // Don't skip newlines here if there are pending heredocs
            if self.pending_heredocs.is_empty() {
                self.skip_newlines()?;
            }
            if self.at_command_start() {
                list.push(self.parse_and_or()?);
            }
        }

        let mut cc = CompleteCommand {
            list,
            span: self.span_from(start),
        };

        // Resolve pending here-docs: read bodies from source after the newline
        if !self.pending_heredocs.is_empty() && self.at(&TokenKind::Newline) {
            self.resolve_heredocs(&mut cc)?;
        }

        Ok(cc)
    }

    fn parse_and_or(&mut self) -> Result<AndOrList, ParseError> {
        let first = self.parse_pipeline()?;
        let mut rest = Vec::new();

        loop {
            let op = if self.at(&TokenKind::AndAnd) {
                self.advance()?;
                AndOrOp::And
            } else if self.at(&TokenKind::OrOr) {
                self.advance()?;
                AndOrOp::Or
            } else {
                break;
            };
            self.skip_newlines()?;
            rest.push((op, self.parse_pipeline()?));
        }

        Ok(AndOrList { first, rest })
    }

    fn parse_pipeline(&mut self) -> Result<Pipeline, ParseError> {
        let negated = if self.at_word_eq("!") {
            self.advance()?;
            true
        } else {
            false
        };

        let mut commands = Vec::new();
        let mut pipe_stderr = Vec::new();
        commands.push(self.parse_command()?);

        loop {
            if self.at(&TokenKind::Pipe) {
                pipe_stderr.push(false);
                self.advance()?;
                self.skip_newlines()?;
                commands.push(self.parse_command()?);
            } else if self.at(&TokenKind::PipeAmp) {
                pipe_stderr.push(true);
                self.advance()?;
                self.skip_newlines()?;
                commands.push(self.parse_command()?);
            } else {
                break;
            }
        }

        Ok(Pipeline {
            negated,
            commands,
            pipe_stderr,
        })
    }

    fn parse_command(&mut self) -> Result<Command, ParseError> {
        // Arithmetic command: (( expr ))
        // Detected as LParen followed immediately by another LParen with no gap.
        if self.at(&TokenKind::LParen) {
            if let Ok(next) = self.peek_nth(0) {
                if next.kind == TokenKind::LParen && next.span.start == self.current.span.end {
                    return self.parse_arith_command();
                }
            }
            return self.parse_subshell();
        }

        // Extended test: [[ expression ]]
        if self.at(&TokenKind::DblLBracket) {
            return self.parse_double_bracket();
        }

        if self.at_word() {
            let text = self.current_text();
            match text {
                "{" => return self.parse_group(),
                "if" => return self.parse_if(),
                "while" => return self.parse_while(),
                "until" => return self.parse_until(),
                "for" => return self.parse_for(),
                "case" => return self.parse_case(),
                "select" => return self.parse_select(),
                "function" => return self.parse_function_bash(),
                _ => {
                    // Check for POSIX function definition: name() ...
                    if self.peek_nth(0)?.kind == TokenKind::LParen
                        && self.peek_nth(1)?.kind == TokenKind::RParen
                    {
                        return self.parse_function_posix();
                    }
                }
            }
        }

        Ok(Command::Simple(self.parse_simple_command()?))
    }

    // ---- Compound commands ----

    fn parse_subshell(&mut self) -> Result<Command, ParseError> {
        let start = self.current.span.start;
        self.advance()?; // consume (
        let body = self.parse_compound_list()?;
        if !self.at(&TokenKind::RParen) {
            return Err(ParseError {
                message: "expected ')' to close subshell".into(),
                offset: self.current.span.start,
            });
        }
        self.advance()?; // consume )
        Ok(Command::Subshell(SubshellCommand {
            body,
            span: self.span_from(start),
        }))
    }

    fn parse_group(&mut self) -> Result<Command, ParseError> {
        let start = self.current.span.start;
        self.expect_word("{")?;
        let body = self.parse_compound_list()?;
        self.expect_word("}")?;
        Ok(Command::Group(GroupCommand {
            body,
            span: self.span_from(start),
        }))
    }

    fn parse_if(&mut self) -> Result<Command, ParseError> {
        let start = self.current.span.start;
        self.expect_word("if")?;
        let condition = self.parse_compound_list()?;
        self.expect_word("then")?;
        let then_body = self.parse_compound_list()?;

        let mut elifs = Vec::new();
        while self.at_word_eq("elif") {
            self.advance()?;
            let elif_cond = self.parse_compound_list()?;
            self.expect_word("then")?;
            let elif_body = self.parse_compound_list()?;
            elifs.push(ElifClause {
                condition: elif_cond,
                then_body: elif_body,
            });
        }

        let else_body = if self.at_word_eq("else") {
            self.advance()?;
            Some(self.parse_compound_list()?)
        } else {
            None
        };

        self.expect_word("fi")?;
        Ok(Command::If(IfCommand {
            condition,
            then_body,
            elifs,
            else_body,
            span: self.span_from(start),
        }))
    }

    fn parse_while(&mut self) -> Result<Command, ParseError> {
        let start = self.current.span.start;
        self.expect_word("while")?;
        let condition = self.parse_compound_list()?;
        self.expect_word("do")?;
        let body = self.parse_compound_list()?;
        self.expect_word("done")?;
        Ok(Command::While(WhileCommand {
            condition,
            body,
            span: self.span_from(start),
        }))
    }

    fn parse_until(&mut self) -> Result<Command, ParseError> {
        let start = self.current.span.start;
        self.expect_word("until")?;
        let condition = self.parse_compound_list()?;
        self.expect_word("do")?;
        let body = self.parse_compound_list()?;
        self.expect_word("done")?;
        Ok(Command::Until(UntilCommand {
            condition,
            body,
            span: self.span_from(start),
        }))
    }

    fn parse_for(&mut self) -> Result<Command, ParseError> {
        let start = self.current.span.start;
        self.expect_word("for")?;

        if self.is_arith_for_start() {
            return self.parse_arith_for(start);
        }

        if !self.at_word() {
            return Err(ParseError {
                message: "expected variable name after 'for'".into(),
                offset: self.current.span.start,
            });
        }
        let var_name = self.current_text().into();
        self.advance()?;

        let words = self.parse_loop_words_clause()?;

        self.skip_newlines()?;
        self.expect_word("do")?;
        let body = self.parse_compound_list()?;
        self.expect_word("done")?;

        Ok(Command::For(ForCommand {
            var_name,
            words,
            body,
            span: self.span_from(start),
        }))
    }

    fn is_arith_for_start(&mut self) -> bool {
        let current_end = self.current.span.end;
        self.at(&TokenKind::LParen)
            && self
                .peek_nth(0)
                .is_ok_and(|next| next.kind == TokenKind::LParen && next.span.start == current_end)
    }

    fn parse_loop_words_clause(&mut self) -> Result<Option<Vec<Word>>, ParseError> {
        if !self.at_word_eq("in") {
            self.consume_optional_semi()?;
            return Ok(None);
        }

        self.advance()?;
        let mut words = Vec::new();
        while self.at_word()
            && !self.at_word_eq("do")
            && !TERMINATOR_WORDS.contains(&self.current_text())
        {
            words.push(self.parse_word()?);
        }
        self.consume_optional_semi()?;
        Ok(Some(words))
    }

    fn consume_optional_semi(&mut self) -> Result<(), ParseError> {
        if self.at(&TokenKind::Semi) {
            self.advance()?;
        }
        Ok(())
    }

    /// Parse `select name [in word ...]; do body; done`.
    fn parse_select(&mut self) -> Result<Command, ParseError> {
        let start = self.current.span.start;
        self.expect_word("select")?;

        if !self.at_word() {
            return Err(ParseError {
                message: "expected variable name after 'select'".into(),
                offset: self.current.span.start,
            });
        }
        let var_name = self.current_text().into();
        self.advance()?;

        // Optional `in word...` clause
        let words = if self.at_word_eq("in") {
            self.advance()?;
            let mut words = Vec::new();
            while self.at_word()
                && !self.at_word_eq("do")
                && !TERMINATOR_WORDS.contains(&self.current_text())
            {
                words.push(self.parse_word()?);
            }
            if self.at(&TokenKind::Semi) {
                self.advance()?;
            }
            Some(words)
        } else {
            if self.at(&TokenKind::Semi) {
                self.advance()?;
            }
            None
        };

        self.skip_newlines()?;
        self.expect_word("do")?;
        let body = self.parse_compound_list()?;
        self.expect_word("done")?;

        // Collect trailing redirections (e.g., `done <<< "input"`)
        let mut redirections = Vec::new();
        while self.at_redirection() || self.at_fd_prefix_redirection() {
            if self.at_fd_prefix_redirection() {
                let fd_text = self.current_text();
                let fd: u32 = fd_text.parse().unwrap_or(0);
                self.advance()?;
                let mut redir = self.parse_redirection()?;
                redir.fd = Some(fd);
                redirections.push(redir);
            } else {
                redirections.push(self.parse_redirection()?);
            }
        }

        Ok(Command::Select(SelectCommand {
            var_name,
            words,
            body,
            redirections,
            span: self.span_from(start),
        }))
    }

    /// Parse `(( expr ))` arithmetic command.
    /// The lexer tokenizes `((` as two `LParen` tokens. We consume them, then
    /// collect raw source characters until the matching `))`.
    fn parse_arith_command(&mut self) -> Result<Command, ParseError> {
        let start = self.current.span.start;
        self.advance()?; // first (
        self.advance()?; // second (

        let expr = self.collect_arith_expr()?;

        Ok(Command::ArithCommand(ArithCommandNode {
            expr: expr.into(),
            span: self.span_from(start),
        }))
    }

    /// Parse C-style for loop: `for (( init; cond; step )) do body done`.
    /// Called after `for` has been consumed. `start` is the span start of `for`.
    fn parse_arith_for(&mut self, start: u32) -> Result<Command, ParseError> {
        self.advance()?; // first (
        self.advance()?; // second (

        // Collect three semicolon-separated expressions until ))
        let inner = self.collect_arith_expr()?;

        // Split inner on ';' to get init, cond, step
        let parts: Vec<&str> = inner.splitn(3, ';').collect();
        let init = parts.first().map_or("", |s| s.trim());
        let cond = parts.get(1).map_or("", |s| s.trim());
        let step = parts.get(2).map_or("", |s| s.trim());

        // Optional ; or newline before `do`
        if self.at(&TokenKind::Semi) {
            self.advance()?;
        }
        self.skip_newlines()?;
        self.expect_word("do")?;
        let body = self.parse_compound_list()?;
        self.expect_word("done")?;

        Ok(Command::ArithFor(ArithForCommand {
            init: init.into(),
            cond: cond.into(),
            step: step.into(),
            body,
            span: self.span_from(start),
        }))
    }

    /// Collect raw source text for an arithmetic expression until `))` is found.
    /// Handles nested parentheses. Consumes the closing `))`.
    fn collect_arith_expr(&mut self) -> Result<String, ParseError> {
        // We need to read raw source text until we find ))
        // The current token is the first token after ((.
        // Strategy: track byte position in source and scan for )).
        let expr_start = self.current.span.start as usize;
        let src = self.source;
        let bytes = src.as_bytes();
        let mut pos = expr_start;
        let mut depth: u32 = 0;

        while pos < bytes.len() {
            if bytes[pos] == b'(' {
                depth += 1;
                pos += 1;
            } else if bytes[pos] == b')' {
                if depth > 0 {
                    depth -= 1;
                    pos += 1;
                } else if pos + 1 < bytes.len() && bytes[pos + 1] == b')' {
                    // Found ))
                    let expr = src[expr_start..pos].trim().to_string();
                    let end_pos = pos + 2;
                    // Reposition lexer past ))
                    self.lexer.set_position(end_pos);
                    self.peeked.clear();
                    self.prev_end = end_pos as u32;
                    self.current = self.lexer.next_token().map_err(lex_err)?;
                    return Ok(expr);
                } else {
                    return Err(ParseError {
                        message: "expected '))' to close arithmetic expression".into(),
                        offset: pos as u32,
                    });
                }
            } else {
                pos += 1;
            }
        }

        Err(ParseError {
            message: "unterminated arithmetic expression, expected '))'".into(),
            offset: expr_start as u32,
        })
    }

    /// Parse `case word in pattern) body ;; ... esac`.
    fn parse_case(&mut self) -> Result<Command, ParseError> {
        let start = self.current.span.start;
        self.expect_word("case")?;

        if !self.at_word() {
            return Err(ParseError {
                message: "expected word after 'case'".into(),
                offset: self.current.span.start,
            });
        }
        let word = self.parse_word()?;
        self.skip_newlines()?;
        self.expect_word("in")?;
        self.skip_newlines()?;

        let mut items = Vec::new();
        while !self.at_word_eq("esac") && !self.at(&TokenKind::Eof) {
            let patterns = self.parse_case_patterns()?;
            self.skip_newlines()?;

            let body = self.parse_case_body()?;
            let terminator = self.parse_case_terminator()?;
            self.skip_newlines()?;

            items.push(CaseItem {
                patterns,
                body,
                terminator,
            });
        }

        self.expect_word("esac")?;
        Ok(Command::Case(CaseCommand {
            word,
            items,
            span: self.span_from(start),
        }))
    }

    fn parse_case_patterns(&mut self) -> Result<Vec<Word>, ParseError> {
        if self.at(&TokenKind::LParen) {
            self.advance()?;
        }
        let mut patterns = vec![self.parse_word()?];
        while self.at(&TokenKind::Pipe) {
            self.advance()?;
            patterns.push(self.parse_word()?);
        }
        if !self.at(&TokenKind::RParen) {
            return Err(ParseError {
                message: "expected ')' after case pattern".into(),
                offset: self.current.span.start,
            });
        }
        self.advance()?;
        Ok(patterns)
    }

    fn parse_case_body(&mut self) -> Result<Vec<CompleteCommand>, ParseError> {
        let mut body = Vec::new();
        while self.at_command_start() && !self.is_double_semi() && !self.is_case_fallthrough() {
            body.push(self.parse_complete_command()?);
            self.skip_newlines()?;
        }
        Ok(body)
    }

    fn parse_case_terminator(&mut self) -> Result<CaseTerminator, ParseError> {
        if self.is_case_continue_testing() {
            self.advance()?;
            self.advance()?;
            self.advance()?;
            return Ok(CaseTerminator::ContinueTesting);
        }
        if self.is_double_semi() {
            self.advance()?;
            self.advance()?;
            return Ok(CaseTerminator::Break);
        }
        if self.is_case_fallthrough() {
            self.advance()?;
            self.advance()?;
            return Ok(CaseTerminator::Fallthrough);
        }
        Ok(CaseTerminator::Break)
    }

    /// Parse `[[ expression ]]` extended test command.
    fn parse_double_bracket(&mut self) -> Result<Command, ParseError> {
        let start = self.current.span.start;
        self.advance()?; // consume [[

        let mut words = Vec::new();
        loop {
            if self.at(&TokenKind::DblRBracket) {
                self.advance()?; // consume ]]
                break;
            }
            if self.at(&TokenKind::Eof) {
                return Err(ParseError {
                    message: "expected ']]' to close extended test".into(),
                    offset: self.current.span.start,
                });
            }
            // Collect tokens as words (operators inside [[ ]] are expression tokens)
            if self.at_word() {
                words.push(self.parse_word()?);
            } else {
                // Operator tokens (&&, ||, <, >, (, )) become literal words
                let tok = self.advance()?;
                let text = tok.text(self.source);
                words.push(Word {
                    parts: vec![WordPart::Literal(text.into())],
                    span: tok.span,
                });
            }
        }

        Ok(Command::DoubleBracket(DoubleBracketCommand {
            words,
            span: self.span_from(start),
        }))
    }

    /// Parse POSIX-style function: `name ( ) compound_command`
    fn parse_function_posix(&mut self) -> Result<Command, ParseError> {
        let start = self.current.span.start;
        let name = self.current_text().into();
        self.advance()?; // name
        self.advance()?; // (
        self.advance()?; // )
        self.skip_newlines()?;
        let body = Box::new(self.parse_command()?);
        Ok(Command::FunctionDef(FunctionDef {
            name,
            body,
            span: self.span_from(start),
        }))
    }

    /// Parse bash-style function: `function name [( )] compound_command`
    fn parse_function_bash(&mut self) -> Result<Command, ParseError> {
        let start = self.current.span.start;
        self.expect_word("function")?;

        if !self.at_word() {
            return Err(ParseError {
                message: "expected function name after 'function'".into(),
                offset: self.current.span.start,
            });
        }
        let name = self.current_text().into();
        self.advance()?;

        // Optional ( )
        if self.at(&TokenKind::LParen) {
            self.advance()?;
            if !self.at(&TokenKind::RParen) {
                return Err(ParseError {
                    message: "expected ')' after '(' in function definition".into(),
                    offset: self.current.span.start,
                });
            }
            self.advance()?;
        }

        self.skip_newlines()?;
        let body = Box::new(self.parse_command()?);
        Ok(Command::FunctionDef(FunctionDef {
            name,
            body,
            span: self.span_from(start),
        }))
    }

    // ---- Simple commands ----

    fn parse_simple_command(&mut self) -> Result<SimpleCommand, ParseError> {
        let start = self.current.span.start;
        let mut assignments = Vec::new();
        let mut words = Vec::new();
        let mut redirections = Vec::new();
        let mut past_assignments = false;

        loop {
            if !self.parse_simple_command_part(
                &mut assignments,
                &mut words,
                &mut redirections,
                &mut past_assignments,
            )? {
                break;
            }
        }

        if assignments.is_empty() && words.is_empty() && redirections.is_empty() {
            return Err(ParseError {
                message: "expected a command".into(),
                offset: self.current.span.start,
            });
        }

        Ok(SimpleCommand {
            assignments,
            words,
            redirections,
            span: self.span_from(start),
        })
    }

    fn parse_simple_command_part(
        &mut self,
        assignments: &mut Vec<Assignment>,
        words: &mut Vec<Word>,
        redirections: &mut Vec<Redirection>,
        past_assignments: &mut bool,
    ) -> Result<bool, ParseError> {
        if self.at_fd_prefix_redirection() {
            redirections.push(self.parse_fd_prefixed_redirection()?);
            return Ok(true);
        }
        if self.at_redirection() {
            redirections.push(self.parse_redirection()?);
            return Ok(true);
        }
        if self.at_word() {
            self.parse_simple_word_part(assignments, words, past_assignments)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn parse_fd_prefixed_redirection(&mut self) -> Result<Redirection, ParseError> {
        let fd_text = self.current_text();
        let fd: u32 = fd_text.parse().unwrap_or(0);
        self.advance()?;
        let mut redir = self.parse_redirection()?;
        redir.fd = Some(fd);
        Ok(redir)
    }

    fn parse_simple_word_part(
        &mut self,
        assignments: &mut Vec<Assignment>,
        words: &mut Vec<Word>,
        past_assignments: &mut bool,
    ) -> Result<(), ParseError> {
        let text = self.current_text();
        if !*past_assignments && is_assignment_text(text) {
            assignments.push(self.parse_assignment()?);
            return Ok(());
        }
        *past_assignments = true;
        if text.ends_with('=') && self.peek_is_lparen() {
            words.push(self.parse_compound_assign_word()?);
        } else {
            words.push(self.parse_word()?);
        }
        Ok(())
    }

    fn parse_word(&mut self) -> Result<Word, ParseError> {
        let tok = self.advance()?;
        let text = tok.text(self.source);
        let parts = word_parser::parse_word_parts(text);
        Ok(Word {
            parts,
            span: tok.span,
        })
    }

    fn parse_assignment(&mut self) -> Result<Assignment, ParseError> {
        let tok = self.advance()?;
        let text = tok.text(self.source);
        let eq_pos = text.find('=').expect("assignment must contain '='");
        let name = &text[..eq_pos];
        let val_str = &text[eq_pos + 1..];

        let value = if val_str.is_empty() {
            self.parse_assignment_compound_value(&tok, eq_pos)?
        } else {
            Some(self.make_assignment_word(&tok, eq_pos, val_str))
        };

        Ok(Assignment {
            name: name.into(),
            value,
            span: tok.span,
        })
    }

    fn parse_assignment_compound_value(
        &mut self,
        tok: &Token,
        eq_pos: usize,
    ) -> Result<Option<Word>, ParseError> {
        if !self.at(&TokenKind::LParen) {
            return Ok(None);
        }
        self.advance()?;
        let paren_start = tok.span.start + eq_pos as u32 + 1;
        let mut elements = Vec::new();
        while !self.at(&TokenKind::RParen) && !self.at(&TokenKind::Eof) {
            if self.at(&TokenKind::Newline) {
                self.advance()?;
                continue;
            }
            if !self.at_word() {
                break;
            }
            let word = self.advance()?;
            elements.push(word.text(self.source).to_string());
        }
        let end_span = if self.at(&TokenKind::RParen) {
            self.advance()?.span.end
        } else {
            self.current.span.end
        };
        Ok(Some(Word {
            parts: vec![WordPart::Literal(
                format!("({})", elements.join(" ")).into(),
            )],
            span: Span {
                start: paren_start,
                end: end_span,
            },
        }))
    }

    #[allow(clippy::unused_self)]
    fn make_assignment_word(&self, tok: &Token, eq_pos: usize, val_str: &str) -> Word {
        let val_start = tok.span.start + eq_pos as u32 + 1;
        Word {
            parts: word_parser::parse_word_parts(val_str),
            span: Span {
                start: val_start,
                end: tok.span.end,
            },
        }
    }

    fn parse_redirection(&mut self) -> Result<Redirection, ParseError> {
        let op_tok = self.advance()?;
        let (op, is_heredoc) = match op_tok.kind {
            TokenKind::Less => (RedirectionOp::Input, false),
            TokenKind::Greater | TokenKind::AmpGreater => (RedirectionOp::Output, false),
            TokenKind::GreaterGreater => (RedirectionOp::Append, false),
            TokenKind::LessGreater => (RedirectionOp::ReadWrite, false),
            TokenKind::LessLess => (RedirectionOp::HereDoc, true),
            TokenKind::LessLessDash => (RedirectionOp::HereDocStrip, true),
            TokenKind::LessLessLess => (RedirectionOp::HereString, false),
            _ => {
                return Err(ParseError {
                    message: format!("unexpected redirection operator: {:?}", op_tok.kind),
                    offset: op_tok.span.start,
                });
            }
        };

        // For &>, we treat it as both stdout and stderr to file
        let is_amp_greater = op_tok.kind == TokenKind::AmpGreater;

        // Check for >&N or <&N (fd duplication): > followed by & followed by digit word
        if self.at(&TokenKind::Amp) && matches!(op_tok.kind, TokenKind::Greater | TokenKind::Less) {
            self.advance()?; // consume &
            if self.at_word() {
                let target = self.parse_word()?;
                let span = Span {
                    start: op_tok.span.start,
                    end: target.span.end,
                };
                let dup_op = if op_tok.kind == TokenKind::Greater {
                    RedirectionOp::DupOutput
                } else {
                    RedirectionOp::DupInput
                };
                return Ok(Redirection {
                    fd: None,
                    op: dup_op,
                    target,
                    here_doc_body: None,
                    span,
                });
            }
            return Err(ParseError {
                message: "expected fd number after >&".into(),
                offset: self.current.span.start,
            });
        }

        if !self.at_word() {
            return Err(ParseError {
                message: "expected word after redirection operator".into(),
                offset: self.current.span.start,
            });
        }
        let target = self.parse_word()?;
        let span = Span {
            start: op_tok.span.start,
            end: target.span.end,
        };

        if is_heredoc {
            // Extract delimiter text (strip quotes from the delimiter word)
            let delim = heredoc_delimiter(&target);
            self.pending_heredocs.push(PendingHereDoc {
                delimiter: delim,
                strip_tabs: op == RedirectionOp::HereDocStrip,
            });
        }

        // For &>, produce a fd=None redirection (handled specially in runtime)
        // We encode this by using fd = Some(u32::MAX) as a sentinel for &>
        let fd = if is_amp_greater {
            Some(u32::MAX) // sentinel: redirect both stdout and stderr
        } else {
            None
        };

        Ok(Redirection {
            fd,
            op,
            target,
            here_doc_body: None,
            span,
        })
    }

    /// Read here-doc bodies from source and attach them to the AST.
    fn resolve_heredocs(&mut self, cc: &mut CompleteCommand) -> Result<(), ParseError> {
        let newline_end = self.current.span.end as usize;
        let mut scan_pos = newline_end;

        let mut bodies = Vec::new();
        for hd in &self.pending_heredocs {
            let body_start = scan_pos;
            loop {
                let (line_start, line_end, line) = self.heredoc_line(scan_pos);
                let check_line = self.heredoc_check_line(line, hd.strip_tabs);
                if check_line == hd.delimiter {
                    bodies.push(self.build_heredoc_body(body_start, line_start, hd.strip_tabs));
                    scan_pos = self.advance_heredoc_scan(line_end);
                    break;
                }

                if line_end >= self.source.len() {
                    return Err(ParseError {
                        message: format!("unterminated here-doc, expected '{}'", hd.delimiter),
                        offset: body_start as u32,
                    });
                }
                scan_pos = line_end + 1;
            }
        }

        // Walk the AST and assign bodies in order
        let mut body_iter = bodies.into_iter();
        assign_heredoc_bodies_cc(cc, &mut body_iter);

        self.pending_heredocs.clear();

        self.lexer.set_position(scan_pos);
        self.peeked.clear();
        self.current = self.lexer.next_token().map_err(lex_err)?;

        Ok(())
    }

    fn heredoc_line(&self, scan_pos: usize) -> (usize, usize, &str) {
        let line_start = scan_pos;
        let line_end = self.source[scan_pos..]
            .find('\n')
            .map_or(self.source.len(), |i| scan_pos + i);
        (line_start, line_end, &self.source[line_start..line_end])
    }

    #[allow(clippy::unused_self)]
    fn heredoc_check_line<'a>(&self, line: &'a str, strip_tabs: bool) -> &'a str {
        if strip_tabs {
            line.trim_start_matches('\t')
        } else {
            line
        }
    }

    fn build_heredoc_body(
        &self,
        body_start: usize,
        line_start: usize,
        strip_tabs: bool,
    ) -> HereDocBody {
        let raw_body = &self.source[body_start..line_start];
        let content = if strip_tabs {
            raw_body
                .lines()
                .map(|line| line.trim_start_matches('\t'))
                .collect::<Vec<_>>()
                .join("\n")
                + if raw_body.ends_with('\n') { "\n" } else { "" }
        } else {
            raw_body.to_string()
        };
        HereDocBody {
            content: content.into(),
            span: Span {
                start: body_start as u32,
                end: line_start as u32,
            },
        }
    }

    fn advance_heredoc_scan(&self, line_end: usize) -> usize {
        if line_end < self.source.len() {
            line_end + 1
        } else {
            line_end
        }
    }
}

/// Walk a `CompleteCommand` and assign here-doc bodies to here-doc redirections in source order.
fn assign_heredoc_bodies_cc(
    cc: &mut CompleteCommand,
    bodies: &mut impl Iterator<Item = HereDocBody>,
) {
    for and_or in &mut cc.list {
        assign_heredoc_bodies_pipeline(&mut and_or.first, bodies);
        for (_, pipeline) in &mut and_or.rest {
            assign_heredoc_bodies_pipeline(pipeline, bodies);
        }
    }
}

fn assign_heredoc_bodies_pipeline(
    pipeline: &mut Pipeline,
    bodies: &mut impl Iterator<Item = HereDocBody>,
) {
    for cmd in &mut pipeline.commands {
        assign_heredoc_bodies_cmd(cmd, bodies);
    }
}

fn assign_heredoc_bodies_cmd(cmd: &mut Command, bodies: &mut impl Iterator<Item = HereDocBody>) {
    if let Command::Simple(sc) = cmd {
        for redir in &mut sc.redirections {
            if matches!(
                redir.op,
                RedirectionOp::HereDoc | RedirectionOp::HereDocStrip
            ) && redir.here_doc_body.is_none()
            {
                redir.here_doc_body = bodies.next();
            }
        }
    }
}

/// Extract the delimiter string from a here-doc target word, stripping quotes.
fn heredoc_delimiter(word: &Word) -> String {
    let mut result = String::new();
    for part in &word.parts {
        match part {
            WordPart::Literal(s) | WordPart::SingleQuoted(s) => result.push_str(s),
            WordPart::DoubleQuoted(parts) => {
                for p in parts {
                    if let WordPart::Literal(s) = p {
                        result.push_str(s);
                    }
                }
            }
            _ => {}
        }
    }
    result
}

fn is_assignment_text(text: &str) -> bool {
    let Some(eq_pos) = text.find('=') else {
        return false;
    };
    let mut name = &text[..eq_pos];
    if name.is_empty() {
        return false;
    }
    // Handle += operator: strip trailing '+'
    if name.ends_with('+') {
        name = &name[..name.len() - 1];
        if name.is_empty() {
            return false;
        }
    }
    // Strip trailing [subscript] for array element assignment
    if let Some(bracket_start) = name.find('[') {
        if name.ends_with(']') {
            name = &name[..bracket_start];
        } else {
            return false;
        }
    }
    if name.is_empty() {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn lex_err(e: wasmsh_lex::LexerError) -> ParseError {
    ParseError {
        message: e.message,
        offset: e.span.start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(source: &str) -> Program {
        parse(source).unwrap_or_else(|e| panic!("parse failed for {source:?}: {e}"))
    }

    fn first_simple(source: &str) -> SimpleCommand {
        let prog = parse_ok(source);
        let cmd = &prog.commands[0].list[0].first.commands[0];
        match cmd {
            Command::Simple(sc) => sc.clone(),
            other => panic!("expected simple command, got {other:?}"),
        }
    }

    fn first_command(source: &str) -> Command {
        let prog = parse_ok(source);
        prog.commands[0].list[0].first.commands[0].clone()
    }

    fn word_texts(sc: &SimpleCommand) -> Vec<&str> {
        sc.words
            .iter()
            .map(|w| match &w.parts[0] {
                WordPart::Literal(s) => s.as_str(),
                _ => panic!("expected literal word part"),
            })
            .collect()
    }

    // ---- Simple commands (from prompt 02) ----

    #[test]
    fn parse_empty_input() {
        let program = parse_ok("");
        assert!(program.commands.is_empty());
    }

    #[test]
    fn parse_echo_hi() {
        let sc = first_simple("echo hi");
        assert_eq!(word_texts(&sc), vec!["echo", "hi"]);
    }

    #[test]
    fn parse_assignment_prefix() {
        let sc = first_simple("FOO=1 BAR=2 env");
        assert_eq!(sc.assignments.len(), 2);
        assert_eq!(word_texts(&sc), vec!["env"]);
    }

    #[test]
    fn parse_redirections() {
        let sc = first_simple("cat < in > out");
        assert_eq!(word_texts(&sc), vec!["cat"]);
        assert_eq!(sc.redirections.len(), 2);
    }

    #[test]
    fn parse_pipeline() {
        let prog = parse_ok("a | b | c");
        assert_eq!(prog.commands[0].list[0].first.commands.len(), 3);
    }

    #[test]
    fn parse_and_or_semicolons() {
        let prog = parse_ok("false && echo x; true || echo y");
        assert_eq!(prog.commands[0].list.len(), 2);
    }

    #[test]
    fn parse_spans_preserved() {
        let sc = first_simple("echo hello");
        assert_eq!(sc.span, Span { start: 0, end: 10 });
    }

    // ---- Subshell ----

    #[test]
    fn parse_subshell() {
        let cmd = first_command("(echo hi)");
        let Command::Subshell(sub) = cmd else {
            panic!("expected subshell");
        };
        assert_eq!(sub.body.len(), 1);
    }

    #[test]
    fn parse_subshell_with_semicolons() {
        let cmd = first_command("(echo a; echo b)");
        let Command::Subshell(sub) = cmd else {
            panic!("expected subshell");
        };
        // one complete command with two and_or entries
        assert_eq!(sub.body[0].list.len(), 2);
    }

    // ---- Group ----

    #[test]
    fn parse_group() {
        let cmd = first_command("{ echo hi; }");
        let Command::Group(grp) = cmd else {
            panic!("expected group");
        };
        assert_eq!(grp.body.len(), 1);
    }

    // ---- If ----

    #[test]
    fn parse_if_then_fi() {
        let cmd = first_command("if true; then echo yes; fi");
        let Command::If(if_cmd) = cmd else {
            panic!("expected if");
        };
        assert_eq!(if_cmd.condition.len(), 1);
        assert_eq!(if_cmd.then_body.len(), 1);
        assert!(if_cmd.elifs.is_empty());
        assert!(if_cmd.else_body.is_none());
    }

    #[test]
    fn parse_if_else() {
        let cmd = first_command("if true; then echo yes; else echo no; fi");
        let Command::If(if_cmd) = cmd else {
            panic!("expected if");
        };
        assert!(if_cmd.else_body.is_some());
    }

    #[test]
    fn parse_if_elif_else() {
        let cmd = first_command("if a; then b; elif c; then d; elif e; then f; else g; fi");
        let Command::If(if_cmd) = cmd else {
            panic!("expected if");
        };
        assert_eq!(if_cmd.elifs.len(), 2);
        assert!(if_cmd.else_body.is_some());
    }

    // ---- While ----

    #[test]
    fn parse_while() {
        let cmd = first_command("while true; do echo loop; done");
        let Command::While(w) = cmd else {
            panic!("expected while");
        };
        assert_eq!(w.condition.len(), 1);
        assert_eq!(w.body.len(), 1);
    }

    // ---- Until ----

    #[test]
    fn parse_until() {
        let cmd = first_command("until false; do echo loop; done");
        let Command::Until(u) = cmd else {
            panic!("expected until");
        };
        assert_eq!(u.condition.len(), 1);
        assert_eq!(u.body.len(), 1);
    }

    // ---- For ----

    #[test]
    fn parse_for_in() {
        let cmd = first_command("for x in a b c; do echo x; done");
        let Command::For(f) = cmd else {
            panic!("expected for");
        };
        assert_eq!(f.var_name.as_str(), "x");
        let words = f.words.as_ref().unwrap();
        assert_eq!(words.len(), 3);
        assert_eq!(f.body.len(), 1);
    }

    #[test]
    fn parse_for_no_in() {
        let cmd = first_command("for x; do echo x; done");
        let Command::For(f) = cmd else {
            panic!("expected for");
        };
        assert!(f.words.is_none());
    }

    #[test]
    fn parse_for_newline_before_do() {
        let cmd = first_command("for x in a b c\ndo\necho x\ndone");
        let Command::For(f) = cmd else {
            panic!("expected for");
        };
        assert_eq!(f.words.as_ref().unwrap().len(), 3);
    }

    // ---- Case ----

    #[test]
    fn parse_case() {
        let cmd = first_command("case x in\na) echo a;;\nb) echo b;;\nesac");
        let Command::Case(c) = cmd else {
            panic!("expected case");
        };
        assert_eq!(c.items.len(), 2);
    }

    #[test]
    fn parse_case_wildcard() {
        let cmd = first_command("case x in\n*) echo default;;\nesac");
        let Command::Case(c) = cmd else {
            panic!("expected case");
        };
        assert_eq!(c.items.len(), 1);
    }

    // ---- Function definitions ----

    #[test]
    fn parse_function_posix() {
        let cmd = first_command("greet() { echo hello; }");
        let Command::FunctionDef(fd) = cmd else {
            panic!("expected function def");
        };
        assert_eq!(fd.name.as_str(), "greet");
        assert!(matches!(*fd.body, Command::Group(_)));
    }

    #[test]
    fn parse_function_bash() {
        let cmd = first_command("function greet { echo hello; }");
        let Command::FunctionDef(fd) = cmd else {
            panic!("expected function def");
        };
        assert_eq!(fd.name.as_str(), "greet");
    }

    #[test]
    fn parse_function_bash_with_parens() {
        let cmd = first_command("function greet() { echo hello; }");
        let Command::FunctionDef(fd) = cmd else {
            panic!("expected function def");
        };
        assert_eq!(fd.name.as_str(), "greet");
    }

    // ---- Reserved words are context-sensitive ----

    #[test]
    fn reserved_word_as_argument() {
        // `if` as an argument, not as a keyword
        let sc = first_simple("echo if then else");
        assert_eq!(word_texts(&sc), vec!["echo", "if", "then", "else"]);
    }

    // ---- Here-docs ----

    #[test]
    fn parse_heredoc() {
        let source = "cat <<EOF\nhello world\nEOF\n";
        let sc = first_simple(source);
        assert_eq!(sc.redirections.len(), 1);
        assert_eq!(sc.redirections[0].op, RedirectionOp::HereDoc);
        let body = sc.redirections[0].here_doc_body.as_ref().unwrap();
        assert_eq!(body.content.as_str(), "hello world\n");
    }

    #[test]
    fn parse_heredoc_multiline() {
        let source = "cat <<EOF\nline1\nline2\nline3\nEOF\n";
        let sc = first_simple(source);
        let body = sc.redirections[0].here_doc_body.as_ref().unwrap();
        assert_eq!(body.content.as_str(), "line1\nline2\nline3\n");
    }

    #[test]
    fn parse_heredoc_strip_tabs() {
        let source = "cat <<-EOF\n\thello\n\tworld\n\tEOF\n";
        let sc = first_simple(source);
        assert_eq!(sc.redirections[0].op, RedirectionOp::HereDocStrip);
        let body = sc.redirections[0].here_doc_body.as_ref().unwrap();
        assert_eq!(body.content.as_str(), "hello\nworld\n");
    }

    #[test]
    fn parse_multiple_heredocs() {
        let source = "cmd <<A <<B\nbody_a\nA\nbody_b\nB\n";
        let sc = first_simple(source);
        assert_eq!(sc.redirections.len(), 2);
        let body_a = sc.redirections[0].here_doc_body.as_ref().unwrap();
        assert_eq!(body_a.content.as_str(), "body_a\n");
        let body_b = sc.redirections[1].here_doc_body.as_ref().unwrap();
        assert_eq!(body_b.content.as_str(), "body_b\n");
    }

    #[test]
    fn parse_heredoc_in_pipeline() {
        let source = "cat <<EOF | wc -l\nhello\nEOF\n";
        let prog = parse_ok(source);
        let pipeline = &prog.commands[0].list[0].first;
        assert_eq!(pipeline.commands.len(), 2);
        if let Command::Simple(sc) = &pipeline.commands[0] {
            let body = sc.redirections[0].here_doc_body.as_ref().unwrap();
            assert_eq!(body.content.as_str(), "hello\n");
        } else {
            panic!("expected simple command");
        }
    }

    #[test]
    fn parse_heredoc_followed_by_command() {
        let source = "cat <<EOF\nhello\nEOF\necho done";
        let prog = parse_ok(source);
        assert_eq!(prog.commands.len(), 2);
    }

    #[test]
    fn parse_error_unterminated_heredoc() {
        assert!(parse("cat <<EOF\nhello\n").is_err());
    }

    // ---- Error cases ----

    #[test]
    fn parse_error_on_lone_pipe() {
        assert!(parse("|").is_err());
    }

    #[test]
    fn parse_error_on_lone_semicolon() {
        assert!(parse(";").is_err());
    }

    #[test]
    fn parse_error_redirect_no_target() {
        assert!(parse("echo >").is_err());
    }

    #[test]
    fn parse_error_unclosed_if() {
        assert!(parse("if true; then echo x").is_err());
    }

    #[test]
    fn parse_error_unclosed_group() {
        assert!(parse("{ echo x").is_err());
    }

    #[test]
    fn parse_error_unclosed_subshell() {
        assert!(parse("(echo x").is_err());
    }

    // ---- Here-string ----

    #[test]
    fn parse_here_string() {
        let sc = first_simple("cat <<< hello");
        assert_eq!(word_texts(&sc), vec!["cat"]);
        assert_eq!(sc.redirections.len(), 1);
        assert_eq!(sc.redirections[0].op, RedirectionOp::HereString);
    }

    // ---- Fd-prefix redirection ----

    #[test]
    fn parse_stderr_redirect() {
        let sc = first_simple("cmd 2> file");
        assert_eq!(sc.redirections.len(), 1);
        assert_eq!(sc.redirections[0].fd, Some(2));
        assert_eq!(sc.redirections[0].op, RedirectionOp::Output);
    }

    #[test]
    fn parse_fd_dup_output() {
        let sc = first_simple("cmd 2>&1");
        assert_eq!(sc.redirections.len(), 1);
        assert_eq!(sc.redirections[0].fd, Some(2));
        assert_eq!(sc.redirections[0].op, RedirectionOp::DupOutput);
    }

    #[test]
    fn parse_amp_greater() {
        let sc = first_simple("cmd &> file");
        assert_eq!(sc.redirections.len(), 1);
        // &> is encoded as fd=MAX, op=Output
        assert_eq!(sc.redirections[0].fd, Some(u32::MAX));
        assert_eq!(sc.redirections[0].op, RedirectionOp::Output);
    }

    // ---- Double bracket ----

    #[test]
    fn parse_double_bracket_basic() {
        let cmd = first_command("[[ hello == world ]]");
        let Command::DoubleBracket(db) = cmd else {
            panic!("expected DoubleBracket, got {cmd:?}");
        };
        assert_eq!(db.words.len(), 3);
    }

    #[test]
    fn parse_double_bracket_with_var() {
        let cmd = first_command("[[ $x == hello ]]");
        let Command::DoubleBracket(db) = cmd else {
            panic!("expected DoubleBracket");
        };
        assert_eq!(db.words.len(), 3);
    }

    #[test]
    fn parse_double_bracket_logical_ops() {
        // && and || inside [[ ]] should be captured as expression tokens
        let cmd = first_command("[[ a == a && b == b ]]");
        let Command::DoubleBracket(db) = cmd else {
            panic!("expected DoubleBracket");
        };
        // a, ==, a, &&, b, ==, b
        assert_eq!(db.words.len(), 7);
    }

    #[test]
    fn parse_double_bracket_in_if() {
        let prog = parse_ok("if [[ x == x ]]; then echo yes; fi");
        let cmd = &prog.commands[0].list[0].first.commands[0];
        assert!(matches!(cmd, Command::If(_)));
    }

    #[test]
    fn parse_double_bracket_in_pipeline() {
        let prog = parse_ok("[[ x == x ]] && echo yes");
        assert!(!prog.commands.is_empty());
    }

    // ---- Arithmetic command (( )) ----

    #[test]
    fn parse_arith_command_basic() {
        let cmd = first_command("((1+2))");
        let Command::ArithCommand(ac) = cmd else {
            panic!("expected ArithCommand, got {cmd:?}");
        };
        assert_eq!(ac.expr.as_str(), "1+2");
    }

    #[test]
    fn parse_arith_command_with_spaces() {
        let cmd = first_command("(( x = 1 + 2 ))");
        let Command::ArithCommand(ac) = cmd else {
            panic!("expected ArithCommand, got {cmd:?}");
        };
        assert_eq!(ac.expr.as_str(), "x = 1 + 2");
    }

    #[test]
    fn parse_arith_command_with_parens() {
        let cmd = first_command("(( (1+2) * 3 ))");
        let Command::ArithCommand(ac) = cmd else {
            panic!("expected ArithCommand, got {cmd:?}");
        };
        assert_eq!(ac.expr.as_str(), "(1+2) * 3");
    }

    #[test]
    fn parse_arith_command_in_if() {
        let prog = parse_ok("if (( x > 0 )); then echo yes; fi");
        let cmd = &prog.commands[0].list[0].first.commands[0];
        assert!(matches!(cmd, Command::If(_)));
    }

    #[test]
    fn parse_arith_command_in_and_or() {
        let prog = parse_ok("(( x > 0 )) && echo yes");
        assert!(!prog.commands.is_empty());
    }

    // ---- C-style for (( )) ----

    #[test]
    fn parse_arith_for_basic() {
        let cmd = first_command("for ((i=0; i<10; i++)) do echo $i; done");
        let Command::ArithFor(af) = cmd else {
            panic!("expected ArithFor, got {cmd:?}");
        };
        assert_eq!(af.init.as_str(), "i=0");
        assert_eq!(af.cond.as_str(), "i<10");
        assert_eq!(af.step.as_str(), "i++");
        assert_eq!(af.body.len(), 1);
    }

    #[test]
    fn parse_arith_for_with_spaces() {
        let cmd = first_command("for (( i = 0; i < 5; i++ )) do echo $i; done");
        let Command::ArithFor(af) = cmd else {
            panic!("expected ArithFor, got {cmd:?}");
        };
        assert_eq!(af.init.as_str(), "i = 0");
        assert_eq!(af.cond.as_str(), "i < 5");
        assert_eq!(af.step.as_str(), "i++");
    }

    #[test]
    fn parse_arith_for_with_semicolon_before_do() {
        let cmd = first_command("for ((i=0; i<3; i++)); do echo $i; done");
        let Command::ArithFor(af) = cmd else {
            panic!("expected ArithFor, got {cmd:?}");
        };
        assert_eq!(af.init.as_str(), "i=0");
        assert_eq!(af.cond.as_str(), "i<3");
        assert_eq!(af.step.as_str(), "i++");
    }

    #[test]
    fn parse_arith_for_newline_before_do() {
        let cmd = first_command("for ((i=0; i<3; i++))\ndo\necho $i\ndone");
        let Command::ArithFor(af) = cmd else {
            panic!("expected ArithFor, got {cmd:?}");
        };
        assert_eq!(af.init.as_str(), "i=0");
    }

    #[test]
    fn parse_subshell_not_confused_with_arith() {
        // A subshell ( echo hi ) should not be confused with (( ))
        let cmd = first_command("(echo hi)");
        assert!(matches!(cmd, Command::Subshell(_)));
    }
}
