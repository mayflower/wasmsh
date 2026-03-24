//! Stateful lexer for the wasmsh shell.
//!
//! The lexer operates with context-dependent modes and produces structured
//! tokens with span information. Words may span across quotes and dollar
//! expansions — the lexer tracks nesting to find the correct word boundary.

use wasmsh_ast::Span;

/// Lexer mode tracking quoting and expansion context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexerMode {
    Normal,
    Comment,
}

/// The kind of token produced by the lexer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// A word token. `is_reserved_candidate` is true when the text matches a
    /// shell reserved word (only meaningful when unquoted).
    Word {
        is_reserved_candidate: bool,
    },
    Semi,
    Newline,
    Pipe,
    AndAnd,
    OrOr,
    Amp,
    PipeAmp,
    Less,
    Greater,
    GreaterGreater,
    LessLess,
    LessLessDash,
    LessLessLess,
    LessGreater,
    AmpGreater,
    LParen,
    RParen,
    DblLBracket,
    DblRBracket,
    Eof,
}

/// A token with its kind and source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    #[must_use]
    pub fn text<'s>(&self, source: &'s str) -> &'s str {
        &source[self.span.start as usize..self.span.end as usize]
    }
}

/// Errors produced during lexing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexerError {
    pub message: String,
    pub span: Span,
}

impl std::fmt::Display for LexerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "lexer error at {}..{}: {}",
            self.span.start, self.span.end, self.message
        )
    }
}

impl std::error::Error for LexerError {}

const RESERVED_WORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "do", "done", "case", "esac", "while", "until", "for",
    "in", "function", "select", "time", "!", "{", "}", "[[", "]]",
];

fn is_reserved_word(s: &str) -> bool {
    RESERVED_WORDS.contains(&s)
}

/// The shell lexer.
#[derive(Debug)]
pub struct Lexer<'src> {
    source: &'src [u8],
    pos: usize,
    mode: LexerMode,
}

impl<'src> Lexer<'src> {
    #[must_use]
    pub fn new(source: &'src str) -> Self {
        Self {
            source: source.as_bytes(),
            pos: 0,
            mode: LexerMode::Normal,
        }
    }

    #[must_use]
    pub fn mode(&self) -> LexerMode {
        self.mode
    }

    #[must_use]
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Set the lexer position (used by the parser to skip past here-doc bodies).
    pub fn set_position(&mut self, pos: usize) {
        self.pos = pos;
        self.mode = LexerMode::Normal;
    }

    fn peek(&self) -> Option<u8> {
        self.source.get(self.pos).copied()
    }

    fn peek_ahead(&self, offset: usize) -> Option<u8> {
        self.source.get(self.pos + offset).copied()
    }

    fn skip_blanks(&mut self) {
        while let Some(b' ' | b'\t') = self.peek() {
            self.pos += 1;
        }
    }

    fn span_from(&self, start: usize) -> Span {
        Span {
            start: start as u32,
            end: self.pos as u32,
        }
    }

    fn single_op(&mut self, kind: TokenKind) -> Token {
        let start = self.pos;
        self.pos += 1;
        Token {
            kind,
            span: self.span_from(start),
        }
    }

    fn double_op(&mut self, kind: TokenKind) -> Token {
        let start = self.pos;
        self.pos += 2;
        Token {
            kind,
            span: self.span_from(start),
        }
    }

    fn triple_op(&mut self, kind: TokenKind) -> Token {
        let start = self.pos;
        self.pos += 3;
        Token {
            kind,
            span: self.span_from(start),
        }
    }

    fn consume_comment(&mut self) {
        while let Some(b) = self.peek() {
            if b == b'\n' {
                break;
            }
            self.pos += 1;
        }
    }

    // ---- Quoting / expansion helpers for word reading ----

    fn consume_single_quoted(&mut self) -> Result<(), LexerError> {
        let start = self.pos;
        self.pos += 1; // opening '
        loop {
            match self.peek() {
                None => {
                    return Err(LexerError {
                        message: "unterminated single quote".into(),
                        span: self.span_from(start),
                    });
                }
                Some(b'\'') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(_) => {
                    self.pos += 1;
                }
            }
        }
    }

    fn consume_double_quoted(&mut self) -> Result<(), LexerError> {
        let start = self.pos;
        self.pos += 1; // opening "
        loop {
            match self.peek() {
                None => {
                    return Err(LexerError {
                        message: "unterminated double quote".into(),
                        span: self.span_from(start),
                    });
                }
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(b'\\') => {
                    self.pos += 1;
                    if self.peek().is_some() {
                        self.pos += 1;
                    }
                }
                Some(b'$') => {
                    self.consume_dollar()?;
                }
                Some(_) => {
                    self.pos += 1;
                }
            }
        }
    }

    fn consume_backslash(&mut self) {
        self.pos += 1; // backslash
        if self.peek().is_some() {
            self.pos += 1; // escaped char
        }
    }

    fn consume_dollar(&mut self) -> Result<(), LexerError> {
        self.pos += 1; // $
        match self.peek() {
            Some(b'\'') => {
                // $'...' ANSI-C quoting — consume like single-quoted but we keep the $' prefix
                self.consume_single_quoted()?;
            }
            Some(b'(') => {
                self.pos += 1;
                if self.peek() == Some(b'(') {
                    // $(( ... ))
                    self.pos += 1;
                    self.consume_arithmetic()?;
                } else {
                    // $( ... )
                    self.consume_command_subst()?;
                }
            }
            Some(b'{') => {
                self.pos += 1;
                self.consume_brace_param()?;
            }
            Some(b) if b.is_ascii_alphabetic() || b == b'_' => {
                while let Some(b) = self.peek() {
                    if b.is_ascii_alphanumeric() || b == b'_' {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
            Some(b)
                if b == b'?'
                    || b == b'!'
                    || b == b'#'
                    || b == b'$'
                    || b == b'@'
                    || b == b'*'
                    || b == b'-'
                    || b.is_ascii_digit() =>
            {
                self.pos += 1;
            }
            _ => {} // lone $
        }
        Ok(())
    }

    /// Consume until matching `)` for `$(...)`, tracking nested parens and quotes.
    fn consume_command_subst(&mut self) -> Result<(), LexerError> {
        let start = self.pos.saturating_sub(2);
        let mut depth: u32 = 1;
        loop {
            match self.peek() {
                None => {
                    return Err(LexerError {
                        message: "unterminated command substitution".into(),
                        span: self.span_from(start),
                    });
                }
                Some(b'(') => {
                    depth += 1;
                    self.pos += 1;
                }
                Some(b')') => {
                    self.pos += 1;
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                Some(b'\'') => self.consume_single_quoted()?,
                Some(b'"') => self.consume_double_quoted()?,
                Some(b'\\') => self.consume_backslash(),
                Some(b'$') => self.consume_dollar()?,
                Some(_) => self.pos += 1,
            }
        }
    }

    /// Consume until matching `))` for `$((...))`.
    fn consume_arithmetic(&mut self) -> Result<(), LexerError> {
        let start = self.pos.saturating_sub(3);
        let mut depth: u32 = 1;
        loop {
            match self.peek() {
                None => {
                    return Err(LexerError {
                        message: "unterminated arithmetic expansion".into(),
                        span: self.span_from(start),
                    });
                }
                Some(b'(') => {
                    self.pos += 1;
                    if self.peek() == Some(b'(') {
                        self.pos += 1;
                        depth += 1;
                    }
                }
                Some(b')') => {
                    self.pos += 1;
                    if self.peek() == Some(b')') {
                        self.pos += 1;
                        depth -= 1;
                        if depth == 0 {
                            return Ok(());
                        }
                    }
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    /// Consume until matching `}` for `${...}`, tracking nested braces and quotes.
    fn consume_brace_param(&mut self) -> Result<(), LexerError> {
        let start = self.pos.saturating_sub(2);
        let mut depth: u32 = 1;
        loop {
            match self.peek() {
                None => {
                    return Err(LexerError {
                        message: "unterminated parameter expansion".into(),
                        span: self.span_from(start),
                    });
                }
                Some(b'{') => {
                    depth += 1;
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                Some(b'\'') => self.consume_single_quoted()?,
                Some(b'"') => self.consume_double_quoted()?,
                Some(b'\\') => self.consume_backslash(),
                Some(_) => self.pos += 1,
            }
        }
    }

    /// Consume an extglob pattern `?(...)`, `*(...)`, `+(...)`, `@(...)`, `!(...)`.
    /// Called when pos is at the operator char (`?`, `*`, `+`, `@`, `!`) and
    /// the next byte is `(`.
    fn consume_extglob(&mut self) -> Result<(), LexerError> {
        let start = self.pos;
        self.pos += 2; // skip operator + (
        let mut depth: u32 = 1;
        loop {
            match self.peek() {
                None => {
                    return Err(LexerError {
                        message: "unterminated extglob pattern".into(),
                        span: self.span_from(start),
                    });
                }
                Some(b'(') => {
                    depth += 1;
                    self.pos += 1;
                }
                Some(b')') => {
                    self.pos += 1;
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                Some(b'\\') => self.consume_backslash(),
                Some(_) => self.pos += 1,
            }
        }
    }

    /// Read a word token, handling quotes and dollar expansions within the word.
    fn read_word(&mut self) -> Result<Token, LexerError> {
        let start = self.pos;
        let source_str =
            std::str::from_utf8(self.source).expect("lexer source must be valid UTF-8");

        loop {
            match self.peek() {
                None => break,
                Some(b) if is_word_break(b) => break,
                Some(b'\'') => self.consume_single_quoted()?,
                Some(b'"') => self.consume_double_quoted()?,
                Some(b'\\') => self.consume_backslash(),
                Some(b'$') => {
                    // $"..." locale quoting at the top level of a word
                    if self.peek_ahead(1) == Some(b'"') {
                        self.pos += 1; // skip $
                        self.consume_double_quoted()?;
                    } else {
                        self.consume_dollar()?;
                    }
                }
                Some(_) => {
                    // Check for extglob: `?(`, `*(`, `+(`, `@(`, `!(` patterns.
                    // These operators followed by `(` should consume the entire
                    // pattern including `|` and `)` as part of the word.
                    let cur = self.source[self.pos];
                    if matches!(cur, b'?' | b'*' | b'+' | b'@' | b'!')
                        && self.peek_ahead(1) == Some(b'(')
                    {
                        self.consume_extglob()?;
                    } else {
                        self.pos += 1;
                    }
                }
            }
        }

        let text = &source_str[start..self.pos];
        let span = self.span_from(start);
        let is_plain = !text.contains('\'')
            && !text.contains('"')
            && !text.contains('\\')
            && !text.contains('$');

        // Emit dedicated tokens for `[[` and `]]`
        if is_plain && text == "[[" {
            return Ok(Token {
                kind: TokenKind::DblLBracket,
                span,
            });
        }
        if is_plain && text == "]]" {
            return Ok(Token {
                kind: TokenKind::DblRBracket,
                span,
            });
        }

        let is_candidate = is_plain && is_reserved_word(text);

        Ok(Token {
            kind: TokenKind::Word {
                is_reserved_candidate: is_candidate,
            },
            span,
        })
    }

    /// Produce the next token.
    pub fn next_token(&mut self) -> Result<Token, LexerError> {
        loop {
            match self.mode {
                LexerMode::Comment => {
                    self.consume_comment();
                    self.mode = LexerMode::Normal;
                }
                LexerMode::Normal => {
                    self.skip_blanks();

                    let Some(b) = self.peek() else {
                        return Ok(Token {
                            kind: TokenKind::Eof,
                            span: self.span_from(self.pos),
                        });
                    };

                    match b {
                        b'\n' => return Ok(self.single_op(TokenKind::Newline)),
                        b';' => return Ok(self.single_op(TokenKind::Semi)),
                        b'(' => return Ok(self.single_op(TokenKind::LParen)),
                        b')' => return Ok(self.single_op(TokenKind::RParen)),

                        b'#' => {
                            self.mode = LexerMode::Comment;
                        }

                        b'&' => {
                            if self.peek_ahead(1) == Some(b'&') {
                                return Ok(self.double_op(TokenKind::AndAnd));
                            }
                            if self.peek_ahead(1) == Some(b'>') {
                                return Ok(self.double_op(TokenKind::AmpGreater));
                            }
                            return Ok(self.single_op(TokenKind::Amp));
                        }

                        b'|' => {
                            if self.peek_ahead(1) == Some(b'|') {
                                return Ok(self.double_op(TokenKind::OrOr));
                            }
                            if self.peek_ahead(1) == Some(b'&') {
                                return Ok(self.double_op(TokenKind::PipeAmp));
                            }
                            return Ok(self.single_op(TokenKind::Pipe));
                        }

                        b'>' => {
                            if self.peek_ahead(1) == Some(b'>') {
                                return Ok(self.double_op(TokenKind::GreaterGreater));
                            }
                            return Ok(self.single_op(TokenKind::Greater));
                        }

                        b'<' => match self.peek_ahead(1) {
                            Some(b'<') => {
                                if self.peek_ahead(2) == Some(b'<') {
                                    return Ok(self.triple_op(TokenKind::LessLessLess));
                                }
                                if self.peek_ahead(2) == Some(b'-') {
                                    return Ok(self.triple_op(TokenKind::LessLessDash));
                                }
                                return Ok(self.double_op(TokenKind::LessLess));
                            }
                            Some(b'>') => {
                                return Ok(self.double_op(TokenKind::LessGreater));
                            }
                            _ => {
                                return Ok(self.single_op(TokenKind::Less));
                            }
                        },

                        _ => {
                            return self.read_word();
                        }
                    }
                }
            }
        }
    }

    /// Tokenize the entire source into a vector of tokens (excluding Eof).
    pub fn tokenize_all(&mut self) -> Result<Vec<Token>, LexerError> {
        let mut tokens = Vec::new();
        loop {
            let tok = self.next_token()?;
            if tok.kind == TokenKind::Eof {
                break;
            }
            tokens.push(tok);
        }
        Ok(tokens)
    }
}

fn is_word_break(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'<' | b'>' | b'(' | b')' | b'#'
    )
}

/// Convenience function: tokenize a source string into all tokens.
pub fn tokenize(source: &str) -> Result<Vec<Token>, LexerError> {
    Lexer::new(source).tokenize_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<TokenKind> {
        tokenize(source)
            .unwrap()
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    fn tokens_with_text(source: &str) -> Vec<(TokenKind, String)> {
        let toks = tokenize(source).unwrap();
        toks.into_iter()
            .map(|t| (t.kind.clone(), t.text(source).to_string()))
            .collect()
    }

    fn word(reserved: bool) -> TokenKind {
        TokenKind::Word {
            is_reserved_candidate: reserved,
        }
    }

    // --- Basic tokens (unchanged from before) ---

    #[test]
    fn lexer_starts_in_normal_mode() {
        let lexer = Lexer::new("");
        assert_eq!(lexer.mode(), LexerMode::Normal);
        assert_eq!(lexer.position(), 0);
    }

    #[test]
    fn empty_input() {
        assert!(tokenize("").unwrap().is_empty());
    }

    #[test]
    fn separators() {
        assert_eq!(
            kinds(";\n; | || & && ( )"),
            vec![
                TokenKind::Semi,
                TokenKind::Newline,
                TokenKind::Semi,
                TokenKind::Pipe,
                TokenKind::OrOr,
                TokenKind::Amp,
                TokenKind::AndAnd,
                TokenKind::LParen,
                TokenKind::RParen,
            ]
        );
    }

    #[test]
    fn redirections() {
        assert_eq!(
            kinds("< > >> << <<- <>"),
            vec![
                TokenKind::Less,
                TokenKind::Greater,
                TokenKind::GreaterGreater,
                TokenKind::LessLess,
                TokenKind::LessLessDash,
                TokenKind::LessGreater,
            ]
        );
    }

    #[test]
    fn comments() {
        assert_eq!(
            kinds("echo # comment\nhello"),
            vec![word(false), TokenKind::Newline, word(false)]
        );
    }

    #[test]
    fn simple_words() {
        let toks = tokens_with_text("echo hello world");
        assert_eq!(toks[0].1, "echo");
        assert_eq!(toks[1].1, "hello");
        assert_eq!(toks[2].1, "world");
    }

    #[test]
    fn reserved_words() {
        assert_eq!(
            kinds("if then fi"),
            vec![word(true), word(true), word(true)]
        );
    }

    #[test]
    fn spans_accurate() {
        let source = "echo hello";
        let toks = tokenize(source).unwrap();
        assert_eq!(toks[0].span, Span { start: 0, end: 4 });
        assert_eq!(toks[1].span, Span { start: 5, end: 10 });
    }

    // --- Quoting ---

    #[test]
    fn single_quoted_word() {
        let toks = tokens_with_text("echo 'hello world'");
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[1].1, "'hello world'");
    }

    #[test]
    fn single_quoted_preserves_operators() {
        // Operators inside quotes should not break the word
        let toks = tokens_with_text("echo 'a;b|c&d'");
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[1].1, "'a;b|c&d'");
    }

    #[test]
    fn double_quoted_word() {
        let toks = tokens_with_text(r#"echo "hello world""#);
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[1].1, "\"hello world\"");
    }

    #[test]
    fn double_quoted_with_dollar() {
        let toks = tokens_with_text(r#"echo "hello $USER""#);
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[1].1, "\"hello $USER\"");
    }

    #[test]
    fn mixed_quoting() {
        let toks = tokens_with_text("echo hello'world'\"!\"");
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[1].1, "hello'world'\"!\"");
    }

    #[test]
    fn quoted_word_not_reserved() {
        // 'if' should not be flagged as reserved
        let toks = kinds("'if'");
        assert_eq!(toks, vec![word(false)]);
    }

    // --- Dollar expansions ---

    #[test]
    fn dollar_variable() {
        let toks = tokens_with_text("echo $HOME/bin");
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[1].1, "$HOME/bin");
    }

    #[test]
    fn dollar_brace() {
        let toks = tokens_with_text("echo ${HOME}");
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[1].1, "${HOME}");
    }

    #[test]
    fn dollar_brace_with_default() {
        let toks = tokens_with_text("echo ${FOO:-bar}");
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[1].1, "${FOO:-bar}");
    }

    #[test]
    fn command_substitution() {
        let toks = tokens_with_text("echo $(ls -la)");
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[1].1, "$(ls -la)");
    }

    #[test]
    fn arithmetic_expansion() {
        let toks = tokens_with_text("echo $((1+2))");
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[1].1, "$((1+2))");
    }

    #[test]
    fn nested_command_subst_in_double_quote() {
        let toks = tokens_with_text(r#"echo "$(echo hi)""#);
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[1].1, "\"$(echo hi)\"");
    }

    // --- Backslash ---

    #[test]
    fn backslash_escape() {
        let toks = tokens_with_text(r"echo hello\ world");
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[1].1, r"hello\ world");
    }

    // --- Errors ---

    #[test]
    fn unterminated_single_quote() {
        assert!(tokenize("echo 'hello").is_err());
    }

    #[test]
    fn unterminated_double_quote() {
        assert!(tokenize("echo \"hello").is_err());
    }

    #[test]
    fn unterminated_command_subst() {
        assert!(tokenize("echo $(hello").is_err());
    }

    // --- Here-string (<<<) ---

    #[test]
    fn here_string_token() {
        assert_eq!(kinds("<<<"), vec![TokenKind::LessLessLess]);
    }

    #[test]
    fn here_string_with_word() {
        assert_eq!(
            kinds("<<< hello"),
            vec![TokenKind::LessLessLess, word(false)]
        );
    }

    // --- Amp-greater (&>) ---

    #[test]
    fn amp_greater_token() {
        assert_eq!(kinds("&>"), vec![TokenKind::AmpGreater]);
    }

    #[test]
    fn amp_greater_with_word() {
        assert_eq!(
            kinds("echo &> file"),
            vec![word(false), TokenKind::AmpGreater, word(false)]
        );
    }

    // --- ANSI-C quoting ($'...') ---

    #[test]
    fn ansi_c_quote_is_word() {
        let toks = tokens_with_text("$'hello'");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].1, "$'hello'");
    }

    #[test]
    fn ansi_c_quote_with_escapes() {
        let toks = tokens_with_text("$'a\\nb'");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].1, "$'a\\nb'");
    }

    // --- Double bracket tokens ---

    #[test]
    fn double_bracket_tokens() {
        assert_eq!(
            kinds("[[ x ]]"),
            vec![TokenKind::DblLBracket, word(false), TokenKind::DblRBracket]
        );
    }

    #[test]
    fn double_bracket_with_operators() {
        assert_eq!(
            kinds("[[ $x == hello ]]"),
            vec![
                TokenKind::DblLBracket,
                word(false),
                word(false),
                word(false),
                TokenKind::DblRBracket,
            ]
        );
    }

    #[test]
    fn double_bracket_reserved_word() {
        // [[ and ]] should be reserved words in the lexer
        assert!(is_reserved_word("[["));
        assert!(is_reserved_word("]]"));
    }

    // --- Pipe-ampersand (|&) ---

    #[test]
    fn pipe_amp_token() {
        assert_eq!(kinds("|&"), vec![TokenKind::PipeAmp]);
    }

    #[test]
    fn pipe_amp_in_pipeline() {
        assert_eq!(
            kinds("a |& b"),
            vec![word(false), TokenKind::PipeAmp, word(false)]
        );
    }

    // --- Extglob patterns ---

    #[test]
    fn extglob_at_pattern_is_single_word() {
        let toks = tokens_with_text("*.@(jpg|png)");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].1, "*.@(jpg|png)");
    }

    #[test]
    fn extglob_not_pattern_is_single_word() {
        let toks = tokens_with_text("!(*.log)");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].1, "!(*.log)");
    }

    #[test]
    fn extglob_question_pattern_is_single_word() {
        let toks = tokens_with_text("colo?(u)r");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].1, "colo?(u)r");
    }

    // --- Locale quoting ---

    #[test]
    fn locale_quoting_is_word() {
        let toks = tokens_with_text("$\"hello\"");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].1, "$\"hello\"");
    }
}
