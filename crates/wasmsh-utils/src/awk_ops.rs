//! Awk utility: awk.

use std::collections::HashMap;

use crate::helpers::{emit_error, read_text, resolve_path};
use crate::UtilContext;

// ---------------------------------------------------------------------------
// Value
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum AwkValue {
    Str(String),
    Num(f64),
    Uninitialized,
}

impl AwkValue {
    fn to_num(&self) -> f64 {
        match self {
            Self::Num(n) => *n,
            Self::Str(s) => parse_leading_num(s),
            Self::Uninitialized => 0.0,
        }
    }

    fn to_str(&self) -> String {
        match self {
            Self::Str(s) => s.clone(),
            Self::Num(n) => format_num(*n),
            Self::Uninitialized => String::new(),
        }
    }

    fn is_truthy(&self) -> bool {
        match self {
            Self::Num(n) => *n != 0.0,
            Self::Str(s) => !s.is_empty(),
            Self::Uninitialized => false,
        }
    }
}

/// Format a number: integers without decimal point, floats with up to 6 digits.
fn format_num(n: f64) -> String {
    if n.is_infinite() {
        if n > 0.0 {
            return "inf".to_string();
        }
        return "-inf".to_string();
    }
    if n.is_nan() {
        return "nan".to_string();
    }
    // Use OFMT-like formatting: if integer, print as integer
    #[allow(clippy::float_cmp)]
    let is_int = n == n.floor() && n.abs() < 1e15;
    if is_int {
        #[allow(clippy::cast_possible_truncation)]
        let i = n as i64;
        format!("{i}")
    } else {
        // %.6g style
        let s = format!("{n:.6}");
        // Trim trailing zeros after decimal point
        if s.contains('.') {
            let trimmed = s.trim_end_matches('0');
            let trimmed = trimmed.trim_end_matches('.');
            trimmed.to_string()
        } else {
            s
        }
    }
}

/// Parse leading numeric value from a string (awk coercion).
fn parse_leading_num(s: &str) -> f64 {
    let s = s.trim();
    if s.is_empty() {
        return 0.0;
    }
    // Try full parse first
    if let Ok(n) = s.parse::<f64>() {
        return n;
    }
    // Try leading numeric portion
    let mut end = 0;
    let bytes = s.as_bytes();
    if end < bytes.len() && (bytes[end] == b'+' || bytes[end] == b'-') {
        end += 1;
    }
    let mut has_digit = false;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
        has_digit = true;
    }
    if end < bytes.len() && bytes[end] == b'.' {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
            has_digit = true;
        }
    }
    if has_digit {
        s[..end].parse::<f64>().unwrap_or(0.0)
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    // Literals
    Number(f64),
    StringLit(String),
    Regex(String),
    // Identifiers / keywords
    Ident(String),
    // Keywords
    Begin,
    End,
    If,
    Else,
    While,
    For,
    Do,
    Break,
    Continue,
    Next,
    Exit,
    Delete,
    In,
    Print,
    Printf,
    Getline,
    Function,
    Return,
    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    Assign,
    PlusAssign,
    MinusAssign,
    StarAssign,
    SlashAssign,
    PercentAssign,
    CaretAssign,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Match,
    NotMatch,
    And,
    Or,
    Not,
    Incr,
    Decr,
    Question,
    Colon,
    Dollar,
    Comma,
    Semicolon,
    Newline,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Append, // >>
    Pipe,   // |
    // Special
    Eof,
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
}

impl Lexer {
    fn new(src: &str) -> Self {
        Self {
            chars: src.chars().collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied()?;
        self.pos += 1;
        Some(c)
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn skip_whitespace_no_newline(&mut self) {
        while let Some(c) = self.peek() {
            if c == ' ' || c == '\t' || c == '\r' {
                self.advance();
            } else if c == '\\' && self.peek_at(1) == Some('\n') {
                // Line continuation
                self.advance();
                self.advance();
            } else if c == '#' {
                // Comment: skip to end of line
                while let Some(c2) = self.peek() {
                    if c2 == '\n' {
                        break;
                    }
                    self.advance();
                }
            } else {
                break;
            }
        }
    }

    /// Whether the given previous token allows a regex to follow.
    fn can_start_regex(prev: &Token) -> bool {
        matches!(
            prev,
            Token::Eof
                | Token::Newline
                | Token::Semicolon
                | Token::LBrace
                | Token::RBrace
                | Token::LParen
                | Token::Comma
                | Token::Not
                | Token::And
                | Token::Or
                | Token::Match
                | Token::NotMatch
                | Token::Print
                | Token::Printf
                | Token::If
                | Token::While
                | Token::For
                | Token::Do
                | Token::Return
        )
    }

    fn tokenize(&mut self) -> Result<Vec<Token>, String> {
        let mut tokens = Vec::new();
        let mut prev = Token::Eof;

        loop {
            self.skip_whitespace_no_newline();
            let Some(c) = self.peek() else {
                tokens.push(Token::Eof);
                break;
            };

            let tok = match c {
                '\n' => {
                    self.advance();
                    // Collapse multiple newlines
                    while self.peek() == Some('\n') {
                        self.advance();
                    }
                    Token::Newline
                }
                '/' if Lexer::can_start_regex(&prev) => {
                    self.advance(); // consume opening /
                    let mut pat = String::new();
                    let mut escaped = false;
                    loop {
                        match self.advance() {
                            None => return Err("unterminated regex".to_string()),
                            Some('\\') if !escaped => {
                                escaped = true;
                                pat.push('\\');
                            }
                            Some('/') if !escaped => break,
                            Some(ch) => {
                                escaped = false;
                                pat.push(ch);
                            }
                        }
                    }
                    Token::Regex(pat)
                }
                '"' => {
                    self.advance();
                    let mut s = String::new();
                    loop {
                        match self.advance() {
                            None => return Err("unterminated string".to_string()),
                            Some('"') => break,
                            Some('\\') => match self.advance() {
                                Some('n') => s.push('\n'),
                                Some('t') => s.push('\t'),
                                Some('\\') => s.push('\\'),
                                Some('"') => s.push('"'),
                                Some('/') => s.push('/'),
                                Some('a') => s.push('\x07'),
                                Some('b') => s.push('\x08'),
                                Some('r') => s.push('\r'),
                                Some(ch) => {
                                    s.push('\\');
                                    s.push(ch);
                                }
                                None => return Err("unterminated string escape".to_string()),
                            },
                            Some(ch) => s.push(ch),
                        }
                    }
                    Token::StringLit(s)
                }
                '0'..='9' | '.' if c == '.' && !matches!(self.peek_at(1), Some('0'..='9')) => {
                    // This is just a dot, not a number
                    self.advance();
                    Token::StringLit(".".to_string())
                }
                '0'..='9' | '.' => {
                    let mut num = String::new();
                    // Handle hex
                    if c == '0' && matches!(self.peek_at(1), Some('x' | 'X')) {
                        num.push(self.advance().unwrap());
                        num.push(self.advance().unwrap());
                        while let Some(ch) = self.peek() {
                            if ch.is_ascii_hexdigit() {
                                num.push(self.advance().unwrap());
                            } else {
                                break;
                            }
                        }
                        let val = i64::from_str_radix(&num[2..], 16).map_err(|e| e.to_string())?;
                        #[allow(clippy::cast_precision_loss)]
                        Token::Number(val as f64)
                    } else {
                        while let Some(ch) = self.peek() {
                            if ch.is_ascii_digit() || ch == '.' {
                                num.push(self.advance().unwrap());
                            } else {
                                break;
                            }
                        }
                        // Exponent
                        if matches!(self.peek(), Some('e' | 'E')) {
                            num.push(self.advance().unwrap());
                            if matches!(self.peek(), Some('+' | '-')) {
                                num.push(self.advance().unwrap());
                            }
                            while let Some(ch) = self.peek() {
                                if ch.is_ascii_digit() {
                                    num.push(self.advance().unwrap());
                                } else {
                                    break;
                                }
                            }
                        }
                        let val: f64 = num.parse().map_err(|e: std::num::ParseFloatError| {
                            format!("bad number '{num}': {e}")
                        })?;
                        Token::Number(val)
                    }
                }
                'a'..='z' | 'A'..='Z' | '_' => {
                    let mut id = String::new();
                    while let Some(ch) = self.peek() {
                        if ch.is_alphanumeric() || ch == '_' {
                            id.push(self.advance().unwrap());
                        } else {
                            break;
                        }
                    }
                    match id.as_str() {
                        "BEGIN" => Token::Begin,
                        "END" => Token::End,
                        "if" => Token::If,
                        "else" => Token::Else,
                        "while" => Token::While,
                        "for" => Token::For,
                        "do" => Token::Do,
                        "break" => Token::Break,
                        "continue" => Token::Continue,
                        "next" => Token::Next,
                        "exit" => Token::Exit,
                        "delete" => Token::Delete,
                        "in" => Token::In,
                        "print" => Token::Print,
                        "printf" => Token::Printf,
                        "getline" => Token::Getline,
                        "function" => Token::Function,
                        "return" => Token::Return,
                        _ => Token::Ident(id),
                    }
                }
                '+' => {
                    self.advance();
                    if self.peek() == Some('+') {
                        self.advance();
                        Token::Incr
                    } else if self.peek() == Some('=') {
                        self.advance();
                        Token::PlusAssign
                    } else {
                        Token::Plus
                    }
                }
                '-' => {
                    self.advance();
                    if self.peek() == Some('-') {
                        self.advance();
                        Token::Decr
                    } else if self.peek() == Some('=') {
                        self.advance();
                        Token::MinusAssign
                    } else {
                        Token::Minus
                    }
                }
                '*' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        Token::StarAssign
                    } else {
                        Token::Star
                    }
                }
                '/' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        Token::SlashAssign
                    } else {
                        Token::Slash
                    }
                }
                '%' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        Token::PercentAssign
                    } else {
                        Token::Percent
                    }
                }
                '^' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        Token::CaretAssign
                    } else {
                        Token::Caret
                    }
                }
                '=' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        Token::Eq
                    } else {
                        Token::Assign
                    }
                }
                '!' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        Token::Ne
                    } else if self.peek() == Some('~') {
                        self.advance();
                        Token::NotMatch
                    } else {
                        Token::Not
                    }
                }
                '<' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        Token::Le
                    } else {
                        Token::Lt
                    }
                }
                '>' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        Token::Ge
                    } else if self.peek() == Some('>') {
                        self.advance();
                        Token::Append
                    } else {
                        Token::Gt
                    }
                }
                '~' => {
                    self.advance();
                    Token::Match
                }
                '&' => {
                    self.advance();
                    if self.peek() == Some('&') {
                        self.advance();
                        Token::And
                    } else {
                        // Single & not used in awk expressions normally
                        return Err("unexpected '&'".to_string());
                    }
                }
                '|' => {
                    self.advance();
                    if self.peek() == Some('|') {
                        self.advance();
                        Token::Or
                    } else {
                        Token::Pipe
                    }
                }
                '?' => {
                    self.advance();
                    Token::Question
                }
                ':' => {
                    self.advance();
                    Token::Colon
                }
                '$' => {
                    self.advance();
                    Token::Dollar
                }
                ',' => {
                    self.advance();
                    Token::Comma
                }
                ';' => {
                    self.advance();
                    Token::Semicolon
                }
                '(' => {
                    self.advance();
                    Token::LParen
                }
                ')' => {
                    self.advance();
                    Token::RParen
                }
                '{' => {
                    self.advance();
                    Token::LBrace
                }
                '}' => {
                    self.advance();
                    Token::RBrace
                }
                '[' => {
                    self.advance();
                    Token::LBracket
                }
                ']' => {
                    self.advance();
                    Token::RBracket
                }
                _ => {
                    return Err(format!("unexpected character '{c}'"));
                }
            };

            prev = tok.clone();
            tokens.push(tok);
        }

        Ok(tokens)
    }
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum AwkPattern {
    /// Match every record
    All,
    /// Expression that evaluates to truthy
    Expr(Expr),
    /// /regex/
    Regex(String),
    /// Range: pat1, pat2
    Range(Box<AwkPattern>, Box<AwkPattern>),
}

#[derive(Debug, Clone)]
enum Expr {
    Num(f64),
    Str(String),
    Regex(String),
    Var(String),
    FieldRef(Box<Expr>),
    ArrayRef(String, Box<Expr>),
    Assign(Box<Expr>, Box<Expr>),
    OpAssign(BinOp, Box<Expr>, Box<Expr>),
    BinOp(BinOp, Box<Expr>, Box<Expr>),
    UnaryOp(UnaryOp, Box<Expr>),
    PreIncr(Box<Expr>),
    PreDecr(Box<Expr>),
    PostIncr(Box<Expr>),
    PostDecr(Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    /// Regex match: expr ~ /re/ or expr !~ /re/
    MatchOp(bool, Box<Expr>, String),
    /// String concatenation
    Concat(Box<Expr>, Box<Expr>),
    /// `expr in array`
    InArray(Box<Expr>, String),
    /// Function call
    Call(String, Vec<Expr>),
}

#[derive(Debug, Clone, Copy)]
enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, Copy)]
enum UnaryOp {
    Neg,
    Not,
    Pos,
}

#[derive(Debug, Clone)]
enum Stmt {
    Expr(Expr),
    Print(Vec<Expr>, Option<Box<Expr>>),
    Printf(Vec<Expr>, Option<Box<Expr>>),
    If(Expr, Vec<Stmt>, Option<Vec<Stmt>>),
    While(Expr, Vec<Stmt>),
    DoWhile(Vec<Stmt>, Expr),
    For(
        Option<Box<Stmt>>,
        Option<Expr>,
        Option<Box<Stmt>>,
        Vec<Stmt>,
    ),
    ForIn(String, String, Vec<Stmt>),
    Break,
    Continue,
    Next,
    Exit(Option<Expr>),
    Return(Option<Expr>),
    Delete(String, Expr),
    Block(Vec<Stmt>),
}

#[derive(Debug, Clone)]
struct AwkRule {
    pattern: AwkPattern,
    action: Vec<Stmt>,
}

#[derive(Debug, Clone)]
struct AwkFunction {
    params: Vec<String>,
    body: Vec<Stmt>,
}

#[derive(Debug)]
struct AwkProgram {
    begin: Vec<Stmt>,
    rules: Vec<AwkRule>,
    end: Vec<Stmt>,
    functions: HashMap<String, AwkFunction>,
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }

    fn advance(&mut self) -> Token {
        let tok = self.tokens.get(self.pos).cloned().unwrap_or(Token::Eof);
        self.pos += 1;
        tok
    }

    fn expect(&mut self, expected: &Token) -> Result<(), String> {
        let tok = self.advance();
        if std::mem::discriminant(&tok) == std::mem::discriminant(expected) {
            Ok(())
        } else {
            Err(format!("expected {expected:?}, got {tok:?}"))
        }
    }

    fn skip_terminators(&mut self) {
        while matches!(self.peek(), Token::Newline | Token::Semicolon) {
            self.advance();
        }
    }

    fn parse_program(&mut self) -> Result<AwkProgram, String> {
        let mut begin = Vec::new();
        let mut rules = Vec::new();
        let mut end = Vec::new();
        let mut functions = HashMap::new();

        self.skip_terminators();

        while *self.peek() != Token::Eof {
            match self.peek().clone() {
                Token::Function => {
                    self.advance();
                    let name = match self.advance() {
                        Token::Ident(n) => n,
                        t => return Err(format!("expected function name, got {t:?}")),
                    };
                    self.expect(&Token::LParen)?;
                    let mut params = Vec::new();
                    while *self.peek() != Token::RParen {
                        match self.advance() {
                            Token::Ident(p) => params.push(p),
                            t => return Err(format!("expected parameter name, got {t:?}")),
                        }
                        if *self.peek() == Token::Comma {
                            self.advance();
                        }
                    }
                    self.expect(&Token::RParen)?;
                    self.skip_terminators();
                    let body = self.parse_action()?;
                    functions.insert(name, AwkFunction { params, body });
                }
                Token::Begin => {
                    self.advance();
                    self.skip_terminators();
                    let stmts = self.parse_action()?;
                    begin.extend(stmts);
                }
                Token::End => {
                    self.advance();
                    self.skip_terminators();
                    let stmts = self.parse_action()?;
                    end.extend(stmts);
                }
                Token::LBrace => {
                    // No pattern - matches all
                    let action = self.parse_action()?;
                    rules.push(AwkRule {
                        pattern: AwkPattern::All,
                        action,
                    });
                }
                _ => {
                    // Pattern [, pattern] [{action}]
                    let pat = self.parse_pattern()?;
                    self.skip_terminators();
                    if *self.peek() == Token::Comma {
                        // Range pattern
                        self.advance();
                        self.skip_terminators();
                        let pat2 = self.parse_pattern()?;
                        self.skip_terminators();
                        let action = if *self.peek() == Token::LBrace {
                            self.parse_action()?
                        } else {
                            // Default: print $0
                            vec![Stmt::Print(vec![], None)]
                        };
                        rules.push(AwkRule {
                            pattern: AwkPattern::Range(Box::new(pat), Box::new(pat2)),
                            action,
                        });
                    } else if *self.peek() == Token::LBrace {
                        let action = self.parse_action()?;
                        rules.push(AwkRule {
                            pattern: pat,
                            action,
                        });
                    } else {
                        // Pattern only - default action is print $0
                        rules.push(AwkRule {
                            pattern: pat,
                            action: vec![Stmt::Print(vec![], None)],
                        });
                    }
                }
            }
            self.skip_terminators();
        }

        Ok(AwkProgram {
            begin,
            rules,
            end,
            functions,
        })
    }

    fn parse_pattern(&mut self) -> Result<AwkPattern, String> {
        if let Token::Regex(re) = self.peek().clone() {
            self.advance();
            Ok(AwkPattern::Regex(re))
        } else {
            let expr = self.parse_expr()?;
            Ok(AwkPattern::Expr(expr))
        }
    }

    fn parse_action(&mut self) -> Result<Vec<Stmt>, String> {
        self.expect(&Token::LBrace)?;
        self.skip_terminators();
        let mut stmts = Vec::new();
        while *self.peek() != Token::RBrace && *self.peek() != Token::Eof {
            stmts.push(self.parse_stmt()?);
            self.skip_terminators();
        }
        self.expect(&Token::RBrace)?;
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt, String> {
        match self.peek().clone() {
            Token::If => self.parse_if(),
            Token::While => self.parse_while(),
            Token::Do => self.parse_do_while(),
            Token::For => self.parse_for(),
            Token::Break => {
                self.advance();
                Ok(Stmt::Break)
            }
            Token::Continue => {
                self.advance();
                Ok(Stmt::Continue)
            }
            Token::Next => {
                self.advance();
                Ok(Stmt::Next)
            }
            Token::Exit => {
                self.advance();
                let code = if !matches!(
                    self.peek(),
                    Token::Semicolon | Token::Newline | Token::RBrace | Token::Eof
                ) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                Ok(Stmt::Exit(code))
            }
            Token::Return => {
                self.advance();
                let val = if !matches!(
                    self.peek(),
                    Token::Semicolon | Token::Newline | Token::RBrace | Token::Eof
                ) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                Ok(Stmt::Return(val))
            }
            Token::Delete => {
                self.advance();
                let name = match self.advance() {
                    Token::Ident(n) => n,
                    t => return Err(format!("expected array name after delete, got {t:?}")),
                };
                self.expect(&Token::LBracket)?;
                let idx = self.parse_expr()?;
                self.expect(&Token::RBracket)?;
                Ok(Stmt::Delete(name, idx))
            }
            Token::Print => self.parse_print(),
            Token::Printf => self.parse_printf(),
            Token::LBrace => {
                let stmts = self.parse_action()?;
                Ok(Stmt::Block(stmts))
            }
            _ => {
                let expr = self.parse_expr()?;
                Ok(Stmt::Expr(expr))
            }
        }
    }

    fn parse_print(&mut self) -> Result<Stmt, String> {
        self.advance(); // consume 'print'
        let mut args = Vec::new();
        let mut redirect = None;

        if matches!(
            self.peek(),
            Token::Semicolon | Token::Newline | Token::RBrace | Token::Eof | Token::Pipe
        ) {
            // `print` with no args => print $0
        } else {
            args.push(self.parse_expr()?);
            while *self.peek() == Token::Comma {
                self.advance();
                args.push(self.parse_expr()?);
            }
        }

        // Handle output redirection: > file, >> file, | cmd
        if matches!(self.peek(), Token::Gt | Token::Append | Token::Pipe) {
            let _redir_tok = self.advance();
            redirect = Some(Box::new(self.parse_primary()?));
        }

        Ok(Stmt::Print(args, redirect))
    }

    fn parse_printf(&mut self) -> Result<Stmt, String> {
        self.advance(); // consume 'printf'
        let mut args = Vec::new();
        let mut redirect = None;

        args.push(self.parse_expr()?);
        while *self.peek() == Token::Comma {
            self.advance();
            args.push(self.parse_expr()?);
        }

        if matches!(self.peek(), Token::Gt | Token::Append | Token::Pipe) {
            let _redir_tok = self.advance();
            redirect = Some(Box::new(self.parse_primary()?));
        }

        Ok(Stmt::Printf(args, redirect))
    }

    fn parse_if(&mut self) -> Result<Stmt, String> {
        self.advance(); // consume 'if'
        self.expect(&Token::LParen)?;
        let cond = self.parse_expr()?;
        self.expect(&Token::RParen)?;
        self.skip_terminators();
        let then_body = if *self.peek() == Token::LBrace {
            self.parse_action()?
        } else {
            vec![self.parse_stmt()?]
        };
        self.skip_terminators();
        let else_body = if *self.peek() == Token::Else {
            self.advance();
            self.skip_terminators();
            if *self.peek() == Token::LBrace {
                Some(self.parse_action()?)
            } else {
                Some(vec![self.parse_stmt()?])
            }
        } else {
            None
        };
        Ok(Stmt::If(cond, then_body, else_body))
    }

    fn parse_while(&mut self) -> Result<Stmt, String> {
        self.advance(); // consume 'while'
        self.expect(&Token::LParen)?;
        let cond = self.parse_expr()?;
        self.expect(&Token::RParen)?;
        self.skip_terminators();
        let body = if *self.peek() == Token::LBrace {
            self.parse_action()?
        } else {
            vec![self.parse_stmt()?]
        };
        Ok(Stmt::While(cond, body))
    }

    fn parse_do_while(&mut self) -> Result<Stmt, String> {
        self.advance(); // consume 'do'
        self.skip_terminators();
        let body = if *self.peek() == Token::LBrace {
            self.parse_action()?
        } else {
            vec![self.parse_stmt()?]
        };
        self.skip_terminators();
        if !matches!(self.peek(), Token::While) {
            return Err("expected 'while' after do body".to_string());
        }
        self.advance();
        self.expect(&Token::LParen)?;
        let cond = self.parse_expr()?;
        self.expect(&Token::RParen)?;
        Ok(Stmt::DoWhile(body, cond))
    }

    fn parse_for(&mut self) -> Result<Stmt, String> {
        self.advance(); // consume 'for'
        self.expect(&Token::LParen)?;

        // Check for `for (var in array)` pattern
        if let Token::Ident(name) = self.peek().clone() {
            let saved = self.pos;
            self.advance();
            if *self.peek() == Token::In {
                self.advance();
                let arr_name = match self.advance() {
                    Token::Ident(n) => n,
                    t => return Err(format!("expected array name in for-in, got {t:?}")),
                };
                self.expect(&Token::RParen)?;
                self.skip_terminators();
                let body = if *self.peek() == Token::LBrace {
                    self.parse_action()?
                } else {
                    vec![self.parse_stmt()?]
                };
                return Ok(Stmt::ForIn(name, arr_name, body));
            }
            // Rewind
            self.pos = saved;
        }

        // C-style for
        let init = if *self.peek() == Token::Semicolon {
            None
        } else {
            Some(Box::new(self.parse_stmt()?))
        };
        self.expect(&Token::Semicolon)?;

        let cond = if *self.peek() == Token::Semicolon {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.expect(&Token::Semicolon)?;

        let incr = if *self.peek() == Token::RParen {
            None
        } else {
            Some(Box::new(self.parse_stmt()?))
        };
        self.expect(&Token::RParen)?;

        self.skip_terminators();
        let body = if *self.peek() == Token::LBrace {
            self.parse_action()?
        } else {
            vec![self.parse_stmt()?]
        };

        Ok(Stmt::For(init, cond, incr, body))
    }

    // ---- Expression parsing (precedence climbing) ----

    fn parse_expr(&mut self) -> Result<Expr, String> {
        self.parse_assign()
    }

    fn parse_assign(&mut self) -> Result<Expr, String> {
        let lhs = self.parse_ternary()?;

        match self.peek().clone() {
            Token::Assign => {
                self.advance();
                let rhs = self.parse_assign()?;
                Ok(Expr::Assign(Box::new(lhs), Box::new(rhs)))
            }
            Token::PlusAssign => {
                self.advance();
                let rhs = self.parse_assign()?;
                Ok(Expr::OpAssign(BinOp::Add, Box::new(lhs), Box::new(rhs)))
            }
            Token::MinusAssign => {
                self.advance();
                let rhs = self.parse_assign()?;
                Ok(Expr::OpAssign(BinOp::Sub, Box::new(lhs), Box::new(rhs)))
            }
            Token::StarAssign => {
                self.advance();
                let rhs = self.parse_assign()?;
                Ok(Expr::OpAssign(BinOp::Mul, Box::new(lhs), Box::new(rhs)))
            }
            Token::SlashAssign => {
                self.advance();
                let rhs = self.parse_assign()?;
                Ok(Expr::OpAssign(BinOp::Div, Box::new(lhs), Box::new(rhs)))
            }
            Token::PercentAssign => {
                self.advance();
                let rhs = self.parse_assign()?;
                Ok(Expr::OpAssign(BinOp::Mod, Box::new(lhs), Box::new(rhs)))
            }
            Token::CaretAssign => {
                self.advance();
                let rhs = self.parse_assign()?;
                Ok(Expr::OpAssign(BinOp::Pow, Box::new(lhs), Box::new(rhs)))
            }
            _ => Ok(lhs),
        }
    }

    fn parse_ternary(&mut self) -> Result<Expr, String> {
        let cond = self.parse_or()?;
        if *self.peek() == Token::Question {
            self.advance();
            let then_expr = self.parse_expr()?;
            self.expect(&Token::Colon)?;
            let else_expr = self.parse_expr()?;
            Ok(Expr::Ternary(
                Box::new(cond),
                Box::new(then_expr),
                Box::new(else_expr),
            ))
        } else {
            Ok(cond)
        }
    }

    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_and()?;
        while *self.peek() == Token::Or {
            self.advance();
            let rhs = self.parse_and()?;
            lhs = Expr::BinOp(BinOp::Or, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_in_expr()?;
        while *self.peek() == Token::And {
            self.advance();
            let rhs = self.parse_in_expr()?;
            lhs = Expr::BinOp(BinOp::And, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_in_expr(&mut self) -> Result<Expr, String> {
        let lhs = self.parse_match()?;
        // Handle `expr in array`
        if *self.peek() == Token::In {
            self.advance();
            let arr = match self.advance() {
                Token::Ident(n) => n,
                t => return Err(format!("expected array name after 'in', got {t:?}")),
            };
            return Ok(Expr::InArray(Box::new(lhs), arr));
        }
        Ok(lhs)
    }

    fn parse_match(&mut self) -> Result<Expr, String> {
        let lhs = self.parse_comparison()?;
        match self.peek().clone() {
            Token::Match => {
                self.advance();
                match self.advance() {
                    Token::Regex(re) => Ok(Expr::MatchOp(true, Box::new(lhs), re)),
                    t => Err(format!("expected regex after ~, got {t:?}")),
                }
            }
            Token::NotMatch => {
                self.advance();
                match self.advance() {
                    Token::Regex(re) => Ok(Expr::MatchOp(false, Box::new(lhs), re)),
                    t => Err(format!("expected regex after !~, got {t:?}")),
                }
            }
            _ => Ok(lhs),
        }
    }

    fn parse_comparison(&mut self) -> Result<Expr, String> {
        let lhs = self.parse_concat()?;
        let op = match self.peek() {
            Token::Eq => BinOp::Eq,
            Token::Ne => BinOp::Ne,
            Token::Lt => BinOp::Lt,
            Token::Gt => BinOp::Gt,
            Token::Le => BinOp::Le,
            Token::Ge => BinOp::Ge,
            _ => return Ok(lhs),
        };
        self.advance();
        let rhs = self.parse_concat()?;
        Ok(Expr::BinOp(op, Box::new(lhs), Box::new(rhs)))
    }

    /// String concatenation: two adjacent expressions without an operator.
    fn parse_concat(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_addition()?;
        // Concatenation is any non-operator token that could start an expression
        while self.can_start_concat() {
            let rhs = self.parse_addition()?;
            lhs = Expr::Concat(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    /// Check if the next token can start a concatenation operand.
    fn can_start_concat(&self) -> bool {
        matches!(
            self.peek(),
            Token::Number(_)
                | Token::StringLit(_)
                | Token::Ident(_)
                | Token::Dollar
                | Token::LParen
                | Token::Not
                | Token::Incr
                | Token::Decr
        )
    }

    fn parse_addition(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_multiplication()?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_multiplication()?;
            lhs = Expr::BinOp(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_multiplication(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_power()?;
        loop {
            let op = match self.peek() {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                Token::Percent => BinOp::Mod,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_power()?;
            lhs = Expr::BinOp(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_power(&mut self) -> Result<Expr, String> {
        let base = self.parse_unary()?;
        if *self.peek() == Token::Caret {
            self.advance();
            let exp = self.parse_power()?; // Right-associative
            Ok(Expr::BinOp(BinOp::Pow, Box::new(base), Box::new(exp)))
        } else {
            Ok(base)
        }
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        match self.peek().clone() {
            Token::Not => {
                self.advance();
                let expr = self.parse_unary()?;
                Ok(Expr::UnaryOp(UnaryOp::Not, Box::new(expr)))
            }
            Token::Minus => {
                self.advance();
                let expr = self.parse_unary()?;
                Ok(Expr::UnaryOp(UnaryOp::Neg, Box::new(expr)))
            }
            Token::Plus => {
                self.advance();
                let expr = self.parse_unary()?;
                Ok(Expr::UnaryOp(UnaryOp::Pos, Box::new(expr)))
            }
            Token::Incr => {
                self.advance();
                let expr = self.parse_postfix()?;
                Ok(Expr::PreIncr(Box::new(expr)))
            }
            Token::Decr => {
                self.advance();
                let expr = self.parse_postfix()?;
                Ok(Expr::PreDecr(Box::new(expr)))
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_primary()?;
        loop {
            match self.peek().clone() {
                Token::Incr => {
                    self.advance();
                    expr = Expr::PostIncr(Box::new(expr));
                }
                Token::Decr => {
                    self.advance();
                    expr = Expr::PostDecr(Box::new(expr));
                }
                Token::LBracket => {
                    // Array subscript: ident[expr]
                    if let Expr::Var(name) = expr {
                        self.advance();
                        let idx = self.parse_expr()?;
                        self.expect(&Token::RBracket)?;
                        expr = Expr::ArrayRef(name, Box::new(idx));
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.peek().clone() {
            Token::Number(n) => {
                self.advance();
                Ok(Expr::Num(n))
            }
            Token::StringLit(s) => {
                self.advance();
                Ok(Expr::Str(s))
            }
            Token::Regex(re) => {
                self.advance();
                Ok(Expr::Regex(re))
            }
            Token::Dollar => {
                self.advance();
                let expr = self.parse_primary()?;
                Ok(Expr::FieldRef(Box::new(expr)))
            }
            Token::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                Ok(expr)
            }
            Token::Ident(name) => {
                self.advance();
                if *self.peek() == Token::LParen {
                    // Function call
                    self.advance();
                    let mut args = Vec::new();
                    if *self.peek() != Token::RParen {
                        args.push(self.parse_expr()?);
                        while *self.peek() == Token::Comma {
                            self.advance();
                            args.push(self.parse_expr()?);
                        }
                    }
                    self.expect(&Token::RParen)?;
                    Ok(Expr::Call(name, args))
                } else {
                    Ok(Expr::Var(name))
                }
            }
            t => Err(format!("unexpected token in expression: {t:?}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Simple regex engine
// ---------------------------------------------------------------------------

/// Simple regex matching supporting: literal chars, `.` (any), `*` (repeat prev),
/// `+` (one or more), `?` (zero or one), `^` (start), `$` (end), `[...]` char classes,
/// `[^...]` negated, `\d`, `\w`, `\s` and their negations, escape sequences.
fn regex_match(text: &str, pattern: &str) -> bool {
    let anchored_start = pattern.starts_with('^');
    let anchored_end = pattern.ends_with('$') && !pattern.ends_with("\\$");

    let pat = if anchored_start {
        &pattern[1..]
    } else {
        pattern
    };
    let pat = if anchored_end && !pat.is_empty() {
        &pat[..pat.len() - 1]
    } else {
        pat
    };

    let compiled = compile_regex(pat);

    if anchored_start && anchored_end {
        regex_match_here(&compiled, text, 0, 0) == Some(text.len())
    } else if anchored_start {
        regex_match_here(&compiled, text, 0, 0).is_some()
    } else if anchored_end {
        // Try matching at each position and check that it consumes to end
        for start in 0..=text.len() {
            if let Some(end) = regex_match_here(&compiled, text, start, 0) {
                if end == text.len() {
                    return true;
                }
            }
        }
        false
    } else {
        // Unanchored: try at each position
        for start in 0..=text.len() {
            if regex_match_here(&compiled, text, start, 0).is_some() {
                return true;
            }
        }
        false
    }
}

/// Find the first match of `pattern` in `text`, returning (start, end) byte offsets.
fn regex_find(text: &str, pattern: &str) -> Option<(usize, usize)> {
    let anchored_start = pattern.starts_with('^');
    let anchored_end = pattern.ends_with('$') && !pattern.ends_with("\\$");

    let pat = if anchored_start {
        &pattern[1..]
    } else {
        pattern
    };
    let pat = if anchored_end && !pat.is_empty() {
        &pat[..pat.len() - 1]
    } else {
        pat
    };

    let compiled = compile_regex(pat);

    if anchored_start {
        if let Some(end) = regex_match_here(&compiled, text, 0, 0) {
            if !anchored_end || end == text.len() {
                return Some((0, end));
            }
        }
        None
    } else {
        for start in 0..=text.len() {
            if let Some(end) = regex_match_here(&compiled, text, start, 0) {
                if !anchored_end || end == text.len() {
                    // Prefer non-empty matches unless at end
                    if start < end || start == text.len() {
                        return Some((start, end));
                    }
                }
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
enum RePiece {
    Literal(char),
    Dot,
    CharClass(Vec<CcRange>, bool), // ranges, negated
}

#[derive(Debug, Clone)]
enum ReNode {
    Piece(RePiece),
    Repeat(RePiece, RepeatKind),
}

#[derive(Debug, Clone, Copy)]
enum RepeatKind {
    Star,     // *
    Plus,     // +
    Question, // ?
}

#[derive(Debug, Clone)]
enum CcRange {
    Single(char),
    Range(char, char),
}

fn compile_regex(pat: &str) -> Vec<ReNode> {
    let chars: Vec<char> = pat.chars().collect();
    let mut nodes = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let piece = match chars[i] {
            '.' => {
                i += 1;
                RePiece::Dot
            }
            '\\' if i + 1 < chars.len() => {
                i += 1;
                let c = chars[i];
                i += 1;
                match c {
                    'd' => RePiece::CharClass(vec![CcRange::Range('0', '9')], false),
                    'D' => RePiece::CharClass(vec![CcRange::Range('0', '9')], true),
                    'w' => RePiece::CharClass(
                        vec![
                            CcRange::Range('a', 'z'),
                            CcRange::Range('A', 'Z'),
                            CcRange::Range('0', '9'),
                            CcRange::Single('_'),
                        ],
                        false,
                    ),
                    'W' => RePiece::CharClass(
                        vec![
                            CcRange::Range('a', 'z'),
                            CcRange::Range('A', 'Z'),
                            CcRange::Range('0', '9'),
                            CcRange::Single('_'),
                        ],
                        true,
                    ),
                    's' => RePiece::CharClass(
                        vec![
                            CcRange::Single(' '),
                            CcRange::Single('\t'),
                            CcRange::Single('\n'),
                            CcRange::Single('\r'),
                        ],
                        false,
                    ),
                    'S' => RePiece::CharClass(
                        vec![
                            CcRange::Single(' '),
                            CcRange::Single('\t'),
                            CcRange::Single('\n'),
                            CcRange::Single('\r'),
                        ],
                        true,
                    ),
                    't' => RePiece::Literal('\t'),
                    'n' => RePiece::Literal('\n'),
                    'r' => RePiece::Literal('\r'),
                    _ => RePiece::Literal(c),
                }
            }
            '[' => {
                i += 1;
                let negated = i < chars.len() && chars[i] == '^';
                if negated {
                    i += 1;
                }
                let mut ranges = Vec::new();
                // Handle ] as first char in class
                if i < chars.len() && chars[i] == ']' {
                    ranges.push(CcRange::Single(']'));
                    i += 1;
                }
                while i < chars.len() && chars[i] != ']' {
                    let ch = chars[i];
                    i += 1;
                    if i + 1 < chars.len() && chars[i] == '-' && chars[i + 1] != ']' {
                        let end_ch = chars[i + 1];
                        ranges.push(CcRange::Range(ch, end_ch));
                        i += 2;
                    } else {
                        ranges.push(CcRange::Single(ch));
                    }
                }
                if i < chars.len() {
                    i += 1; // consume ]
                }
                RePiece::CharClass(ranges, negated)
            }
            c => {
                i += 1;
                RePiece::Literal(c)
            }
        };

        // Check for quantifier
        if i < chars.len() {
            match chars[i] {
                '*' => {
                    i += 1;
                    nodes.push(ReNode::Repeat(piece, RepeatKind::Star));
                    continue;
                }
                '+' => {
                    i += 1;
                    nodes.push(ReNode::Repeat(piece, RepeatKind::Plus));
                    continue;
                }
                '?' => {
                    i += 1;
                    nodes.push(ReNode::Repeat(piece, RepeatKind::Question));
                    continue;
                }
                _ => {}
            }
        }

        nodes.push(ReNode::Piece(piece));
    }

    nodes
}

fn piece_matches(piece: &RePiece, ch: char) -> bool {
    match piece {
        RePiece::Literal(c) => ch == *c,
        RePiece::Dot => true,
        RePiece::CharClass(ranges, negated) => {
            let mut found = false;
            for r in ranges {
                match r {
                    CcRange::Single(c) => {
                        if ch == *c {
                            found = true;
                            break;
                        }
                    }
                    CcRange::Range(lo, hi) => {
                        if ch >= *lo && ch <= *hi {
                            found = true;
                            break;
                        }
                    }
                }
            }
            if *negated {
                !found
            } else {
                found
            }
        }
    }
}

/// Try matching the compiled regex nodes starting at `text[text_pos..]`, returning
/// the end position of the match if successful.
fn regex_match_here(
    nodes: &[ReNode],
    text: &str,
    text_pos: usize,
    node_idx: usize,
) -> Option<usize> {
    if node_idx >= nodes.len() {
        return Some(text_pos);
    }

    let text_bytes: Vec<char> = text.chars().collect();

    match &nodes[node_idx] {
        ReNode::Piece(piece) => {
            if text_pos < text_bytes.len() && piece_matches(piece, text_bytes[text_pos]) {
                // Calculate byte offset of next char
                let next_pos = text_pos + 1;
                regex_match_chars(nodes, &text_bytes, next_pos, node_idx + 1)
            } else {
                None
            }
        }
        ReNode::Repeat(piece, kind) => {
            regex_match_repeat_chars(nodes, &text_bytes, text_pos, node_idx, piece, *kind)
        }
    }
}

fn regex_match_chars(
    nodes: &[ReNode],
    chars: &[char],
    pos: usize,
    node_idx: usize,
) -> Option<usize> {
    if node_idx >= nodes.len() {
        return Some(pos);
    }

    match &nodes[node_idx] {
        ReNode::Piece(piece) => {
            if pos < chars.len() && piece_matches(piece, chars[pos]) {
                regex_match_chars(nodes, chars, pos + 1, node_idx + 1)
            } else {
                None
            }
        }
        ReNode::Repeat(piece, kind) => {
            regex_match_repeat_chars(nodes, chars, pos, node_idx, piece, *kind)
        }
    }
}

fn regex_match_repeat_chars(
    nodes: &[ReNode],
    chars: &[char],
    pos: usize,
    node_idx: usize,
    piece: &RePiece,
    kind: RepeatKind,
) -> Option<usize> {
    // Count max matches
    let mut count = 0;
    while pos + count < chars.len() && piece_matches(piece, chars[pos + count]) {
        count += 1;
    }

    let min = match kind {
        RepeatKind::Plus => 1,
        RepeatKind::Star | RepeatKind::Question => 0,
    };
    let max = match kind {
        RepeatKind::Question => count.min(1),
        _ => count,
    };

    // Greedy: try longest first
    let mut n = max;
    loop {
        if n < min {
            break;
        }
        if let Some(end) = regex_match_chars(nodes, chars, pos + n, node_idx + 1) {
            return Some(end);
        }
        if n == 0 {
            break;
        }
        n -= 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Interpreter
// ---------------------------------------------------------------------------

/// Control flow signal from statement execution.
enum ControlFlow {
    None,
    Break,
    Continue,
    Next,
    Exit(i32),
    Return(AwkValue),
}

struct AwkInterpreter {
    vars: HashMap<String, AwkValue>,
    arrays: HashMap<String, HashMap<String, AwkValue>>,
    /// Output buffer
    output_buf: Vec<u8>,
    /// Stderr buffer for warnings
    stderr_buf: Vec<u8>,
    /// Functions defined in the program
    functions: HashMap<String, AwkFunction>,
    /// Random state for `rand()/srand()`
    rand_state: u64,
    /// Exit code
    exit_code: i32,
    /// Current fields
    fields: Vec<String>,
    /// Range pattern state: tracking which ranges are active
    range_active: Vec<bool>,
}

impl AwkInterpreter {
    fn new() -> Self {
        let mut vars = HashMap::new();
        vars.insert("FS".to_string(), AwkValue::Str(" ".to_string()));
        vars.insert("RS".to_string(), AwkValue::Str("\n".to_string()));
        vars.insert("OFS".to_string(), AwkValue::Str(" ".to_string()));
        vars.insert("ORS".to_string(), AwkValue::Str("\n".to_string()));
        vars.insert("NR".to_string(), AwkValue::Num(0.0));
        vars.insert("NF".to_string(), AwkValue::Num(0.0));
        vars.insert("FNR".to_string(), AwkValue::Num(0.0));
        vars.insert("FILENAME".to_string(), AwkValue::Str(String::new()));

        Self {
            vars,
            arrays: HashMap::new(),
            output_buf: Vec::new(),
            stderr_buf: Vec::new(),
            functions: HashMap::new(),
            rand_state: 0x1234_5678_9abc_def0,
            exit_code: 0,
            fields: Vec::new(),
            range_active: Vec::new(),
        }
    }

    fn set_var(&mut self, name: &str, val: AwkValue) {
        self.vars.insert(name.to_string(), val);
    }

    fn get_var(&self, name: &str) -> AwkValue {
        self.vars
            .get(name)
            .cloned()
            .unwrap_or(AwkValue::Uninitialized)
    }

    fn get_fs(&self) -> String {
        self.get_var("FS").to_str()
    }

    fn get_ofs(&self) -> String {
        self.get_var("OFS").to_str()
    }

    fn get_ors(&self) -> String {
        self.get_var("ORS").to_str()
    }

    /// Split a record into fields based on FS.
    fn split_record(&self, record: &str) -> Vec<String> {
        let fs = self.get_fs();
        if fs == " " {
            // Default: split on runs of whitespace, trimming leading/trailing
            record.split_whitespace().map(String::from).collect()
        } else if fs.len() == 1 {
            record
                .split(fs.chars().next().unwrap())
                .map(String::from)
                .collect()
        } else {
            // Treat FS as a regex
            // Simple implementation: just split on the literal for now,
            // or if single char, use that
            record.split(&fs).map(String::from).collect()
        }
    }

    /// Set the current record ($0) and re-split fields.
    fn set_record(&mut self, record: &str) {
        let fields = self.split_record(record);
        self.fields = Vec::with_capacity(fields.len() + 1);
        self.fields.push(record.to_string()); // $0
        self.fields.extend(fields);
        #[allow(clippy::cast_precision_loss)]
        let nf = (self.fields.len() - 1) as f64;
        self.set_var("NF", AwkValue::Num(nf));
    }

    /// Rebuild $0 from fields using OFS.
    fn rebuild_record(&mut self) {
        let ofs = self.get_ofs();
        if self.fields.len() > 1 {
            self.fields[0] = self.fields[1..].join(&ofs);
        }
    }

    /// Get field by number.
    fn get_field(&self, n: usize) -> AwkValue {
        if n < self.fields.len() {
            AwkValue::Str(self.fields[n].clone())
        } else {
            AwkValue::Uninitialized
        }
    }

    /// Set field by number.
    fn set_field(&mut self, n: usize, val: String) {
        // Extend fields if necessary
        while self.fields.len() <= n {
            self.fields.push(String::new());
        }
        self.fields[n] = val;
        // Update NF
        #[allow(clippy::cast_precision_loss)]
        let nf = (self.fields.len() - 1) as f64;
        self.set_var("NF", AwkValue::Num(nf));
        // Rebuild $0
        if n > 0 {
            self.rebuild_record();
        } else {
            // $0 was set directly: re-split
            let record = self.fields[0].clone();
            let split = self.split_record(&record);
            self.fields.truncate(1);
            self.fields.extend(split);
            #[allow(clippy::cast_precision_loss)]
            let nf2 = (self.fields.len() - 1) as f64;
            self.set_var("NF", AwkValue::Num(nf2));
        }
    }

    /// Simple xorshift64 PRNG.
    fn next_rand(&mut self) -> f64 {
        let mut x = self.rand_state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rand_state = x;
        // Map to [0, 1)
        (x as u32) as f64 / (u32::MAX as f64)
    }

    fn write_output(&mut self, data: &[u8]) {
        self.output_buf.extend_from_slice(data);
    }

    // ---- Expression evaluation ----

    fn eval_expr(&mut self, expr: &Expr) -> AwkValue {
        match expr {
            Expr::Num(n) => AwkValue::Num(*n),
            Expr::Str(s) => AwkValue::Str(s.clone()),
            Expr::Regex(re) => {
                // Bare regex in expression context: test against $0
                let line = self.get_field(0).to_str();
                let matches = regex_match(&line, re);
                AwkValue::Num(if matches { 1.0 } else { 0.0 })
            }
            Expr::Var(name) => self.get_var(name),
            Expr::FieldRef(idx_expr) => {
                let idx = self.eval_expr(idx_expr).to_num();
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let n = idx.max(0.0) as usize;
                self.get_field(n)
            }
            Expr::ArrayRef(name, idx_expr) => {
                let key = self.eval_expr(idx_expr).to_str();
                self.arrays
                    .get(name)
                    .and_then(|m| m.get(&key))
                    .cloned()
                    .unwrap_or(AwkValue::Uninitialized)
            }
            Expr::Assign(lhs, rhs) => {
                let val = self.eval_expr(rhs);
                self.assign_to(lhs, val.clone());
                val
            }
            Expr::OpAssign(op, lhs, rhs) => {
                let old = self.eval_expr(lhs);
                let rval = self.eval_expr(rhs);
                let result = self.apply_binop(*op, &old, &rval);
                self.assign_to(lhs, result.clone());
                result
            }
            Expr::BinOp(op, lhs, rhs) => {
                let l = self.eval_expr(lhs);
                let r = self.eval_expr(rhs);
                self.apply_binop(*op, &l, &r)
            }
            Expr::UnaryOp(op, expr) => {
                let val = self.eval_expr(expr);
                match op {
                    UnaryOp::Neg => AwkValue::Num(-val.to_num()),
                    UnaryOp::Pos => AwkValue::Num(val.to_num()),
                    UnaryOp::Not => AwkValue::Num(if val.is_truthy() { 0.0 } else { 1.0 }),
                }
            }
            Expr::PreIncr(expr) => {
                let val = self.eval_expr(expr).to_num() + 1.0;
                let result = AwkValue::Num(val);
                self.assign_to(expr, result.clone());
                result
            }
            Expr::PreDecr(expr) => {
                let val = self.eval_expr(expr).to_num() - 1.0;
                let result = AwkValue::Num(val);
                self.assign_to(expr, result.clone());
                result
            }
            Expr::PostIncr(expr) => {
                let old = self.eval_expr(expr).to_num();
                self.assign_to(expr, AwkValue::Num(old + 1.0));
                AwkValue::Num(old)
            }
            Expr::PostDecr(expr) => {
                let old = self.eval_expr(expr).to_num();
                self.assign_to(expr, AwkValue::Num(old - 1.0));
                AwkValue::Num(old)
            }
            Expr::Ternary(cond, then_expr, else_expr) => {
                if self.eval_expr(cond).is_truthy() {
                    self.eval_expr(then_expr)
                } else {
                    self.eval_expr(else_expr)
                }
            }
            Expr::MatchOp(positive, expr, re) => {
                let s = self.eval_expr(expr).to_str();
                let m = regex_match(&s, re);
                let result = if *positive { m } else { !m };
                AwkValue::Num(if result { 1.0 } else { 0.0 })
            }
            Expr::Concat(lhs, rhs) => {
                let l = self.eval_expr(lhs).to_str();
                let r = self.eval_expr(rhs).to_str();
                AwkValue::Str(format!("{l}{r}"))
            }
            Expr::InArray(key_expr, arr) => {
                let key = self.eval_expr(key_expr).to_str();
                let has = self.arrays.get(arr).is_some_and(|m| m.contains_key(&key));
                AwkValue::Num(if has { 1.0 } else { 0.0 })
            }
            Expr::Call(name, args) => self.call_function(name, args),
        }
    }

    fn assign_to(&mut self, target: &Expr, val: AwkValue) {
        match target {
            Expr::Var(name) => {
                self.set_var(name, val);
            }
            Expr::FieldRef(idx_expr) => {
                let idx = self.eval_expr(idx_expr).to_num();
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let n = idx.max(0.0) as usize;
                self.set_field(n, val.to_str());
            }
            Expr::ArrayRef(name, idx_expr) => {
                let key = self.eval_expr(idx_expr).to_str();
                self.arrays
                    .entry(name.clone())
                    .or_default()
                    .insert(key, val);
            }
            _ => {
                // Ignore assignments to non-lvalues (awk just ignores these)
            }
        }
    }

    fn apply_binop(&self, op: BinOp, l: &AwkValue, r: &AwkValue) -> AwkValue {
        match op {
            BinOp::Add => AwkValue::Num(l.to_num() + r.to_num()),
            BinOp::Sub => AwkValue::Num(l.to_num() - r.to_num()),
            BinOp::Mul => AwkValue::Num(l.to_num() * r.to_num()),
            BinOp::Div => {
                let d = r.to_num();
                if d == 0.0 {
                    AwkValue::Num(0.0)
                } else {
                    AwkValue::Num(l.to_num() / d)
                }
            }
            BinOp::Mod => {
                let d = r.to_num();
                if d == 0.0 {
                    AwkValue::Num(0.0)
                } else {
                    AwkValue::Num(l.to_num() % d)
                }
            }
            BinOp::Pow => AwkValue::Num(l.to_num().powf(r.to_num())),
            BinOp::Eq => {
                let result = self.compare_values(l, r) == 0;
                AwkValue::Num(if result { 1.0 } else { 0.0 })
            }
            BinOp::Ne => {
                let result = self.compare_values(l, r) != 0;
                AwkValue::Num(if result { 1.0 } else { 0.0 })
            }
            BinOp::Lt => {
                let result = self.compare_values(l, r) < 0;
                AwkValue::Num(if result { 1.0 } else { 0.0 })
            }
            BinOp::Gt => {
                let result = self.compare_values(l, r) > 0;
                AwkValue::Num(if result { 1.0 } else { 0.0 })
            }
            BinOp::Le => {
                let result = self.compare_values(l, r) <= 0;
                AwkValue::Num(if result { 1.0 } else { 0.0 })
            }
            BinOp::Ge => {
                let result = self.compare_values(l, r) >= 0;
                AwkValue::Num(if result { 1.0 } else { 0.0 })
            }
            BinOp::And => AwkValue::Num(if l.is_truthy() && r.is_truthy() {
                1.0
            } else {
                0.0
            }),
            BinOp::Or => AwkValue::Num(if l.is_truthy() || r.is_truthy() {
                1.0
            } else {
                0.0
            }),
        }
    }

    /// Compare two values using awk rules: if both look numeric, compare numerically;
    /// otherwise compare as strings.
    #[allow(clippy::unused_self)]
    fn compare_values(&self, l: &AwkValue, r: &AwkValue) -> i8 {
        let both_num = matches!(
            (l, r),
            (
                AwkValue::Num(_) | AwkValue::Uninitialized,
                AwkValue::Num(_) | AwkValue::Uninitialized
            )
        );

        // If both are strings that look numeric, compare numerically
        let looks_numeric = |v: &AwkValue| -> bool {
            match v {
                AwkValue::Num(_) | AwkValue::Uninitialized => true,
                AwkValue::Str(s) => {
                    let s = s.trim();
                    !s.is_empty() && s.parse::<f64>().is_ok()
                }
            }
        };

        if both_num || (looks_numeric(l) && looks_numeric(r)) {
            let ln = l.to_num();
            let rn = r.to_num();
            if ln < rn {
                -1
            } else {
                i8::from(ln > rn)
            }
        } else {
            let ls = l.to_str();
            let rs = r.to_str();
            match ls.cmp(&rs) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            }
        }
    }

    // ---- Built-in functions ----

    fn call_function(&mut self, name: &str, args: &[Expr]) -> AwkValue {
        match name {
            "length" => {
                let s = if args.is_empty() {
                    self.get_field(0).to_str()
                } else {
                    self.eval_expr(&args[0]).to_str()
                };
                #[allow(clippy::cast_precision_loss)]
                AwkValue::Num(s.len() as f64)
            }
            "substr" => {
                if args.is_empty() {
                    return AwkValue::Str(String::new());
                }
                let s = self.eval_expr(&args[0]).to_str();
                let chars: Vec<char> = s.chars().collect();
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let start = (self
                    .eval_expr(args.get(1).unwrap_or(&Expr::Num(1.0)))
                    .to_num() as isize)
                    .max(1) as usize;
                let start_idx = (start - 1).min(chars.len());
                if args.len() >= 3 {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let len = self.eval_expr(&args[2]).to_num().max(0.0) as usize;
                    let end = (start_idx + len).min(chars.len());
                    AwkValue::Str(chars[start_idx..end].iter().collect())
                } else {
                    AwkValue::Str(chars[start_idx..].iter().collect())
                }
            }
            "index" => {
                if args.len() < 2 {
                    return AwkValue::Num(0.0);
                }
                let s = self.eval_expr(&args[0]).to_str();
                let t = self.eval_expr(&args[1]).to_str();
                #[allow(clippy::cast_precision_loss)]
                match s.find(&t) {
                    Some(pos) => AwkValue::Num((pos + 1) as f64),
                    None => AwkValue::Num(0.0),
                }
            }
            "split" => {
                if args.len() < 2 {
                    return AwkValue::Num(0.0);
                }
                let s = self.eval_expr(&args[0]).to_str();
                let arr_name = match &args[1] {
                    Expr::Var(n) => n.clone(),
                    _ => return AwkValue::Num(0.0),
                };
                let sep = if args.len() >= 3 {
                    self.eval_expr(&args[2]).to_str()
                } else {
                    self.get_fs()
                };
                let parts: Vec<&str> = if sep == " " {
                    s.split_whitespace().collect()
                } else {
                    s.split(&sep).collect()
                };
                // Clear existing array
                self.arrays.insert(arr_name.clone(), HashMap::new());
                let arr = self.arrays.get_mut(&arr_name).unwrap();
                for (i, part) in parts.iter().enumerate() {
                    arr.insert(format!("{}", i + 1), AwkValue::Str((*part).to_string()));
                }
                #[allow(clippy::cast_precision_loss)]
                AwkValue::Num(parts.len() as f64)
            }
            "sub" | "gsub" => {
                let global = name == "gsub";
                if args.len() < 2 {
                    return AwkValue::Num(0.0);
                }
                // First arg can be a regex literal /re/ or a string expression
                let pattern = match &args[0] {
                    Expr::Regex(re) => re.clone(),
                    other => self.eval_expr(other).to_str(),
                };
                let replacement = self.eval_expr(&args[1]).to_str();

                // Target defaults to $0
                let (target_str, target_field) = if args.len() >= 3 {
                    match &args[2] {
                        Expr::Var(name) => (self.get_var(name).to_str(), None),
                        Expr::FieldRef(idx) => {
                            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                            let n = self.eval_expr(idx).to_num().max(0.0) as usize;
                            (self.get_field(n).to_str(), Some(n))
                        }
                        _ => (self.get_field(0).to_str(), Some(0)),
                    }
                } else {
                    (self.get_field(0).to_str(), Some(0))
                };

                let (result, count) = regex_replace(&target_str, &pattern, &replacement, global);

                // Assign back
                if let Some(field_n) = target_field {
                    self.set_field(field_n, result);
                } else if args.len() >= 3 {
                    if let Expr::Var(name) = &args[2] {
                        self.set_var(name, AwkValue::Str(result));
                    }
                }

                #[allow(clippy::cast_precision_loss)]
                AwkValue::Num(count as f64)
            }
            "match" => {
                if args.len() < 2 {
                    return AwkValue::Num(0.0);
                }
                let s = self.eval_expr(&args[0]).to_str();
                let re = match &args[1] {
                    Expr::Regex(r) => r.clone(),
                    other => self.eval_expr(other).to_str(),
                };
                if let Some((start, end)) = regex_find(&s, &re) {
                    #[allow(clippy::cast_precision_loss)]
                    {
                        self.set_var("RSTART", AwkValue::Num((start + 1) as f64));
                        self.set_var("RLENGTH", AwkValue::Num((end - start) as f64));
                        AwkValue::Num((start + 1) as f64)
                    }
                } else {
                    self.set_var("RSTART", AwkValue::Num(0.0));
                    self.set_var("RLENGTH", AwkValue::Num(-1.0));
                    AwkValue::Num(0.0)
                }
            }
            "sprintf" => {
                if args.is_empty() {
                    return AwkValue::Str(String::new());
                }
                let fmt = self.eval_expr(&args[0]).to_str();
                let arg_vals: Vec<AwkValue> = args[1..].iter().map(|a| self.eval_expr(a)).collect();
                AwkValue::Str(self.format_string(&fmt, &arg_vals))
            }
            "tolower" => {
                let s = if args.is_empty() {
                    self.get_field(0).to_str()
                } else {
                    self.eval_expr(&args[0]).to_str()
                };
                AwkValue::Str(s.to_lowercase())
            }
            "toupper" => {
                let s = if args.is_empty() {
                    self.get_field(0).to_str()
                } else {
                    self.eval_expr(&args[0]).to_str()
                };
                AwkValue::Str(s.to_uppercase())
            }
            "int" => {
                let n = if args.is_empty() {
                    0.0
                } else {
                    self.eval_expr(&args[0]).to_num()
                };
                AwkValue::Num(n.trunc())
            }
            "sqrt" => {
                let n = if args.is_empty() {
                    0.0
                } else {
                    self.eval_expr(&args[0]).to_num()
                };
                AwkValue::Num(n.sqrt())
            }
            "sin" => {
                let n = if args.is_empty() {
                    0.0
                } else {
                    self.eval_expr(&args[0]).to_num()
                };
                AwkValue::Num(n.sin())
            }
            "cos" => {
                let n = if args.is_empty() {
                    0.0
                } else {
                    self.eval_expr(&args[0]).to_num()
                };
                AwkValue::Num(n.cos())
            }
            "log" => {
                let n = if args.is_empty() {
                    0.0
                } else {
                    self.eval_expr(&args[0]).to_num()
                };
                AwkValue::Num(n.ln())
            }
            "exp" => {
                let n = if args.is_empty() {
                    0.0
                } else {
                    self.eval_expr(&args[0]).to_num()
                };
                AwkValue::Num(n.exp())
            }
            "rand" => {
                let r = self.next_rand();
                AwkValue::Num(r)
            }
            "srand" => {
                let old = self.rand_state;
                if args.is_empty() {
                    // Use a fixed but different seed
                    self.rand_state = 0xdead_beef_cafe_babe;
                } else {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    {
                        self.rand_state = self.eval_expr(&args[0]).to_num() as u64;
                    }
                    // Ensure non-zero
                    if self.rand_state == 0 {
                        self.rand_state = 1;
                    }
                }
                #[allow(clippy::cast_precision_loss)]
                AwkValue::Num(old as f64)
            }
            _ => {
                // Try user-defined function
                if let Some(func) = self.functions.get(name).cloned() {
                    let arg_vals: Vec<AwkValue> = args.iter().map(|a| self.eval_expr(a)).collect();
                    self.call_user_function(&func, &arg_vals)
                } else {
                    AwkValue::Uninitialized
                }
            }
        }
    }

    fn call_user_function(&mut self, func: &AwkFunction, args: &[AwkValue]) -> AwkValue {
        // Save current variables that match parameter names
        let mut saved = Vec::new();
        for (i, param) in func.params.iter().enumerate() {
            let old = self.vars.remove(param);
            saved.push((param.clone(), old));
            let val = args.get(i).cloned().unwrap_or(AwkValue::Uninitialized);
            self.set_var(param, val);
        }

        // Execute body
        let mut result = AwkValue::Uninitialized;
        for stmt in &func.body {
            match self.exec_stmt(stmt) {
                ControlFlow::Return(val) => {
                    result = val;
                    break;
                }
                ControlFlow::Exit(code) => {
                    self.exit_code = code;
                    break;
                }
                _ => {}
            }
        }

        // Restore saved variables
        for (name, old) in saved {
            if let Some(v) = old {
                self.set_var(&name, v);
            } else {
                self.vars.remove(&name);
            }
        }

        result
    }

    // ---- Printf/format implementation ----

    #[allow(clippy::unused_self)]
    fn format_string(&self, fmt: &str, args: &[AwkValue]) -> String {
        let mut result = String::new();
        let chars: Vec<char> = fmt.chars().collect();
        let mut i = 0;
        let mut arg_idx = 0;

        while i < chars.len() {
            if chars[i] == '%' {
                i += 1;
                if i >= chars.len() {
                    result.push('%');
                    break;
                }
                if chars[i] == '%' {
                    result.push('%');
                    i += 1;
                    continue;
                }

                // Parse flags
                let mut left_align = false;
                let mut zero_pad = false;
                let mut plus_sign = false;
                let mut space_sign = false;
                loop {
                    if i >= chars.len() {
                        break;
                    }
                    match chars[i] {
                        '-' => {
                            left_align = true;
                            i += 1;
                        }
                        '0' => {
                            zero_pad = true;
                            i += 1;
                        }
                        '+' => {
                            plus_sign = true;
                            i += 1;
                        }
                        ' ' => {
                            space_sign = true;
                            i += 1;
                        }
                        _ => break,
                    }
                }

                // Parse width
                let mut width = 0usize;
                if i < chars.len() && chars[i] == '*' {
                    i += 1;
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    {
                        width = args.get(arg_idx).map_or(0, |v| v.to_num() as usize);
                    }
                    arg_idx += 1;
                } else {
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        width = width * 10 + (chars[i] as usize - '0' as usize);
                        i += 1;
                    }
                }

                // Parse precision
                let mut precision: Option<usize> = None;
                if i < chars.len() && chars[i] == '.' {
                    i += 1;
                    let mut prec = 0usize;
                    if i < chars.len() && chars[i] == '*' {
                        i += 1;
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        {
                            prec = args.get(arg_idx).map_or(0, |v| v.to_num() as usize);
                        }
                        arg_idx += 1;
                    } else {
                        while i < chars.len() && chars[i].is_ascii_digit() {
                            prec = prec * 10 + (chars[i] as usize - '0' as usize);
                            i += 1;
                        }
                    }
                    precision = Some(prec);
                }

                if i >= chars.len() {
                    break;
                }

                let spec = chars[i];
                i += 1;

                let arg = args
                    .get(arg_idx)
                    .cloned()
                    .unwrap_or(AwkValue::Uninitialized);
                arg_idx += 1;

                let formatted = match spec {
                    'd' | 'i' => {
                        #[allow(clippy::cast_possible_truncation)]
                        let n = arg.to_num() as i64;
                        let s = if plus_sign && n >= 0 {
                            format!("+{n}")
                        } else if space_sign && n >= 0 {
                            format!(" {n}")
                        } else {
                            format!("{n}")
                        };
                        pad_string(&s, width, left_align, if zero_pad { '0' } else { ' ' })
                    }
                    'o' => {
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let n = arg.to_num() as u64;
                        let s = format!("{n:o}");
                        pad_string(&s, width, left_align, if zero_pad { '0' } else { ' ' })
                    }
                    'x' => {
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let n = arg.to_num() as u64;
                        let s = format!("{n:x}");
                        pad_string(&s, width, left_align, if zero_pad { '0' } else { ' ' })
                    }
                    'X' => {
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let n = arg.to_num() as u64;
                        let s = format!("{n:X}");
                        pad_string(&s, width, left_align, if zero_pad { '0' } else { ' ' })
                    }
                    'f' => {
                        let n = arg.to_num();
                        let prec = precision.unwrap_or(6);
                        let s = if plus_sign && n >= 0.0 {
                            format!("+{n:.prec$}")
                        } else if space_sign && n >= 0.0 {
                            format!(" {n:.prec$}")
                        } else {
                            format!("{n:.prec$}")
                        };
                        pad_string(&s, width, left_align, if zero_pad { '0' } else { ' ' })
                    }
                    'e' => {
                        let n = arg.to_num();
                        let prec = precision.unwrap_or(6);
                        let s = format_scientific(n, prec, false);
                        let s = if plus_sign && n >= 0.0 {
                            format!("+{s}")
                        } else if space_sign && n >= 0.0 {
                            format!(" {s}")
                        } else {
                            s
                        };
                        pad_string(&s, width, left_align, if zero_pad { '0' } else { ' ' })
                    }
                    'E' => {
                        let n = arg.to_num();
                        let prec = precision.unwrap_or(6);
                        let s = format_scientific(n, prec, true);
                        let s = if plus_sign && n >= 0.0 {
                            format!("+{s}")
                        } else if space_sign && n >= 0.0 {
                            format!(" {s}")
                        } else {
                            s
                        };
                        pad_string(&s, width, left_align, if zero_pad { '0' } else { ' ' })
                    }
                    'g' | 'G' => {
                        let n = arg.to_num();
                        let prec = precision.unwrap_or(6).max(1);
                        let s = format_g(n, prec, spec == 'G');
                        let s = if plus_sign && n >= 0.0 {
                            format!("+{s}")
                        } else if space_sign && n >= 0.0 {
                            format!(" {s}")
                        } else {
                            s
                        };
                        pad_string(&s, width, left_align, if zero_pad { '0' } else { ' ' })
                    }
                    's' => {
                        let mut s = arg.to_str();
                        if let Some(prec) = precision {
                            let truncated: String = s.chars().take(prec).collect();
                            s = truncated;
                        }
                        pad_string(&s, width, left_align, ' ')
                    }
                    'c' => {
                        let s = arg.to_str();
                        let c = s.chars().next().map_or(String::new(), |c| c.to_string());
                        pad_string(&c, width, left_align, ' ')
                    }
                    _ => {
                        format!("%{spec}")
                    }
                };

                result.push_str(&formatted);
            } else if chars[i] == '\\' {
                i += 1;
                if i < chars.len() {
                    match chars[i] {
                        'n' => result.push('\n'),
                        't' => result.push('\t'),
                        'r' => result.push('\r'),
                        '\\' => result.push('\\'),
                        '"' => result.push('"'),
                        'a' => result.push('\x07'),
                        'b' => result.push('\x08'),
                        c => {
                            result.push('\\');
                            result.push(c);
                        }
                    }
                    i += 1;
                } else {
                    result.push('\\');
                }
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }

        result
    }

    // ---- Statement execution ----

    fn exec_stmts(&mut self, stmts: &[Stmt]) -> ControlFlow {
        for stmt in stmts {
            let cf = self.exec_stmt(stmt);
            match cf {
                ControlFlow::None => {}
                other => return other,
            }
        }
        ControlFlow::None
    }

    fn exec_stmt(&mut self, stmt: &Stmt) -> ControlFlow {
        match stmt {
            Stmt::Expr(expr) => {
                self.eval_expr(expr);
                ControlFlow::None
            }
            Stmt::Print(args, _redirect) => {
                if args.is_empty() {
                    // print $0
                    let s = self.get_field(0).to_str();
                    let ors = self.get_ors();
                    self.write_output(s.as_bytes());
                    self.write_output(ors.as_bytes());
                } else {
                    let ofs = self.get_ofs();
                    let ors = self.get_ors();
                    let mut first = true;
                    for arg in args {
                        if !first {
                            self.write_output(ofs.as_bytes());
                        }
                        let val = self.eval_expr(arg);
                        self.write_output(val.to_str().as_bytes());
                        first = false;
                    }
                    self.write_output(ors.as_bytes());
                }
                ControlFlow::None
            }
            Stmt::Printf(args, _redirect) => {
                if args.is_empty() {
                    return ControlFlow::None;
                }
                let fmt = self.eval_expr(&args[0]).to_str();
                let arg_vals: Vec<AwkValue> = args[1..].iter().map(|a| self.eval_expr(a)).collect();
                let s = self.format_string(&fmt, &arg_vals);
                self.write_output(s.as_bytes());
                ControlFlow::None
            }
            Stmt::If(cond, then_body, else_body) => {
                if self.eval_expr(cond).is_truthy() {
                    self.exec_stmts(then_body)
                } else if let Some(eb) = else_body {
                    self.exec_stmts(eb)
                } else {
                    ControlFlow::None
                }
            }
            Stmt::While(cond, body) => {
                let mut iteration = 0;
                loop {
                    if !self.eval_expr(cond).is_truthy() {
                        break;
                    }
                    match self.exec_stmts(body) {
                        ControlFlow::Break => break,
                        ControlFlow::Continue | ControlFlow::None => {}
                        other => return other,
                    }
                    iteration += 1;
                    if iteration > 1_000_000 {
                        self.stderr_buf
                            .extend_from_slice(b"awk: loop iteration limit (1000000) reached\n");
                        break;
                    }
                }
                ControlFlow::None
            }
            Stmt::DoWhile(body, cond) => {
                let mut iteration = 0;
                loop {
                    match self.exec_stmts(body) {
                        ControlFlow::Break => break,
                        ControlFlow::Continue | ControlFlow::None => {}
                        other => return other,
                    }
                    if !self.eval_expr(cond).is_truthy() {
                        break;
                    }
                    iteration += 1;
                    if iteration > 1_000_000 {
                        self.stderr_buf
                            .extend_from_slice(b"awk: loop iteration limit (1000000) reached\n");
                        break;
                    }
                }
                ControlFlow::None
            }
            Stmt::For(init, cond, incr, body) => {
                if let Some(init_stmt) = init {
                    let cf = self.exec_stmt(init_stmt);
                    if !matches!(cf, ControlFlow::None) {
                        return cf;
                    }
                }
                let mut iteration = 0;
                loop {
                    if let Some(c) = cond {
                        if !self.eval_expr(c).is_truthy() {
                            break;
                        }
                    }
                    match self.exec_stmts(body) {
                        ControlFlow::Break => break,
                        ControlFlow::Continue | ControlFlow::None => {}
                        other => return other,
                    }
                    if let Some(incr_stmt) = incr {
                        self.exec_stmt(incr_stmt);
                    }
                    iteration += 1;
                    if iteration > 1_000_000 {
                        self.stderr_buf
                            .extend_from_slice(b"awk: loop iteration limit (1000000) reached\n");
                        break;
                    }
                }
                ControlFlow::None
            }
            Stmt::ForIn(var, arr_name, body) => {
                let keys: Vec<String> = self
                    .arrays
                    .get(arr_name)
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                for key in keys {
                    self.set_var(var, AwkValue::Str(key));
                    match self.exec_stmts(body) {
                        ControlFlow::Break => break,
                        ControlFlow::Continue | ControlFlow::None => {}
                        other => return other,
                    }
                }
                ControlFlow::None
            }
            Stmt::Break => ControlFlow::Break,
            Stmt::Continue => ControlFlow::Continue,
            Stmt::Next => ControlFlow::Next,
            Stmt::Exit(code) => {
                let c = code.as_ref().map_or(0, |e| {
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        self.eval_expr(e).to_num() as i32
                    }
                });
                ControlFlow::Exit(c)
            }
            Stmt::Return(expr) => {
                let val = expr
                    .as_ref()
                    .map_or(AwkValue::Uninitialized, |e| self.eval_expr(e));
                ControlFlow::Return(val)
            }
            Stmt::Delete(name, idx_expr) => {
                let key = self.eval_expr(idx_expr).to_str();
                if let Some(arr) = self.arrays.get_mut(name) {
                    arr.remove(&key);
                }
                ControlFlow::None
            }
            Stmt::Block(stmts) => self.exec_stmts(stmts),
        }
    }

    /// Test whether a pattern matches the current record.
    fn pattern_matches(&mut self, pattern: &AwkPattern, rule_idx: usize) -> bool {
        match pattern {
            AwkPattern::All => true,
            AwkPattern::Expr(expr) => self.eval_expr(expr).is_truthy(),
            AwkPattern::Regex(re) => {
                let line = self.get_field(0).to_str();
                regex_match(&line, re)
            }
            AwkPattern::Range(start_pat, end_pat) => {
                // Ensure range_active is big enough
                while self.range_active.len() <= rule_idx {
                    self.range_active.push(false);
                }

                if self.range_active[rule_idx] {
                    // Already in range; check if end pattern matches
                    let end_matches = match end_pat.as_ref() {
                        AwkPattern::Regex(re) => {
                            let line = self.get_field(0).to_str();
                            regex_match(&line, re)
                        }
                        AwkPattern::Expr(expr) => self.eval_expr(expr).is_truthy(),
                        _ => false,
                    };
                    if end_matches {
                        self.range_active[rule_idx] = false;
                    }
                    true
                } else {
                    // Check if start pattern matches
                    let start_matches = match start_pat.as_ref() {
                        AwkPattern::Regex(re) => {
                            let line = self.get_field(0).to_str();
                            regex_match(&line, re)
                        }
                        AwkPattern::Expr(expr) => self.eval_expr(expr).is_truthy(),
                        _ => false,
                    };
                    if start_matches {
                        self.range_active[rule_idx] = true;
                        true
                    } else {
                        false
                    }
                }
            }
        }
    }

    /// Run the full awk program on the given input.
    fn run(&mut self, program: &AwkProgram, inputs: &[(String, String)]) -> i32 {
        self.functions.clone_from(&program.functions);

        // BEGIN
        if let ControlFlow::Exit(code) = self.exec_stmts(&program.begin) {
            self.exit_code = code;
            // Still run END
            self.exec_stmts(&program.end);
            return self.exit_code;
        }

        let mut nr: f64 = 0.0;

        for (filename, content) in inputs {
            let rs = self.get_var("RS").to_str();
            let records: Vec<&str> = if rs == "\n" {
                content.lines().collect()
            } else if rs.is_empty() {
                // TODO: paragraph mode (RS="") — currently falls back to line-based
                content.lines().collect()
            } else if rs.len() == 1 {
                content.split(rs.chars().next().unwrap()).collect()
            } else {
                content.lines().collect()
            };

            let mut fnr: f64 = 0.0;
            self.set_var("FILENAME", AwkValue::Str(filename.clone()));

            for record in records {
                nr += 1.0;
                fnr += 1.0;
                self.set_var("NR", AwkValue::Num(nr));
                self.set_var("FNR", AwkValue::Num(fnr));
                self.set_record(record);

                for (rule_idx, rule) in program.rules.iter().enumerate() {
                    if self.pattern_matches(&rule.pattern, rule_idx) {
                        match self.exec_stmts(&rule.action) {
                            ControlFlow::Next => {
                                break;
                            }
                            ControlFlow::Exit(code) => {
                                self.exit_code = code;
                                // Run END block
                                self.exec_stmts(&program.end);
                                return self.exit_code;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // END
        if let ControlFlow::Exit(code) = self.exec_stmts(&program.end) {
            self.exit_code = code;
        }

        self.exit_code
    }
}

// ---------------------------------------------------------------------------
// Regex replace helper
// ---------------------------------------------------------------------------

fn regex_replace(text: &str, pattern: &str, replacement: &str, global: bool) -> (String, usize) {
    let mut result = String::new();
    let mut count = 0;
    let mut search_start = 0;

    loop {
        if search_start > text.len() {
            break;
        }

        // Search in the remaining text
        let remaining = &text[search_start..];
        if let Some((match_start, match_end)) = regex_find(remaining, pattern) {
            // Append text before the match
            result.push_str(&remaining[..match_start]);

            // Build replacement: & refers to the matched text
            let matched = &remaining[match_start..match_end];
            let repl = replacement.replace('&', matched);
            result.push_str(&repl);

            count += 1;

            // Move past the match
            let advance = match_end.max(1); // Ensure we always advance
            search_start += advance;

            if !global {
                // Append the rest
                if search_start <= text.len() {
                    result.push_str(&text[search_start..]);
                }
                break;
            }
        } else {
            // No more matches; append the rest
            result.push_str(remaining);
            break;
        }
    }

    (result, count)
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

fn pad_string(s: &str, width: usize, left_align: bool, pad_char: char) -> String {
    if s.len() >= width {
        return s.to_string();
    }
    let padding: String = std::iter::repeat_n(pad_char, width - s.len()).collect();
    if left_align {
        format!("{s}{padding}")
    } else {
        // For zero-padding with a sign, put sign before zeros
        if pad_char == '0' && (s.starts_with('-') || s.starts_with('+') || s.starts_with(' ')) {
            let (sign, num) = s.split_at(1);
            let padding: String = std::iter::repeat_n('0', width - s.len()).collect();
            format!("{sign}{padding}{num}")
        } else {
            format!("{padding}{s}")
        }
    }
}

fn format_scientific(n: f64, prec: usize, upper: bool) -> String {
    if n == 0.0 {
        let e = if upper { 'E' } else { 'e' };
        return format!("{:.prec$}{e}+00", 0.0);
    }
    let sign = if n < 0.0 { "-" } else { "" };
    let abs = n.abs();
    let exp = abs.log10().floor() as i32;
    let mantissa = abs / 10f64.powi(exp);
    let e = if upper { 'E' } else { 'e' };
    let exp_sign = if exp >= 0 { '+' } else { '-' };
    let exp_abs = exp.unsigned_abs();
    format!("{sign}{mantissa:.prec$}{e}{exp_sign}{exp_abs:02}")
}

#[allow(clippy::cast_possible_wrap)]
fn format_g(n: f64, prec: usize, upper: bool) -> String {
    if n == 0.0 {
        return "0".to_string();
    }
    let abs = n.abs();
    let exp = if abs == 0.0 {
        0
    } else {
        abs.log10().floor() as i32
    };

    if exp >= -4 && exp < prec as i32 {
        // Use %f style
        let decimal_digits = if prec as i32 - 1 - exp > 0 {
            (prec as i32 - 1 - exp) as usize
        } else {
            0
        };
        let s = format!("{n:.decimal_digits$}");
        // Trim trailing zeros
        if s.contains('.') {
            let s = s.trim_end_matches('0').trim_end_matches('.');
            s.to_string()
        } else {
            s
        }
    } else {
        // Use %e style
        let sig_digits = prec.saturating_sub(1);
        let s = format_scientific(n, sig_digits, upper);
        // Trim trailing zeros before 'e'/'E'
        let e_char = if upper { 'E' } else { 'e' };
        if let Some(e_pos) = s.find(e_char) {
            let (mantissa_part, exp_part) = s.split_at(e_pos);
            if mantissa_part.contains('.') {
                let trimmed = mantissa_part.trim_end_matches('0').trim_end_matches('.');
                format!("{trimmed}{exp_part}")
            } else {
                s
            }
        } else {
            s
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub(crate) fn util_awk(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut fs: Option<String> = None;
    let mut pre_vars: Vec<(String, String)> = Vec::new();
    let mut prog_text: Option<String> = None;
    let mut prog_file: Option<String> = None;

    // Parse options
    while let Some(arg) = args.first() {
        if *arg == "-F" {
            if args.len() < 2 {
                ctx.output.stderr(b"awk: -F requires an argument\n");
                return 1;
            }
            fs = Some(args[1].to_string());
            args = &args[2..];
        } else if let Some(sep) = arg.strip_prefix("-F") {
            fs = Some(sep.to_string());
            args = &args[1..];
        } else if *arg == "-v" {
            if args.len() < 2 {
                ctx.output.stderr(b"awk: -v requires an argument\n");
                return 1;
            }
            if let Some((name, val)) = args[1].split_once('=') {
                pre_vars.push((name.to_string(), val.to_string()));
            } else {
                let msg = format!("awk: invalid -v argument: '{}'\n", args[1]);
                ctx.output.stderr(msg.as_bytes());
                return 1;
            }
            args = &args[2..];
        } else if *arg == "-f" {
            if args.len() < 2 {
                ctx.output.stderr(b"awk: -f requires an argument\n");
                return 1;
            }
            prog_file = Some(args[1].to_string());
            args = &args[2..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            let msg = format!("awk: unknown option: '{arg}'\n");
            ctx.output.stderr(msg.as_bytes());
            return 1;
        } else {
            break;
        }
    }

    // Get program text
    if let Some(file_path) = prog_file {
        let full = resolve_path(ctx.cwd, &file_path);
        match read_text(ctx.fs, &full) {
            Ok(text) => prog_text = Some(text),
            Err(e) => {
                emit_error(ctx.output, "awk", &file_path, &e);
                return 1;
            }
        }
    } else if let Some(arg) = args.first() {
        prog_text = Some((*arg).to_string());
        args = &args[1..];
    }

    let Some(program_src) = prog_text else {
        ctx.output.stderr(b"awk: no program text\n");
        return 1;
    };

    // Tokenize
    let mut lexer = Lexer::new(&program_src);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(e) => {
            let msg = format!("awk: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            return 2;
        }
    };

    // Parse
    let mut parser = Parser::new(tokens);
    let program = match parser.parse_program() {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("awk: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            return 2;
        }
    };

    // Build interpreter
    let mut interp = AwkInterpreter::new();

    // Set FS if provided
    if let Some(ref sep) = fs {
        interp.set_var("FS", AwkValue::Str(sep.clone()));
    }

    // Set pre-assigned variables
    for (name, val) in &pre_vars {
        // Try to parse as number
        if let Ok(n) = val.parse::<f64>() {
            interp.set_var(name, AwkValue::Num(n));
        } else {
            interp.set_var(name, AwkValue::Str(val.clone()));
        }
    }

    // Gather input
    let file_args: Vec<&str> = args.to_vec();
    let inputs: Vec<(String, String)> = if file_args.is_empty() {
        let text = if let Some(data) = ctx.stdin {
            String::from_utf8_lossy(data).to_string()
        } else {
            String::new()
        };
        vec![(String::new(), text)]
    } else {
        let mut v = Vec::new();
        for path in &file_args {
            // Check for var=value assignments in file args
            if let Some((name, val)) = path.split_once('=') {
                if !name.is_empty()
                    && name.chars().all(|c| c.is_alphanumeric() || c == '_')
                    && name
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_alphabetic() || c == '_')
                {
                    if let Ok(n) = val.parse::<f64>() {
                        interp.set_var(name, AwkValue::Num(n));
                    } else {
                        interp.set_var(name, AwkValue::Str(val.to_string()));
                    }
                    continue;
                }
            }
            let full = resolve_path(ctx.cwd, path);
            match read_text(ctx.fs, &full) {
                Ok(text) => v.push(((*path).to_string(), text)),
                Err(e) => {
                    emit_error(ctx.output, "awk", path, &e);
                    return 1;
                }
            }
        }
        if let (true, Some(stdin_data)) = (v.is_empty(), ctx.stdin) {
            let text = String::from_utf8_lossy(stdin_data).to_string();
            v.push((String::new(), text));
        }
        v
    };

    // Run
    let exit_code = interp.run(&program, &inputs);

    // Flush output
    ctx.output.stdout(&interp.output_buf);
    if !interp.stderr_buf.is_empty() {
        ctx.output.stderr(&interp.stderr_buf);
    }

    exit_code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UtilContext, VecOutput};
    use wasmsh_fs::{MemoryFs, OpenOptions, Vfs};

    fn run_awk(program: &str, input: &str) -> (i32, String, String) {
        run_awk_with_args(&["awk", program], input)
    }

    fn run_awk_with_args(argv: &[&str], input: &str) -> (i32, String, String) {
        let mut fs = MemoryFs::new();
        let mut output = VecOutput::default();
        let stdin_data = input.as_bytes();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut output,
                cwd: "/",
                stdin: Some(stdin_data),
                state: None,
            };
            util_awk(&mut ctx, argv)
        };
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        (status, stdout, stderr)
    }

    fn run_awk_with_file(program: &str, path: &str, content: &str) -> (i32, String, String) {
        let mut fs = MemoryFs::new();
        let h = fs.open(path, OpenOptions::write()).unwrap();
        fs.write_file(h, content.as_bytes()).unwrap();
        fs.close(h);
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut output,
                cwd: "/",
                stdin: None,
                state: None,
            };
            let argv = vec!["awk", program, path];
            util_awk(&mut ctx, &argv)
        };
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        (status, stdout, stderr)
    }

    #[test]
    fn test_print_all() {
        let (status, out, _) = run_awk("{ print }", "hello\nworld\n");
        assert_eq!(status, 0);
        assert_eq!(out, "hello\nworld\n");
    }

    #[test]
    fn test_print_field() {
        let (status, out, _) = run_awk("{ print $1 }", "hello world\nfoo bar\n");
        assert_eq!(status, 0);
        assert_eq!(out, "hello\nfoo\n");
    }

    #[test]
    fn test_print_second_field() {
        let (status, out, _) = run_awk("{ print $2 }", "a b c\nd e f\n");
        assert_eq!(status, 0);
        assert_eq!(out, "b\ne\n");
    }

    #[test]
    fn test_field_separator() {
        let (status, out, _) = run_awk_with_args(
            &["awk", "-F", ":", "{ print $1 }"],
            "root:x:0\nuser:x:1000\n",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "root\nuser\n");
    }

    #[test]
    fn test_field_separator_no_space() {
        let (status, out, _) = run_awk_with_args(&["awk", "-F:", "{ print $1 }"], "a:b:c\n");
        assert_eq!(status, 0);
        assert_eq!(out, "a\n");
    }

    #[test]
    fn test_begin_end() {
        let (status, out, _) = run_awk(
            "BEGIN { print \"start\" } { print } END { print \"done\" }",
            "mid\n",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "start\nmid\ndone\n");
    }

    #[test]
    fn test_nr() {
        let (status, out, _) = run_awk("{ print NR, $0 }", "a\nb\nc\n");
        assert_eq!(status, 0);
        assert_eq!(out, "1 a\n2 b\n3 c\n");
    }

    #[test]
    fn test_nf() {
        let (status, out, _) = run_awk("{ print NF }", "a b c\nd\ne f\n");
        assert_eq!(status, 0);
        assert_eq!(out, "3\n1\n2\n");
    }

    #[test]
    fn test_regex_pattern() {
        let (status, out, _) = run_awk("/hello/", "hello world\ngoodbye\nhello again\n");
        assert_eq!(status, 0);
        assert_eq!(out, "hello world\nhello again\n");
    }

    #[test]
    fn test_expression_pattern() {
        let (status, out, _) = run_awk("NR > 1", "first\nsecond\nthird\n");
        assert_eq!(status, 0);
        assert_eq!(out, "second\nthird\n");
    }

    #[test]
    fn test_arithmetic() {
        let (status, out, _) = run_awk("{ print $1 + $2 }", "3 4\n10 20\n");
        assert_eq!(status, 0);
        assert_eq!(out, "7\n30\n");
    }

    #[test]
    fn test_string_concatenation() {
        let (status, out, _) = run_awk("{ print $1 $2 }", "hel lo\nwor ld\n");
        assert_eq!(status, 0);
        assert_eq!(out, "hello\nworld\n");
    }

    #[test]
    fn test_variables() {
        let (status, out, _) = run_awk("{ sum += $1 } END { print sum }", "10\n20\n30\n");
        assert_eq!(status, 0);
        assert_eq!(out, "60\n");
    }

    #[test]
    fn test_if_else() {
        let (status, out, _) = run_awk(
            "{ if ($1 > 5) print \"big\"; else print \"small\" }",
            "3\n7\n1\n10\n",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "small\nbig\nsmall\nbig\n");
    }

    #[test]
    fn test_for_loop() {
        let (status, out, _) = run_awk(
            "BEGIN { for (i = 1; i <= 5; i++) printf \"%d \", i; print \"\" }",
            "",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "1 2 3 4 5 \n");
    }

    #[test]
    fn test_while_loop() {
        let (status, out, _) = run_awk("BEGIN { i = 1; while (i <= 3) { print i; i++ } }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "1\n2\n3\n");
    }

    #[test]
    fn test_arrays() {
        let (status, out, _) = run_awk(
            "{ count[$1]++ } END { for (w in count) print w, count[w] }",
            "a\nb\na\nc\nb\na\n",
        );
        assert_eq!(status, 0);
        // Check that the output contains the right counts (order may vary)
        assert!(out.contains("a 3"));
        assert!(out.contains("b 2"));
        assert!(out.contains("c 1"));
    }

    #[test]
    fn test_printf_d() {
        let (status, out, _) = run_awk("{ printf \"%d\\n\", $1 }", "42\n");
        assert_eq!(status, 0);
        assert_eq!(out, "42\n");
    }

    #[test]
    fn test_printf_s() {
        let (status, out, _) = run_awk("{ printf \"%-10s|\\n\", $1 }", "hi\n");
        assert_eq!(status, 0);
        assert_eq!(out, "hi        |\n");
    }

    #[test]
    fn test_printf_f() {
        let (status, out, _) = run_awk("BEGIN { printf \"%.2f\\n\", 3.14159 }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "3.14\n");
    }

    #[test]
    fn test_length() {
        let (status, out, _) = run_awk("{ print length($0) }", "hello\n");
        assert_eq!(status, 0);
        assert_eq!(out, "5\n");
    }

    #[test]
    fn test_substr() {
        let (status, out, _) = run_awk("{ print substr($0, 2, 3) }", "abcdef\n");
        assert_eq!(status, 0);
        assert_eq!(out, "bcd\n");
    }

    #[test]
    fn test_index_func() {
        let (status, out, _) = run_awk("{ print index($0, \"cd\") }", "abcdef\n");
        assert_eq!(status, 0);
        assert_eq!(out, "3\n");
    }

    #[test]
    fn test_split_func() {
        let (status, out, _) = run_awk(
            "{ n = split($0, a, \":\"); for (i = 1; i <= n; i++) print a[i] }",
            "a:b:c\n",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "a\nb\nc\n");
    }

    #[test]
    fn test_sub_func() {
        let (status, out, _) = run_awk("{ sub(/world/, \"earth\"); print }", "hello world\n");
        assert_eq!(status, 0);
        assert_eq!(out, "hello earth\n");
    }

    #[test]
    fn test_gsub_func() {
        let (status, out, _) = run_awk("{ gsub(/o/, \"0\"); print }", "foo boo\n");
        assert_eq!(status, 0);
        assert_eq!(out, "f00 b00\n");
    }

    #[test]
    fn test_tolower_toupper() {
        let (status, out, _) = run_awk("{ print tolower($1), toupper($2) }", "Hello world\n");
        assert_eq!(status, 0);
        assert_eq!(out, "hello WORLD\n");
    }

    #[test]
    fn test_match_op() {
        let (status, out, _) = run_awk("$0 ~ /^hello/", "hello world\ngoodbye\nhello again\n");
        assert_eq!(status, 0);
        assert_eq!(out, "hello world\nhello again\n");
    }

    #[test]
    fn test_not_match_op() {
        let (status, out, _) = run_awk("$0 !~ /hello/", "hello\nworld\nhello again\n");
        assert_eq!(status, 0);
        assert_eq!(out, "world\n");
    }

    #[test]
    fn test_ternary() {
        let (status, out, _) = run_awk("{ print ($1 > 5 ? \"big\" : \"small\") }", "3\n10\n");
        assert_eq!(status, 0);
        assert_eq!(out, "small\nbig\n");
    }

    #[test]
    fn test_multiple_rules() {
        let (status, out, _) = run_awk(
            "/hello/ { print \"matched\" } /world/ { print \"also matched\" }",
            "hello world\nhello\nworld\n",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "matched\nalso matched\nmatched\nalso matched\n");
    }

    #[test]
    fn test_next() {
        let (status, out, _) = run_awk(
            "/skip/ { next } { print }",
            "one\nskip this\ntwo\nskip that\nthree\n",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "one\ntwo\nthree\n");
    }

    #[test]
    fn test_exit() {
        let (status, out, _) = run_awk("NR == 2 { exit } { print }", "a\nb\nc\n");
        assert_eq!(status, 0);
        assert_eq!(out, "a\n");
    }

    #[test]
    fn test_exit_code() {
        let (status, _, _) = run_awk("BEGIN { exit 42 }", "");
        assert_eq!(status, 42);
    }

    #[test]
    fn test_delete_array() {
        let (status, out, _) = run_awk(
            "BEGIN { a[1]=\"x\"; a[2]=\"y\"; delete a[1]; for (k in a) print k, a[k] }",
            "",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "2 y\n");
    }

    #[test]
    fn test_in_array() {
        let (status, out, _) = run_awk(
            "BEGIN { a[\"x\"] = 1; if (\"x\" in a) print \"yes\"; if (\"y\" in a) print \"no\" }",
            "",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "yes\n");
    }

    #[test]
    fn test_do_while() {
        let (status, out, _) = run_awk("BEGIN { i = 1; do { print i; i++ } while (i <= 3) }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "1\n2\n3\n");
    }

    #[test]
    fn test_for_in_array() {
        let (status, out, _) = run_awk(
            "BEGIN { a[\"x\"] = 1; a[\"y\"] = 2; for (k in a) print k }",
            "",
        );
        assert_eq!(status, 0);
        // Order not guaranteed, just check both keys present
        assert!(out.contains('x'));
        assert!(out.contains('y'));
    }

    #[test]
    fn test_ofs() {
        let (status, out, _) = run_awk("BEGIN { OFS = \",\" } { print $1, $2, $3 }", "a b c\n");
        assert_eq!(status, 0);
        assert_eq!(out, "a,b,c\n");
    }

    #[test]
    fn test_field_assignment() {
        let (status, out, _) = run_awk("{ $2 = \"X\"; print }", "a b c\n");
        assert_eq!(status, 0);
        assert_eq!(out, "a X c\n");
    }

    #[test]
    fn test_pre_increment() {
        let (status, out, _) = run_awk("BEGIN { x = 5; print ++x }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "6\n");
    }

    #[test]
    fn test_post_increment() {
        let (status, out, _) = run_awk("BEGIN { x = 5; print x++ }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "5\n");
    }

    #[test]
    fn test_power_operator() {
        let (status, out, _) = run_awk("BEGIN { print 2 ^ 10 }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "1024\n");
    }

    #[test]
    fn test_modulo() {
        let (status, out, _) = run_awk("BEGIN { print 17 % 5 }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "2\n");
    }

    #[test]
    fn test_division_by_zero() {
        let (status, out, _) = run_awk("BEGIN { print 10 / 0 }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "0\n");
    }

    #[test]
    fn test_math_functions() {
        let (status, out, _) = run_awk("BEGIN { print int(3.7) }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "3\n");
    }

    #[test]
    fn test_sqrt() {
        let (status, out, _) = run_awk("BEGIN { print sqrt(16) }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "4\n");
    }

    #[test]
    fn test_sprintf() {
        let (status, out, _) = run_awk("BEGIN { s = sprintf(\"%05d\", 42); print s }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "00042\n");
    }

    #[test]
    fn test_v_option() {
        let (status, out, _) =
            run_awk_with_args(&["awk", "-v", "x=hello", "BEGIN { print x }"], "");
        assert_eq!(status, 0);
        assert_eq!(out, "hello\n");
    }

    #[test]
    fn test_empty_input() {
        let (status, out, _) = run_awk("{ print }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "");
    }

    #[test]
    fn test_begin_only() {
        let (status, out, _) = run_awk("BEGIN { print \"hello\" }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "hello\n");
    }

    #[test]
    fn test_end_only() {
        let (status, out, _) = run_awk("END { print NR }", "a\nb\nc\n");
        assert_eq!(status, 0);
        assert_eq!(out, "3\n");
    }

    #[test]
    fn test_comparison_string() {
        let (status, out, _) = run_awk("$1 == \"hello\" { print \"found\" }", "hello\nworld\n");
        assert_eq!(status, 0);
        assert_eq!(out, "found\n");
    }

    #[test]
    fn test_logical_and() {
        let (status, out, _) = run_awk("NR > 1 && NR < 4 { print }", "a\nb\nc\nd\ne\n");
        assert_eq!(status, 0);
        assert_eq!(out, "b\nc\n");
    }

    #[test]
    fn test_logical_or() {
        let (status, out, _) = run_awk("NR == 1 || NR == 3 { print }", "a\nb\nc\nd\n");
        assert_eq!(status, 0);
        assert_eq!(out, "a\nc\n");
    }

    #[test]
    fn test_unary_negation() {
        let (status, out, _) = run_awk("BEGIN { x = 5; print -x }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "-5\n");
    }

    #[test]
    fn test_logical_not() {
        let (status, out, _) = run_awk("!($1 > 3) { print }", "1\n5\n2\n");
        assert_eq!(status, 0);
        assert_eq!(out, "1\n2\n");
    }

    #[test]
    fn test_assignment_operators() {
        let (status, out, _) = run_awk(
            "BEGIN { x = 10; x += 5; print x; x -= 3; print x; x *= 2; print x; x /= 4; print x; x %= 3; print x }",
            "",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "15\n12\n24\n6\n0\n");
    }

    #[test]
    fn test_break_in_loop() {
        let (status, out, _) = run_awk(
            "BEGIN { for (i = 1; i <= 10; i++) { if (i == 4) break; print i } }",
            "",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "1\n2\n3\n");
    }

    #[test]
    fn test_continue_in_loop() {
        let (status, out, _) = run_awk(
            "BEGIN { for (i = 1; i <= 5; i++) { if (i == 3) continue; print i } }",
            "",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "1\n2\n4\n5\n");
    }

    #[test]
    fn test_file_input() {
        let (status, out, _) =
            run_awk_with_file("{ print $1 }", "/test.txt", "hello world\nfoo bar\n");
        assert_eq!(status, 0);
        assert_eq!(out, "hello\nfoo\n");
    }

    #[test]
    fn test_dollar_nf() {
        let (status, out, _) = run_awk("{ print $NF }", "a b c\nd e\n");
        assert_eq!(status, 0);
        assert_eq!(out, "c\ne\n");
    }

    #[test]
    fn test_multiple_statements() {
        let (status, out, _) = run_awk("{ x = $1 + $2; y = $1 * $2; print x, y }", "3 4\n5 6\n");
        assert_eq!(status, 0);
        assert_eq!(out, "7 12\n11 30\n");
    }

    #[test]
    fn test_no_program() {
        let (status, _, err) = run_awk_with_args(&["awk"], "");
        assert_ne!(status, 0);
        assert!(!err.is_empty());
    }

    #[test]
    fn test_match_function() {
        let (status, out, _) = run_awk(
            "{ if (match($0, /[0-9]+/)) print RSTART, RLENGTH }",
            "abc 123 def\n",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "5 3\n");
    }

    #[test]
    fn test_user_function() {
        let (status, out, _) = run_awk(
            "function double(x) { return x * 2 } BEGIN { print double(21) }",
            "",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "42\n");
    }

    #[test]
    fn test_regex_char_class() {
        let (status, out, _) = run_awk("/[0-9]/ { print }", "abc\n123\ndef\n4x\n");
        assert_eq!(status, 0);
        assert_eq!(out, "123\n4x\n");
    }

    #[test]
    fn test_hex_number() {
        let (status, out, _) = run_awk("BEGIN { print 0xFF }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "255\n");
    }

    #[test]
    fn test_string_comparison() {
        let (status, out, _) = run_awk("$0 > \"b\" { print }", "a\nb\nc\nd\n");
        assert_eq!(status, 0);
        assert_eq!(out, "c\nd\n");
    }

    #[test]
    fn test_print_with_ors() {
        let (status, out, _) = run_awk(
            "BEGIN { ORS = \" \" } { print $1 } END { printf \"\\n\" }",
            "a\nb\nc\n",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "a b c \n");
    }

    #[test]
    fn test_printf_percent() {
        let (status, out, _) = run_awk("BEGIN { printf \"100%%\\n\" }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "100%\n");
    }

    #[test]
    fn test_multi_dimensional_key() {
        // In awk, multi-subscript arrays use SUBSEP concatenation,
        // but we just test basic associative behavior
        let (status, out, _) = run_awk("BEGIN { a[\"1,2\"] = \"x\"; print a[\"1,2\"] }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "x\n");
    }

    #[test]
    fn test_regex_dot() {
        let (status, out, _) = run_awk("/h.llo/ { print }", "hello\nhxllo\nhllo\n");
        assert_eq!(status, 0);
        assert_eq!(out, "hello\nhxllo\n");
    }

    #[test]
    fn test_regex_star() {
        let (status, out, _) = run_awk("/ab*c/ { print }", "ac\nabc\nabbc\nadc\n");
        assert_eq!(status, 0);
        assert_eq!(out, "ac\nabc\nabbc\n");
    }

    #[test]
    fn test_regex_plus() {
        let (status, out, _) = run_awk("/ab+c/ { print }", "ac\nabc\nabbc\n");
        assert_eq!(status, 0);
        assert_eq!(out, "abc\nabbc\n");
    }

    #[test]
    fn test_sub_with_ampersand() {
        let (status, out, _) = run_awk("{ sub(/world/, \"[&]\"); print }", "hello world\n");
        assert_eq!(status, 0);
        assert_eq!(out, "hello [world]\n");
    }
}
