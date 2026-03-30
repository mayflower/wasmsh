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

fn looks_numeric(v: &AwkValue) -> bool {
    match v {
        AwkValue::Num(_) | AwkValue::Uninitialized => true,
        AwkValue::Str(s) => {
            let s = s.trim();
            !s.is_empty() && s.parse::<f64>().is_ok()
        }
    }
}

fn should_compare_numeric(l: &AwkValue, r: &AwkValue) -> bool {
    let both_num = matches!(
        (l, r),
        (
            AwkValue::Num(_) | AwkValue::Uninitialized,
            AwkValue::Num(_) | AwkValue::Uninitialized
        )
    );
    both_num || (looks_numeric(l) && looks_numeric(r))
}

fn compare_nums(ln: f64, rn: f64) -> i8 {
    if ln < rn {
        -1
    } else {
        i8::from(ln > rn)
    }
}

fn compare_strs(ls: &str, rs: &str) -> i8 {
    match ls.cmp(rs) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

fn bool_to_num(value: bool) -> f64 {
    if value {
        1.0
    } else {
        0.0
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

fn keyword_token(id: &str) -> Option<Token> {
    match id {
        "BEGIN" => Some(Token::Begin),
        "END" => Some(Token::End),
        "if" => Some(Token::If),
        "else" => Some(Token::Else),
        "while" => Some(Token::While),
        "for" => Some(Token::For),
        "do" => Some(Token::Do),
        "break" => Some(Token::Break),
        "continue" => Some(Token::Continue),
        "next" => Some(Token::Next),
        "exit" => Some(Token::Exit),
        "delete" => Some(Token::Delete),
        "in" => Some(Token::In),
        "print" => Some(Token::Print),
        "printf" => Some(Token::Printf),
        "getline" => Some(Token::Getline),
        "function" => Some(Token::Function),
        "return" => Some(Token::Return),
        _ => None,
    }
}

fn is_operator_start(c: char) -> bool {
    matches!(
        c,
        '+' | '-' | '*' | '/' | '%' | '^' | '=' | '!' | '<' | '>' | '~' | '&' | '|'
    )
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

    fn lex_regex(&mut self) -> Result<Token, String> {
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
        Ok(Token::Regex(pat))
    }

    fn lex_string(&mut self) -> Result<Token, String> {
        self.advance(); // consume opening "
        let mut s = String::new();
        loop {
            match self.advance() {
                None => return Err("unterminated string".to_string()),
                Some('"') => break,
                Some('\\') => self.push_string_escape(&mut s)?,
                Some(ch) => s.push(ch),
            }
        }
        Ok(Token::StringLit(s))
    }

    fn push_string_escape(&mut self, s: &mut String) -> Result<(), String> {
        match self.advance() {
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
        }
        Ok(())
    }

    fn lex_number(&mut self, first: char) -> Result<Token, String> {
        if first == '0' && matches!(self.peek_at(1), Some('x' | 'X')) {
            return self.lex_hex_number();
        }
        let num = self.lex_decimal_number();
        let val: f64 = num
            .parse()
            .map_err(|e: std::num::ParseFloatError| format!("bad number '{num}': {e}"))?;
        Ok(Token::Number(val))
    }

    fn lex_hex_number(&mut self) -> Result<Token, String> {
        let mut num = String::new();
        num.push(self.advance().unwrap());
        num.push(self.advance().unwrap());
        while matches!(self.peek(), Some(ch) if ch.is_ascii_hexdigit()) {
            num.push(self.advance().unwrap());
        }
        let val = i64::from_str_radix(&num[2..], 16).map_err(|e| e.to_string())?;
        #[allow(clippy::cast_precision_loss)]
        Ok(Token::Number(val as f64))
    }

    fn lex_decimal_number(&mut self) -> String {
        let mut num = String::new();
        while matches!(self.peek(), Some(ch) if ch.is_ascii_digit() || ch == '.') {
            num.push(self.advance().unwrap());
        }
        self.lex_number_exponent(&mut num);
        num
    }

    fn lex_number_exponent(&mut self, num: &mut String) {
        if !matches!(self.peek(), Some('e' | 'E')) {
            return;
        }
        num.push(self.advance().unwrap());
        if matches!(self.peek(), Some('+' | '-')) {
            num.push(self.advance().unwrap());
        }
        while matches!(self.peek(), Some(ch) if ch.is_ascii_digit()) {
            num.push(self.advance().unwrap());
        }
    }

    fn lex_ident_or_keyword(&mut self) -> Token {
        let mut id = String::new();
        while let Some(ch) = self.peek() {
            if ch.is_alphanumeric() || ch == '_' {
                id.push(self.advance().unwrap());
            } else {
                break;
            }
        }
        keyword_token(&id).unwrap_or(Token::Ident(id))
    }

    fn lex_operator(&mut self, ch: char) -> Result<Token, String> {
        self.advance();
        match ch {
            '+' => Ok(self.lex_operator_repeat_or_assign(
                '+',
                Token::Plus,
                Token::Incr,
                Token::PlusAssign,
            )),
            '-' => Ok(self.lex_operator_repeat_or_assign(
                '-',
                Token::Minus,
                Token::Decr,
                Token::MinusAssign,
            )),
            '*' => Ok(self.lex_operator_assign(Token::Star, Token::StarAssign)),
            '/' => Ok(self.lex_operator_assign(Token::Slash, Token::SlashAssign)),
            '%' => Ok(self.lex_operator_assign(Token::Percent, Token::PercentAssign)),
            '^' => Ok(self.lex_operator_assign(Token::Caret, Token::CaretAssign)),
            '=' => Ok(self.lex_operator_assign(Token::Assign, Token::Eq)),
            '!' => Ok(self.lex_bang_operator()),
            '<' => Ok(self.lex_operator_assign(Token::Lt, Token::Le)),
            '>' => Ok(self.lex_gt_operator()),
            '~' => Ok(Token::Match),
            '&' => {
                if self.peek() == Some('&') {
                    self.advance();
                    Ok(Token::And)
                } else {
                    Err("unexpected '&'".to_string())
                }
            }
            '|' => Ok(if self.peek() == Some('|') {
                self.advance();
                Token::Or
            } else {
                Token::Pipe
            }),
            _ => Err(format!("unexpected character '{ch}'")),
        }
    }

    fn lex_operator_repeat_or_assign(
        &mut self,
        repeated_char: char,
        plain: Token,
        repeated: Token,
        assigned: Token,
    ) -> Token {
        if self.peek() == Some(repeated_char) {
            self.advance();
            repeated
        } else if self.peek() == Some('=') {
            self.advance();
            assigned
        } else {
            plain
        }
    }

    fn lex_operator_assign(&mut self, plain: Token, assigned: Token) -> Token {
        if self.peek() == Some('=') {
            self.advance();
            assigned
        } else {
            plain
        }
    }

    fn lex_bang_operator(&mut self) -> Token {
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

    fn lex_gt_operator(&mut self) -> Token {
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

    fn tokenize(&mut self) -> Result<Vec<Token>, String> {
        let mut tokens = Vec::new();
        let mut prev = Token::Eof;

        loop {
            self.skip_whitespace_no_newline();
            let Some(c) = self.peek() else {
                tokens.push(Token::Eof);
                break;
            };

            let tok = self.lex_next_token(c, &prev)?;
            prev = tok.clone();
            tokens.push(tok);
        }

        Ok(tokens)
    }

    fn lex_next_token(&mut self, c: char, prev: &Token) -> Result<Token, String> {
        match c {
            '\n' => Ok(self.lex_newlines()),
            '/' if Lexer::can_start_regex(prev) => self.lex_regex(),
            '"' => self.lex_string(),
            '.' if !matches!(self.peek_at(1), Some('0'..='9')) => {
                self.advance();
                Ok(Token::StringLit(".".to_string()))
            }
            '0'..='9' | '.' => self.lex_number(c),
            'a'..='z' | 'A'..='Z' | '_' => Ok(self.lex_ident_or_keyword()),
            _ => self.lex_punctuation_or_operator(c),
        }
    }

    fn lex_newlines(&mut self) -> Token {
        self.advance();
        while self.peek() == Some('\n') {
            self.advance();
        }
        Token::Newline
    }

    fn lex_punctuation_or_operator(&mut self, c: char) -> Result<Token, String> {
        if let Some(tok) = self.try_lex_single_char_punctuation(c) {
            return Ok(tok);
        }
        if is_operator_start(c) {
            return self.lex_operator(c);
        }
        Err(format!("unexpected character '{c}'"))
    }

    fn try_lex_single_char_punctuation(&mut self, c: char) -> Option<Token> {
        let tok = match c {
            '?' => Token::Question,
            ':' => Token::Colon,
            '$' => Token::Dollar,
            ',' => Token::Comma,
            ';' => Token::Semicolon,
            '(' => Token::LParen,
            ')' => Token::RParen,
            '{' => Token::LBrace,
            '}' => Token::RBrace,
            '[' => Token::LBracket,
            ']' => Token::RBracket,
            _ => return None,
        };
        self.advance();
        Some(tok)
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

    fn parse_function_def(&mut self) -> Result<(String, AwkFunction), String> {
        self.advance(); // consume 'function'
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
        Ok((name, AwkFunction { params, body }))
    }

    fn parse_rule(&mut self) -> Result<AwkRule, String> {
        let pat = self.parse_pattern()?;
        self.skip_terminators();
        if *self.peek() == Token::Comma {
            self.advance();
            self.skip_terminators();
            let pat2 = self.parse_pattern()?;
            self.skip_terminators();
            let action = if *self.peek() == Token::LBrace {
                self.parse_action()?
            } else {
                vec![Stmt::Print(vec![], None)]
            };
            Ok(AwkRule {
                pattern: AwkPattern::Range(Box::new(pat), Box::new(pat2)),
                action,
            })
        } else if *self.peek() == Token::LBrace {
            let action = self.parse_action()?;
            Ok(AwkRule {
                pattern: pat,
                action,
            })
        } else {
            Ok(AwkRule {
                pattern: pat,
                action: vec![Stmt::Print(vec![], None)],
            })
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
                    let (name, func) = self.parse_function_def()?;
                    functions.insert(name, func);
                }
                Token::Begin => {
                    self.advance();
                    self.skip_terminators();
                    begin.extend(self.parse_action()?);
                }
                Token::End => {
                    self.advance();
                    self.skip_terminators();
                    end.extend(self.parse_action()?);
                }
                Token::LBrace => {
                    let action = self.parse_action()?;
                    rules.push(AwkRule {
                        pattern: AwkPattern::All,
                        action,
                    });
                }
                _ => {
                    rules.push(self.parse_rule()?);
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
                Ok(Stmt::Exit(self.parse_optional_expr()?))
            }
            Token::Return => {
                self.advance();
                Ok(Stmt::Return(self.parse_optional_expr()?))
            }
            Token::Delete => self.parse_delete(),
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

    fn parse_delete(&mut self) -> Result<Stmt, String> {
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
        let then_body = self.parse_stmt_body()?;
        let else_body = self.parse_else_clause()?;
        Ok(Stmt::If(cond, then_body, else_body))
    }

    fn parse_else_clause(&mut self) -> Result<Option<Vec<Stmt>>, String> {
        self.skip_terminators();
        if *self.peek() != Token::Else {
            return Ok(None);
        }
        self.advance();
        Ok(Some(self.parse_stmt_body()?))
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
        if let Some(stmt) = self.try_parse_for_in()? {
            return Ok(stmt);
        }
        self.parse_c_style_for()
    }

    fn try_parse_for_in(&mut self) -> Result<Option<Stmt>, String> {
        let Token::Ident(name) = self.peek().clone() else {
            return Ok(None);
        };
        let saved = self.pos;
        self.advance();
        if *self.peek() != Token::In {
            self.pos = saved;
            return Ok(None);
        }
        self.advance();
        let arr_name = match self.advance() {
            Token::Ident(n) => n,
            t => return Err(format!("expected array name in for-in, got {t:?}")),
        };
        self.expect(&Token::RParen)?;
        let body = self.parse_stmt_body()?;
        Ok(Some(Stmt::ForIn(name, arr_name, body)))
    }

    fn parse_c_style_for(&mut self) -> Result<Stmt, String> {
        let init = self.parse_for_stmt_clause(&Token::Semicolon)?;
        let cond = self.parse_for_expr_clause(&Token::Semicolon)?;
        let incr = self.parse_for_stmt_clause(&Token::RParen)?;
        let body = self.parse_stmt_body()?;
        Ok(Stmt::For(init, cond, incr, body))
    }

    fn parse_for_stmt_clause(&mut self, terminator: &Token) -> Result<Option<Box<Stmt>>, String> {
        let clause = if self.peek() == terminator {
            None
        } else {
            Some(Box::new(self.parse_stmt()?))
        };
        self.expect(terminator)?;
        Ok(clause)
    }

    fn parse_for_expr_clause(&mut self, terminator: &Token) -> Result<Option<Expr>, String> {
        let clause = if self.peek() == terminator {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.expect(terminator)?;
        Ok(clause)
    }

    fn parse_stmt_body(&mut self) -> Result<Vec<Stmt>, String> {
        self.skip_terminators();
        if *self.peek() == Token::LBrace {
            self.parse_action()
        } else {
            Ok(vec![self.parse_stmt()?])
        }
    }

    // ---- Expression parsing (precedence climbing) ----

    /// Parse an optional expression (used after `exit` and `return`).
    fn parse_optional_expr(&mut self) -> Result<Option<Expr>, String> {
        if matches!(
            self.peek(),
            Token::Semicolon | Token::Newline | Token::RBrace | Token::Eof
        ) {
            Ok(None)
        } else {
            Ok(Some(self.parse_expr()?))
        }
    }

    fn parse_expr(&mut self) -> Result<Expr, String> {
        self.parse_assign()
    }

    fn parse_assign(&mut self) -> Result<Expr, String> {
        let lhs = self.parse_ternary()?;

        let op = match self.peek() {
            Token::Assign => None,
            Token::PlusAssign => Some(BinOp::Add),
            Token::MinusAssign => Some(BinOp::Sub),
            Token::StarAssign => Some(BinOp::Mul),
            Token::SlashAssign => Some(BinOp::Div),
            Token::PercentAssign => Some(BinOp::Mod),
            Token::CaretAssign => Some(BinOp::Pow),
            _ => return Ok(lhs),
        };
        self.advance();
        let rhs = self.parse_assign()?;
        match op {
            None => Ok(Expr::Assign(Box::new(lhs), Box::new(rhs))),
            Some(binop) => Ok(Expr::OpAssign(binop, Box::new(lhs), Box::new(rhs))),
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
            Token::Number(n) => Ok(self.parse_literal_expr(Expr::Num(n))),
            Token::StringLit(s) => Ok(self.parse_literal_expr(Expr::Str(s))),
            Token::Regex(re) => Ok(self.parse_literal_expr(Expr::Regex(re))),
            Token::Dollar => self.parse_field_ref(),
            Token::LParen => self.parse_grouped_expr(),
            Token::Ident(name) => self.parse_ident_primary(name),
            t => Err(format!("unexpected token in expression: {t:?}")),
        }
    }

    fn parse_literal_expr(&mut self, expr: Expr) -> Expr {
        self.advance();
        expr
    }

    fn parse_field_ref(&mut self) -> Result<Expr, String> {
        self.advance();
        let expr = self.parse_primary()?;
        Ok(Expr::FieldRef(Box::new(expr)))
    }

    fn parse_grouped_expr(&mut self) -> Result<Expr, String> {
        self.advance();
        let expr = self.parse_expr()?;
        self.expect(&Token::RParen)?;
        Ok(expr)
    }

    fn parse_ident_primary(&mut self, name: String) -> Result<Expr, String> {
        self.advance();
        if *self.peek() != Token::LParen {
            return Ok(Expr::Var(name));
        }
        self.advance();
        let args = self.parse_call_args()?;
        self.expect(&Token::RParen)?;
        Ok(Expr::Call(name, args))
    }

    fn parse_call_args(&mut self) -> Result<Vec<Expr>, String> {
        let mut args = Vec::new();
        if *self.peek() == Token::RParen {
            return Ok(args);
        }
        args.push(self.parse_expr()?);
        while *self.peek() == Token::Comma {
            self.advance();
            args.push(self.parse_expr()?);
        }
        Ok(args)
    }
}

// ---------------------------------------------------------------------------
// Simple regex engine
// ---------------------------------------------------------------------------

/// Strip anchor characters from a pattern and return `(inner_pattern, anchored_start, anchored_end)`.
fn strip_anchors(pattern: &str) -> (&str, bool, bool) {
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
    (pat, anchored_start, anchored_end)
}

struct CompiledPattern {
    nodes: Vec<ReNode>,
    anchored_start: bool,
    anchored_end: bool,
}

fn compile_pattern(pattern: &str) -> CompiledPattern {
    let (pat, anchored_start, anchored_end) = strip_anchors(pattern);
    CompiledPattern {
        nodes: compile_regex(pat),
        anchored_start,
        anchored_end,
    }
}

fn start_positions(anchored: bool, text_len: usize) -> Box<dyn Iterator<Item = usize>> {
    if anchored {
        Box::new(std::iter::once(0))
    } else {
        Box::new(0..=text_len)
    }
}

/// Simple regex matching supporting: literal chars, `.` (any), `*` (repeat prev),
/// `+` (one or more), `?` (zero or one), `^` (start), `$` (end), `[...]` char classes,
/// `[^...]` negated, `\d`, `\w`, `\s` and their negations, escape sequences.
fn regex_match(text: &str, pattern: &str) -> bool {
    let cp = compile_pattern(pattern);
    for start in start_positions(cp.anchored_start, text.len()) {
        if let Some(end) = regex_match_here(&cp.nodes, text, start, 0) {
            if !cp.anchored_end || end == text.len() {
                return true;
            }
        }
    }
    false
}

/// Find the first match of `pattern` in `text`, returning (start, end) byte offsets.
fn regex_find(text: &str, pattern: &str) -> Option<(usize, usize)> {
    let cp = compile_pattern(pattern);
    for start in start_positions(cp.anchored_start, text.len()) {
        if let Some(end) = regex_match_here(&cp.nodes, text, start, 0) {
            if (!cp.anchored_end || end == text.len()) && (start < end || start == text.len()) {
                return Some((start, end));
            }
        }
    }
    None
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

/// Parse a backslash escape sequence into a regex piece (e.g. `\d`, `\w`, `\n`).
fn compile_escape(c: char) -> RePiece {
    let word_ranges = vec![
        CcRange::Range('a', 'z'),
        CcRange::Range('A', 'Z'),
        CcRange::Range('0', '9'),
        CcRange::Single('_'),
    ];
    let space_ranges = vec![
        CcRange::Single(' '),
        CcRange::Single('\t'),
        CcRange::Single('\n'),
        CcRange::Single('\r'),
    ];
    match c {
        'd' => RePiece::CharClass(vec![CcRange::Range('0', '9')], false),
        'D' => RePiece::CharClass(vec![CcRange::Range('0', '9')], true),
        'w' => RePiece::CharClass(word_ranges, false),
        'W' => RePiece::CharClass(word_ranges, true),
        's' => RePiece::CharClass(space_ranges, false),
        'S' => RePiece::CharClass(space_ranges, true),
        't' => RePiece::Literal('\t'),
        'n' => RePiece::Literal('\n'),
        'r' => RePiece::Literal('\r'),
        _ => RePiece::Literal(c),
    }
}

/// Parse a `[...]` character class from `chars` starting at position `i` (just past `[`).
fn compile_char_class(chars: &[char], mut i: usize) -> (RePiece, usize) {
    let negated = i < chars.len() && chars[i] == '^';
    if negated {
        i += 1;
    }
    let mut ranges = Vec::new();
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
    (RePiece::CharClass(ranges, negated), i)
}

fn compile_regex(pat: &str) -> Vec<ReNode> {
    let chars: Vec<char> = pat.chars().collect();
    let mut nodes = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let (piece, new_i) = compile_next_piece(&chars, i);
        i = new_i;
        let node = attach_quantifier(&chars, &mut i, piece);
        nodes.push(node);
    }

    nodes
}

fn compile_next_piece(chars: &[char], mut i: usize) -> (RePiece, usize) {
    let piece = match chars[i] {
        '.' => {
            i += 1;
            RePiece::Dot
        }
        '\\' if i + 1 < chars.len() => {
            i += 1;
            let c = chars[i];
            i += 1;
            compile_escape(c)
        }
        '[' => {
            i += 1;
            let (p, new_i) = compile_char_class(chars, i);
            i = new_i;
            p
        }
        c => {
            i += 1;
            RePiece::Literal(c)
        }
    };
    (piece, i)
}

fn attach_quantifier(chars: &[char], i: &mut usize, piece: RePiece) -> ReNode {
    if *i >= chars.len() {
        return ReNode::Piece(piece);
    }
    let kind = match chars[*i] {
        '*' => RepeatKind::Star,
        '+' => RepeatKind::Plus,
        '?' => RepeatKind::Question,
        _ => return ReNode::Piece(piece),
    };
    *i += 1;
    ReNode::Repeat(piece, kind)
}

fn piece_matches(piece: &RePiece, ch: char) -> bool {
    match piece {
        RePiece::Literal(c) => ch == *c,
        RePiece::Dot => true,
        RePiece::CharClass(ranges, negated) => {
            let found = ranges.iter().any(|r| match r {
                CcRange::Single(c) => ch == *c,
                CcRange::Range(lo, hi) => ch >= *lo && ch <= *hi,
            });
            found != *negated
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
        ReNode::Piece(piece) => regex_match_piece_chars(nodes, chars, pos, node_idx, piece),
        ReNode::Repeat(piece, kind) => {
            regex_match_repeat_chars(nodes, chars, pos, node_idx, piece, *kind)
        }
    }
}

fn regex_match_piece_chars(
    nodes: &[ReNode],
    chars: &[char],
    pos: usize,
    node_idx: usize,
    piece: &RePiece,
) -> Option<usize> {
    if pos < chars.len() && piece_matches(piece, chars[pos]) {
        regex_match_chars(nodes, chars, pos + 1, node_idx + 1)
    } else {
        None
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
        self.update_nf();
        if n > 0 {
            self.rebuild_record();
        } else {
            self.resplit_from_record0();
        }
    }

    fn update_nf(&mut self) {
        #[allow(clippy::cast_precision_loss)]
        let nf = (self.fields.len() - 1) as f64;
        self.set_var("NF", AwkValue::Num(nf));
    }

    /// Re-split fields from $0 after $0 was set directly.
    fn resplit_from_record0(&mut self) {
        let record = self.fields[0].clone();
        let split = self.split_record(&record);
        self.fields.truncate(1);
        self.fields.extend(split);
        self.update_nf();
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
            Expr::Regex(re) => self.eval_regex_expr(re),
            Expr::Var(name) => self.get_var(name),
            Expr::FieldRef(idx_expr) => self.eval_field_ref(idx_expr),
            Expr::ArrayRef(name, idx_expr) => self.eval_array_ref(name, idx_expr),
            Expr::Assign(lhs, rhs) => self.eval_assign_expr(lhs, rhs),
            Expr::OpAssign(op, lhs, rhs) => self.eval_op_assign_expr(*op, lhs, rhs),
            Expr::BinOp(op, lhs, rhs) => self.eval_binop_expr(*op, lhs, rhs),
            Expr::UnaryOp(op, expr) => self.eval_unary_expr(*op, expr),
            Expr::PreIncr(expr) => self.eval_pre_update(expr, 1.0),
            Expr::PreDecr(expr) => self.eval_pre_update(expr, -1.0),
            Expr::PostIncr(expr) => self.eval_post_update(expr, 1.0),
            Expr::PostDecr(expr) => self.eval_post_update(expr, -1.0),
            Expr::Ternary(cond, then_expr, else_expr) => {
                self.eval_ternary_expr(cond, then_expr, else_expr)
            }
            Expr::MatchOp(positive, expr, re) => self.eval_match_expr(*positive, expr, re),
            Expr::Concat(lhs, rhs) => self.eval_concat_expr(lhs, rhs),
            Expr::InArray(key_expr, arr) => self.eval_in_array_expr(key_expr, arr),
            Expr::Call(name, args) => self.call_function(name, args),
        }
    }

    fn eval_regex_expr(&self, re: &str) -> AwkValue {
        let line = self.get_field(0).to_str();
        AwkValue::Num(bool_to_num(regex_match(&line, re)))
    }

    fn eval_field_ref(&mut self, idx_expr: &Expr) -> AwkValue {
        let idx = self.eval_expr(idx_expr).to_num();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = idx.max(0.0) as usize;
        self.get_field(n)
    }

    fn eval_array_ref(&mut self, name: &str, idx_expr: &Expr) -> AwkValue {
        let key = self.eval_expr(idx_expr).to_str();
        self.arrays
            .get(name)
            .and_then(|m| m.get(&key))
            .cloned()
            .unwrap_or(AwkValue::Uninitialized)
    }

    fn eval_assign_expr(&mut self, lhs: &Expr, rhs: &Expr) -> AwkValue {
        let val = self.eval_expr(rhs);
        self.assign_to(lhs, val.clone());
        val
    }

    fn eval_op_assign_expr(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> AwkValue {
        let old = self.eval_expr(lhs);
        let rval = self.eval_expr(rhs);
        let result = self.apply_binop(op, &old, &rval);
        self.assign_to(lhs, result.clone());
        result
    }

    fn eval_binop_expr(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> AwkValue {
        let l = self.eval_expr(lhs);
        let r = self.eval_expr(rhs);
        self.apply_binop(op, &l, &r)
    }

    fn eval_unary_expr(&mut self, op: UnaryOp, expr: &Expr) -> AwkValue {
        let val = self.eval_expr(expr);
        match op {
            UnaryOp::Neg => AwkValue::Num(-val.to_num()),
            UnaryOp::Pos => AwkValue::Num(val.to_num()),
            UnaryOp::Not => AwkValue::Num(bool_to_num(!val.is_truthy())),
        }
    }

    fn eval_pre_update(&mut self, expr: &Expr, delta: f64) -> AwkValue {
        let val = self.eval_expr(expr).to_num() + delta;
        let result = AwkValue::Num(val);
        self.assign_to(expr, result.clone());
        result
    }

    fn eval_post_update(&mut self, expr: &Expr, delta: f64) -> AwkValue {
        let old = self.eval_expr(expr).to_num();
        self.assign_to(expr, AwkValue::Num(old + delta));
        AwkValue::Num(old)
    }

    fn eval_ternary_expr(&mut self, cond: &Expr, then_expr: &Expr, else_expr: &Expr) -> AwkValue {
        if self.eval_expr(cond).is_truthy() {
            self.eval_expr(then_expr)
        } else {
            self.eval_expr(else_expr)
        }
    }

    fn eval_match_expr(&mut self, positive: bool, expr: &Expr, re: &str) -> AwkValue {
        let s = self.eval_expr(expr).to_str();
        let matched = regex_match(&s, re);
        AwkValue::Num(bool_to_num(if positive { matched } else { !matched }))
    }

    fn eval_concat_expr(&mut self, lhs: &Expr, rhs: &Expr) -> AwkValue {
        let l = self.eval_expr(lhs).to_str();
        let r = self.eval_expr(rhs).to_str();
        AwkValue::Str(format!("{l}{r}"))
    }

    fn eval_in_array_expr(&mut self, key_expr: &Expr, arr: &str) -> AwkValue {
        let key = self.eval_expr(key_expr).to_str();
        let has = self.arrays.get(arr).is_some_and(|m| m.contains_key(&key));
        AwkValue::Num(bool_to_num(has))
    }

    fn assign_to(&mut self, target: &Expr, val: AwkValue) {
        match target {
            Expr::Var(name) => self.set_var(name, val),
            Expr::FieldRef(idx_expr) => self.assign_to_field(idx_expr, &val),
            Expr::ArrayRef(name, idx_expr) => self.assign_to_array(name, idx_expr, val),
            _ => {} // Ignore assignments to non-lvalues (awk just ignores these)
        }
    }

    fn assign_to_field(&mut self, idx_expr: &Expr, val: &AwkValue) {
        let idx = self.eval_expr(idx_expr).to_num();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = idx.max(0.0) as usize;
        self.set_field(n, val.to_str());
    }

    fn assign_to_array(&mut self, name: &str, idx_expr: &Expr, val: AwkValue) {
        let key = self.eval_expr(idx_expr).to_str();
        self.arrays
            .entry(name.to_string())
            .or_default()
            .insert(key, val);
    }

    fn apply_binop(&self, op: BinOp, l: &AwkValue, r: &AwkValue) -> AwkValue {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::Pow => {
                self.apply_numeric_binop(op, l, r)
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                self.apply_compare_binop(op, l, r)
            }
            BinOp::And | BinOp::Or => self.apply_logic_binop(op, l, r),
        }
    }

    #[allow(clippy::unused_self)]
    fn apply_numeric_binop(&self, op: BinOp, l: &AwkValue, r: &AwkValue) -> AwkValue {
        let left = l.to_num();
        let right = r.to_num();
        let value = match op {
            BinOp::Add => left + right,
            BinOp::Sub => left - right,
            BinOp::Mul => left * right,
            BinOp::Div => {
                if right == 0.0 {
                    0.0
                } else {
                    left / right
                }
            }
            BinOp::Mod => {
                if right == 0.0 {
                    0.0
                } else {
                    left % right
                }
            }
            BinOp::Pow => left.powf(right),
            _ => unreachable!(),
        };
        AwkValue::Num(value)
    }

    fn apply_compare_binop(&self, op: BinOp, l: &AwkValue, r: &AwkValue) -> AwkValue {
        let cmp = self.compare_values(l, r);
        let result = match op {
            BinOp::Eq => cmp == 0,
            BinOp::Ne => cmp != 0,
            BinOp::Lt => cmp < 0,
            BinOp::Gt => cmp > 0,
            BinOp::Le => cmp <= 0,
            BinOp::Ge => cmp >= 0,
            _ => unreachable!(),
        };
        AwkValue::Num(bool_to_num(result))
    }

    #[allow(clippy::unused_self)]
    fn apply_logic_binop(&self, op: BinOp, l: &AwkValue, r: &AwkValue) -> AwkValue {
        let result = match op {
            BinOp::And => l.is_truthy() && r.is_truthy(),
            BinOp::Or => l.is_truthy() || r.is_truthy(),
            _ => unreachable!(),
        };
        AwkValue::Num(bool_to_num(result))
    }

    /// Compare two values using awk rules: if both look numeric, compare numerically;
    /// otherwise compare as strings.
    #[allow(clippy::unused_self)]
    fn compare_values(&self, l: &AwkValue, r: &AwkValue) -> i8 {
        if should_compare_numeric(l, r) {
            compare_nums(l.to_num(), r.to_num())
        } else {
            compare_strs(&l.to_str(), &r.to_str())
        }
    }

    // ---- Built-in functions ----

    fn call_string_builtin(&mut self, name: &str, args: &[Expr]) -> Option<AwkValue> {
        match name {
            "length" => Some(self.call_length_builtin(args)),
            "substr" => Some(self.call_substr_builtin(args)),
            "index" => Some(self.call_index_builtin(args)),
            "split" => Some(self.call_split(args)),
            "sub" | "gsub" => Some(self.call_sub_gsub(name == "gsub", args)),
            "match" => Some(self.call_match_builtin(args)),
            "sprintf" => Some(self.call_sprintf_builtin(args)),
            "tolower" => Some(self.call_case_builtin(args, false)),
            "toupper" => Some(self.call_case_builtin(args, true)),
            _ => None,
        }
    }

    fn call_length_builtin(&mut self, args: &[Expr]) -> AwkValue {
        let s = self.eval_builtin_str_arg(args);
        #[allow(clippy::cast_precision_loss)]
        {
            AwkValue::Num(s.chars().count() as f64)
        }
    }

    fn call_substr_builtin(&mut self, args: &[Expr]) -> AwkValue {
        if args.is_empty() {
            return AwkValue::Str(String::new());
        }
        let s = self.eval_expr(&args[0]).to_str();
        let chars: Vec<char> = s.chars().collect();
        let start_idx = self.substr_start_index(args, chars.len());
        let end_idx = self.substr_end_index(args, start_idx, chars.len());
        AwkValue::Str(chars[start_idx..end_idx].iter().collect())
    }

    fn substr_start_index(&mut self, args: &[Expr], len: usize) -> usize {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let start = (self
            .eval_expr(args.get(1).unwrap_or(&Expr::Num(1.0)))
            .to_num() as isize)
            .max(1) as usize;
        (start - 1).min(len)
    }

    fn substr_end_index(&mut self, args: &[Expr], start_idx: usize, len: usize) -> usize {
        if args.len() < 3 {
            return len;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let slice_len = self.eval_expr(&args[2]).to_num().max(0.0) as usize;
        (start_idx + slice_len).min(len)
    }

    fn call_index_builtin(&mut self, args: &[Expr]) -> AwkValue {
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

    fn call_sprintf_builtin(&mut self, args: &[Expr]) -> AwkValue {
        if args.is_empty() {
            return AwkValue::Str(String::new());
        }
        let fmt = self.eval_expr(&args[0]).to_str();
        let arg_vals: Vec<AwkValue> = args[1..].iter().map(|a| self.eval_expr(a)).collect();
        AwkValue::Str(self.format_string(&fmt, &arg_vals))
    }

    fn call_case_builtin(&mut self, args: &[Expr], uppercase: bool) -> AwkValue {
        let s = self.eval_builtin_str_arg(args);
        if uppercase {
            AwkValue::Str(s.to_uppercase())
        } else {
            AwkValue::Str(s.to_lowercase())
        }
    }

    fn eval_builtin_str_arg(&mut self, args: &[Expr]) -> String {
        if args.is_empty() {
            self.get_field(0).to_str()
        } else {
            self.eval_expr(&args[0]).to_str()
        }
    }

    fn call_split(&mut self, args: &[Expr]) -> AwkValue {
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
        self.arrays.insert(arr_name.clone(), HashMap::new());
        let arr = self.arrays.get_mut(&arr_name).unwrap();
        for (i, part) in parts.iter().enumerate() {
            arr.insert(format!("{}", i + 1), AwkValue::Str((*part).to_string()));
        }
        #[allow(clippy::cast_precision_loss)]
        AwkValue::Num(parts.len() as f64)
    }

    fn call_sub_gsub(&mut self, global: bool, args: &[Expr]) -> AwkValue {
        if args.len() < 2 {
            return AwkValue::Num(0.0);
        }
        let pattern = self.eval_regex_or_str(&args[0]);
        let replacement = self.eval_expr(&args[1]).to_str();
        let (target_str, target_field) = self.resolve_sub_target(args);
        let (result, count) = regex_replace(&target_str, &pattern, &replacement, global);
        self.store_sub_result(args, target_field, result);

        #[allow(clippy::cast_precision_loss)]
        AwkValue::Num(count as f64)
    }

    fn eval_regex_or_str(&mut self, expr: &Expr) -> String {
        match expr {
            Expr::Regex(re) => re.clone(),
            other => self.eval_expr(other).to_str(),
        }
    }

    /// Resolve the target string and optional field index for sub/gsub.
    fn resolve_sub_target(&mut self, args: &[Expr]) -> (String, Option<usize>) {
        if args.len() < 3 {
            return (self.get_field(0).to_str(), Some(0));
        }
        match &args[2] {
            Expr::Var(name) => (self.get_var(name).to_str(), None),
            Expr::FieldRef(idx) => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let n = self.eval_expr(idx).to_num().max(0.0) as usize;
                (self.get_field(n).to_str(), Some(n))
            }
            _ => (self.get_field(0).to_str(), Some(0)),
        }
    }

    /// Store the result of sub/gsub back to the target.
    fn store_sub_result(&mut self, args: &[Expr], target_field: Option<usize>, result: String) {
        if let Some(field_n) = target_field {
            self.set_field(field_n, result);
        } else if let Some(Expr::Var(name)) = args.get(2) {
            self.set_var(name, AwkValue::Str(result));
        }
    }

    fn call_match_builtin(&mut self, args: &[Expr]) -> AwkValue {
        if args.len() < 2 {
            return AwkValue::Num(0.0);
        }
        let s = self.eval_expr(&args[0]).to_str();
        let re = self.eval_regex_or_str(&args[1]);
        match regex_find(&s, &re) {
            Some((start, end)) => self.set_match_found(start, end),
            None => self.set_match_not_found(),
        }
    }

    fn set_match_found(&mut self, start: usize, end: usize) -> AwkValue {
        #[allow(clippy::cast_precision_loss)]
        {
            self.set_var("RSTART", AwkValue::Num((start + 1) as f64));
            self.set_var("RLENGTH", AwkValue::Num((end - start) as f64));
            AwkValue::Num((start + 1) as f64)
        }
    }

    fn set_match_not_found(&mut self) -> AwkValue {
        self.set_var("RSTART", AwkValue::Num(0.0));
        self.set_var("RLENGTH", AwkValue::Num(-1.0));
        AwkValue::Num(0.0)
    }

    fn eval_num_arg(&mut self, args: &[Expr]) -> f64 {
        if args.is_empty() {
            0.0
        } else {
            self.eval_expr(&args[0]).to_num()
        }
    }

    fn call_math_builtin(&mut self, name: &str, args: &[Expr]) -> Option<AwkValue> {
        match name {
            "int" => Some(AwkValue::Num(self.eval_num_arg(args).trunc())),
            "sqrt" => Some(AwkValue::Num(self.eval_num_arg(args).sqrt())),
            "sin" => Some(AwkValue::Num(self.eval_num_arg(args).sin())),
            "cos" => Some(AwkValue::Num(self.eval_num_arg(args).cos())),
            "log" => Some(AwkValue::Num(self.eval_num_arg(args).ln())),
            "exp" => Some(AwkValue::Num(self.eval_num_arg(args).exp())),
            "rand" => Some(AwkValue::Num(self.next_rand())),
            "srand" => Some(self.call_srand(args)),
            _ => None,
        }
    }

    fn call_srand(&mut self, args: &[Expr]) -> AwkValue {
        let old = self.rand_state;
        if args.is_empty() {
            self.rand_state = 0xdead_beef_cafe_babe;
        } else {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                self.rand_state = self.eval_expr(&args[0]).to_num() as u64;
            }
            if self.rand_state == 0 {
                self.rand_state = 1;
            }
        }
        #[allow(clippy::cast_precision_loss)]
        AwkValue::Num(old as f64)
    }

    fn call_function(&mut self, name: &str, args: &[Expr]) -> AwkValue {
        if let Some(val) = self.call_string_builtin(name, args) {
            return val;
        }
        if let Some(val) = self.call_math_builtin(name, args) {
            return val;
        }
        // Try user-defined function
        if let Some(func) = self.functions.get(name).cloned() {
            let arg_vals: Vec<AwkValue> = args.iter().map(|a| self.eval_expr(a)).collect();
            self.call_user_function(&func, &arg_vals)
        } else {
            AwkValue::Uninitialized
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
            match chars[i] {
                '%' => {
                    if !self.push_format_spec(&mut result, &chars, &mut i, args, &mut arg_idx) {
                        break;
                    }
                }
                '\\' => self.push_escaped_format_char(&mut result, &chars, &mut i),
                ch => {
                    result.push(ch);
                    i += 1;
                }
            }
        }

        result
    }

    #[allow(clippy::unused_self)]
    fn push_format_spec(
        &self,
        result: &mut String,
        chars: &[char],
        i: &mut usize,
        args: &[AwkValue],
        arg_idx: &mut usize,
    ) -> bool {
        *i += 1;
        if *i >= chars.len() {
            result.push('%');
            return false;
        }
        if chars[*i] == '%' {
            result.push('%');
            *i += 1;
            return true;
        }
        let Some((flags, width, precision, spec)) = parse_format_spec(chars, i, args, arg_idx)
        else {
            return false;
        };
        let arg = args
            .get(*arg_idx)
            .cloned()
            .unwrap_or(AwkValue::Uninitialized);
        *arg_idx += 1;
        result.push_str(&format_one_spec(spec, width, precision, &flags, &arg));
        true
    }

    #[allow(clippy::unused_self)]
    fn push_escaped_format_char(&self, result: &mut String, chars: &[char], i: &mut usize) {
        *i += 1;
        if *i >= chars.len() {
            result.push('\\');
            return;
        }
        match chars[*i] {
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
        *i += 1;
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

    fn exec_while(&mut self, cond: &Expr, body: &[Stmt]) -> ControlFlow {
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

    fn exec_do_while(&mut self, body: &[Stmt], cond: &Expr) -> ControlFlow {
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

    fn exec_for(
        &mut self,
        init: Option<&Stmt>,
        cond: Option<&Expr>,
        incr: Option<&Stmt>,
        body: &[Stmt],
    ) -> ControlFlow {
        if let Some(init_stmt) = init {
            let cf = self.exec_stmt(init_stmt);
            if !matches!(cf, ControlFlow::None) {
                return cf;
            }
        }
        self.exec_for_loop(cond, incr, body)
    }

    fn exec_for_loop(
        &mut self,
        cond: Option<&Expr>,
        incr: Option<&Stmt>,
        body: &[Stmt],
    ) -> ControlFlow {
        let mut iteration = 0;
        loop {
            if !self.for_condition_holds(cond) {
                break;
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

    fn for_condition_holds(&mut self, cond: Option<&Expr>) -> bool {
        match cond {
            Some(c) => self.eval_expr(c).is_truthy(),
            None => true,
        }
    }

    fn exec_for_in(&mut self, var: &str, arr_name: &str, body: &[Stmt]) -> ControlFlow {
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

    fn exec_print(&mut self, args: &[Expr]) -> ControlFlow {
        if args.is_empty() {
            self.print_record();
        } else {
            self.print_args(args);
        }
        ControlFlow::None
    }

    fn print_record(&mut self) {
        let s = self.get_field(0).to_str();
        let ors = self.get_ors();
        self.write_output(s.as_bytes());
        self.write_output(ors.as_bytes());
    }

    fn print_args(&mut self, args: &[Expr]) {
        let ofs = self.get_ofs();
        let ors = self.get_ors();
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                self.write_output(ofs.as_bytes());
            }
            let val = self.eval_expr(arg);
            self.write_output(val.to_str().as_bytes());
        }
        self.write_output(ors.as_bytes());
    }

    fn exec_stmt(&mut self, stmt: &Stmt) -> ControlFlow {
        match stmt {
            Stmt::Expr(expr) => self.exec_expr_stmt(expr),
            Stmt::Print(args, _redirect) => self.exec_print(args),
            Stmt::Printf(args, _redirect) => self.exec_printf(args),
            Stmt::If(cond, then_body, else_body) => {
                self.exec_if(cond, then_body, else_body.as_ref())
            }
            Stmt::While(cond, body) => self.exec_while(cond, body),
            Stmt::DoWhile(body, cond) => self.exec_do_while(body, cond),
            Stmt::For(init, cond, incr, body) => {
                self.exec_for(init.as_deref(), cond.as_ref(), incr.as_deref(), body)
            }
            Stmt::ForIn(var, arr_name, body) => self.exec_for_in(var, arr_name, body),
            Stmt::Break => ControlFlow::Break,
            Stmt::Continue => ControlFlow::Continue,
            Stmt::Next => ControlFlow::Next,
            Stmt::Exit(code) => self.exec_exit(code.as_ref()),
            Stmt::Return(expr) => self.exec_return(expr.as_ref()),
            Stmt::Delete(name, idx_expr) => self.exec_delete(name, idx_expr),
            Stmt::Block(stmts) => self.exec_stmts(stmts),
        }
    }

    fn exec_expr_stmt(&mut self, expr: &Expr) -> ControlFlow {
        self.eval_expr(expr);
        ControlFlow::None
    }

    fn exec_printf(&mut self, args: &[Expr]) -> ControlFlow {
        if args.is_empty() {
            return ControlFlow::None;
        }
        let fmt = self.eval_expr(&args[0]).to_str();
        let arg_vals: Vec<AwkValue> = args[1..].iter().map(|a| self.eval_expr(a)).collect();
        let s = self.format_string(&fmt, &arg_vals);
        self.write_output(s.as_bytes());
        ControlFlow::None
    }

    fn exec_if(
        &mut self,
        cond: &Expr,
        then_body: &[Stmt],
        else_body: Option<&Vec<Stmt>>,
    ) -> ControlFlow {
        if self.eval_expr(cond).is_truthy() {
            self.exec_stmts(then_body)
        } else if let Some(eb) = else_body {
            self.exec_stmts(eb)
        } else {
            ControlFlow::None
        }
    }

    fn exec_exit(&mut self, code: Option<&Expr>) -> ControlFlow {
        let c = code.map_or(0, |e| {
            #[allow(clippy::cast_possible_truncation)]
            {
                self.eval_expr(e).to_num() as i32
            }
        });
        ControlFlow::Exit(c)
    }

    fn exec_return(&mut self, expr: Option<&Expr>) -> ControlFlow {
        let val = expr.map_or(AwkValue::Uninitialized, |e| self.eval_expr(e));
        ControlFlow::Return(val)
    }

    fn exec_delete(&mut self, name: &str, idx_expr: &Expr) -> ControlFlow {
        let key = self.eval_expr(idx_expr).to_str();
        if let Some(arr) = self.arrays.get_mut(name) {
            arr.remove(&key);
        }
        ControlFlow::None
    }

    /// Test a simple (non-range) pattern against the current record.
    fn test_simple_pattern(&mut self, pattern: &AwkPattern) -> bool {
        match pattern {
            AwkPattern::Regex(re) => {
                let line = self.get_field(0).to_str();
                regex_match(&line, re)
            }
            AwkPattern::Expr(expr) => self.eval_expr(expr).is_truthy(),
            _ => false,
        }
    }

    /// Test whether a pattern matches the current record.
    fn pattern_matches(&mut self, pattern: &AwkPattern, rule_idx: usize) -> bool {
        match pattern {
            AwkPattern::All => true,
            AwkPattern::Expr(_) | AwkPattern::Regex(_) => self.test_simple_pattern(pattern),
            AwkPattern::Range(start_pat, end_pat) => {
                self.test_range_pattern(start_pat, end_pat, rule_idx)
            }
        }
    }

    fn test_range_pattern(
        &mut self,
        start_pat: &AwkPattern,
        end_pat: &AwkPattern,
        rule_idx: usize,
    ) -> bool {
        while self.range_active.len() <= rule_idx {
            self.range_active.push(false);
        }
        if self.range_active[rule_idx] {
            if self.test_simple_pattern(end_pat) {
                self.range_active[rule_idx] = false;
            }
            true
        } else {
            let start_matches = self.test_simple_pattern(start_pat);
            if start_matches {
                self.range_active[rule_idx] = true;
            }
            start_matches
        }
    }

    /// Split input content into records based on the current RS.
    fn split_into_records<'a>(&self, content: &'a str) -> Vec<&'a str> {
        let rs = self.get_var("RS").to_str();
        if rs.is_empty() || rs == "\n" {
            content.lines().collect()
        } else if rs.len() == 1 {
            content.split(rs.chars().next().unwrap()).collect()
        } else {
            content.lines().collect()
        }
    }

    /// Process one record against all rules, returning `true` if early exit requested.
    fn process_record(&mut self, program: &AwkProgram) -> Option<i32> {
        for (rule_idx, rule) in program.rules.iter().enumerate() {
            if self.pattern_matches(&rule.pattern, rule_idx) {
                match self.exec_stmts(&rule.action) {
                    ControlFlow::Next => break,
                    ControlFlow::Exit(code) => return Some(code),
                    _ => {}
                }
            }
        }
        None
    }

    /// Run the full awk program on the given input.
    fn run(&mut self, program: &AwkProgram, inputs: &[(String, String)]) -> i32 {
        self.functions.clone_from(&program.functions);

        if let ControlFlow::Exit(code) = self.exec_stmts(&program.begin) {
            self.exit_code = code;
            self.exec_stmts(&program.end);
            return self.exit_code;
        }

        if let Some(code) = self.run_main_loop(program, inputs) {
            self.exit_code = code;
            self.exec_stmts(&program.end);
            return self.exit_code;
        }

        if let ControlFlow::Exit(code) = self.exec_stmts(&program.end) {
            self.exit_code = code;
        }
        self.exit_code
    }

    fn run_main_loop(&mut self, program: &AwkProgram, inputs: &[(String, String)]) -> Option<i32> {
        let mut nr: f64 = 0.0;
        for (filename, content) in inputs {
            if let Some(code) = self.run_input(program, filename, content, &mut nr) {
                return Some(code);
            }
        }
        None
    }

    fn run_input(
        &mut self,
        program: &AwkProgram,
        filename: &str,
        content: &str,
        nr: &mut f64,
    ) -> Option<i32> {
        let records = self.split_into_records(content);
        let mut fnr: f64 = 0.0;
        self.set_var("FILENAME", AwkValue::Str(filename.to_string()));
        for record in records {
            *nr += 1.0;
            fnr += 1.0;
            self.set_var("NR", AwkValue::Num(*nr));
            self.set_var("FNR", AwkValue::Num(fnr));
            self.set_record(record);
            if let Some(code) = self.process_record(program) {
                return Some(code);
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Regex replace helper
// ---------------------------------------------------------------------------

fn regex_replace(text: &str, pattern: &str, replacement: &str, global: bool) -> (String, usize) {
    let mut result = String::new();
    let mut count = 0;
    let mut search_start = 0;

    while search_start <= text.len() {
        let remaining = &text[search_start..];
        let Some((match_start, match_end)) = regex_find(remaining, pattern) else {
            result.push_str(remaining);
            break;
        };
        result.push_str(&remaining[..match_start]);
        apply_replacement(&mut result, &remaining[match_start..match_end], replacement);
        count += 1;
        search_start += match_end.max(1);

        if !global {
            append_remainder(&mut result, text, search_start);
            break;
        }
    }

    (result, count)
}

fn apply_replacement(result: &mut String, matched: &str, replacement: &str) {
    let repl = replacement.replace('&', matched);
    result.push_str(&repl);
}

fn append_remainder(result: &mut String, text: &str, start: usize) {
    if start <= text.len() {
        result.push_str(&text[start..]);
    }
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

#[allow(clippy::struct_excessive_bools)]
struct FormatFlags {
    left_align: bool,
    zero_pad: bool,
    plus_sign: bool,
    space_sign: bool,
}

fn parse_format_flags(chars: &[char], mut i: usize) -> (FormatFlags, usize) {
    let mut flags = FormatFlags {
        left_align: false,
        zero_pad: false,
        plus_sign: false,
        space_sign: false,
    };
    while i < chars.len() {
        match chars[i] {
            '-' => flags.left_align = true,
            '0' => flags.zero_pad = true,
            '+' => flags.plus_sign = true,
            ' ' => flags.space_sign = true,
            _ => break,
        }
        i += 1;
    }
    (flags, i)
}

fn parse_format_spec(
    chars: &[char],
    i: &mut usize,
    args: &[AwkValue],
    arg_idx: &mut usize,
) -> Option<(FormatFlags, usize, Option<usize>, char)> {
    let (flags, new_i) = parse_format_flags(chars, *i);
    *i = new_i;
    let width = parse_format_width(chars, i, args, arg_idx);
    let precision = parse_format_precision(chars, i, args, arg_idx);
    if *i >= chars.len() {
        return None;
    }
    let spec = chars[*i];
    *i += 1;
    Some((flags, width, precision, spec))
}

fn parse_format_width(
    chars: &[char],
    i: &mut usize,
    args: &[AwkValue],
    arg_idx: &mut usize,
) -> usize {
    if *i < chars.len() && chars[*i] == '*' {
        *i += 1;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let width = args.get(*arg_idx).map_or(0, |v| v.to_num() as usize);
        *arg_idx += 1;
        return width;
    }
    parse_format_digits(chars, i)
}

fn parse_format_precision(
    chars: &[char],
    i: &mut usize,
    args: &[AwkValue],
    arg_idx: &mut usize,
) -> Option<usize> {
    if *i >= chars.len() || chars[*i] != '.' {
        return None;
    }
    *i += 1;
    if *i < chars.len() && chars[*i] == '*' {
        *i += 1;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let precision = args.get(*arg_idx).map_or(0, |v| v.to_num() as usize);
        *arg_idx += 1;
        return Some(precision);
    }
    Some(parse_format_digits(chars, i))
}

fn parse_format_digits(chars: &[char], i: &mut usize) -> usize {
    let mut value = 0usize;
    while *i < chars.len() && chars[*i].is_ascii_digit() {
        value = value * 10 + (chars[*i] as usize - '0' as usize);
        *i += 1;
    }
    value
}

fn format_one_spec(
    spec: char,
    width: usize,
    precision: Option<usize>,
    flags: &FormatFlags,
    arg: &AwkValue,
) -> String {
    match spec {
        'd' | 'i' => format_int_spec(width, flags, arg),
        'o' => format_unsigned_spec(width, flags, arg, 'o'),
        'x' => format_unsigned_spec(width, flags, arg, 'x'),
        'X' => format_unsigned_spec(width, flags, arg, 'X'),
        'f' => format_float_spec(width, precision, flags, arg),
        'e' | 'E' => format_scientific_spec(width, precision, flags, arg, spec),
        'g' | 'G' => format_g_spec(width, precision, flags, arg, spec),
        's' => format_str_spec(width, precision, flags, arg),
        'c' => format_char_spec(width, flags, arg),
        _ => format!("%{spec}"),
    }
}

fn pad_char(flags: &FormatFlags) -> char {
    if flags.zero_pad {
        '0'
    } else {
        ' '
    }
}

fn format_int_spec(width: usize, flags: &FormatFlags, arg: &AwkValue) -> String {
    #[allow(clippy::cast_possible_truncation)]
    let n = arg.to_num() as i64;
    let s = apply_sign_prefix(&format!("{}", n.abs()), n < 0, flags);
    pad_string(&s, width, flags.left_align, pad_char(flags))
}

fn format_unsigned_spec(width: usize, flags: &FormatFlags, arg: &AwkValue, base: char) -> String {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = arg.to_num() as u64;
    let s = match base {
        'o' => format!("{n:o}"),
        'X' => format!("{n:X}"),
        _ => format!("{n:x}"),
    };
    pad_string(&s, width, flags.left_align, pad_char(flags))
}

fn format_float_spec(
    width: usize,
    precision: Option<usize>,
    flags: &FormatFlags,
    arg: &AwkValue,
) -> String {
    let n = arg.to_num();
    let prec = precision.unwrap_or(6);
    let s = format_signed_float(n, &format!("{:.prec$}", n.abs()), flags);
    pad_string(&s, width, flags.left_align, pad_char(flags))
}

fn format_scientific_spec(
    width: usize,
    precision: Option<usize>,
    flags: &FormatFlags,
    arg: &AwkValue,
    spec: char,
) -> String {
    let n = arg.to_num();
    let prec = precision.unwrap_or(6);
    let raw = format_scientific(n.abs(), prec, spec == 'E');
    let s = format_signed_float(n, &raw, flags);
    pad_string(&s, width, flags.left_align, pad_char(flags))
}

fn format_g_spec(
    width: usize,
    precision: Option<usize>,
    flags: &FormatFlags,
    arg: &AwkValue,
    spec: char,
) -> String {
    let n = arg.to_num();
    let prec = precision.unwrap_or(6).max(1);
    let raw = format_g(n, prec, spec == 'G');
    let s = format_signed_float(n, &raw, flags);
    pad_string(&s, width, flags.left_align, pad_char(flags))
}

fn format_str_spec(
    width: usize,
    precision: Option<usize>,
    flags: &FormatFlags,
    arg: &AwkValue,
) -> String {
    let mut s = arg.to_str();
    if let Some(prec) = precision {
        let truncated: String = s.chars().take(prec).collect();
        s = truncated;
    }
    pad_string(&s, width, flags.left_align, ' ')
}

fn format_char_spec(width: usize, flags: &FormatFlags, arg: &AwkValue) -> String {
    let s = arg.to_str();
    let c = s.chars().next().map_or(String::new(), |c| c.to_string());
    pad_string(&c, width, flags.left_align, ' ')
}

/// Apply sign prefix (+, space, or -) to a formatted number string.
fn apply_sign_prefix(abs_str: &str, negative: bool, flags: &FormatFlags) -> String {
    if negative {
        format!("-{abs_str}")
    } else if flags.plus_sign {
        format!("+{abs_str}")
    } else if flags.space_sign {
        format!(" {abs_str}")
    } else {
        abs_str.to_string()
    }
}

/// Format a signed float with optional +/space prefix.
fn format_signed_float(n: f64, abs_formatted: &str, flags: &FormatFlags) -> String {
    if n < 0.0 {
        format!("-{abs_formatted}")
    } else if flags.plus_sign {
        format!("+{abs_formatted}")
    } else if flags.space_sign {
        format!(" {abs_formatted}")
    } else {
        abs_formatted.to_string()
    }
}

fn pad_string(s: &str, width: usize, left_align: bool, pad_ch: char) -> String {
    if s.len() >= width {
        return s.to_string();
    }
    if left_align {
        return pad_string_left(s, width, pad_ch);
    }
    pad_string_right(s, width, pad_ch)
}

fn pad_string_left(s: &str, width: usize, pad_ch: char) -> String {
    let padding: String = std::iter::repeat_n(pad_ch, width - s.len()).collect();
    format!("{s}{padding}")
}

fn pad_string_right(s: &str, width: usize, pad_ch: char) -> String {
    let padding: String = std::iter::repeat_n(pad_ch, width - s.len()).collect();
    if pad_ch == '0' && has_sign_prefix(s) {
        let (sign, num) = s.split_at(1);
        format!("{sign}{padding}{num}")
    } else {
        format!("{padding}{s}")
    }
}

fn has_sign_prefix(s: &str) -> bool {
    s.starts_with('-') || s.starts_with('+') || s.starts_with(' ')
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
    let exp = n.abs().log10().floor() as i32;
    if use_fixed_notation(exp, prec) {
        format_g_fixed(n, prec, exp)
    } else {
        format_g_scientific(n, prec, upper)
    }
}

fn use_fixed_notation(exp: i32, prec: usize) -> bool {
    #[allow(clippy::cast_possible_wrap)]
    let limit = prec as i32;
    exp >= -4 && exp < limit
}

fn format_g_fixed(n: f64, prec: usize, exp: i32) -> String {
    #[allow(clippy::cast_possible_wrap)]
    let decimal_digits = ((prec as i32 - 1 - exp).max(0)) as usize;
    trim_decimal_zeros(format!("{n:.decimal_digits$}"))
}

fn format_g_scientific(n: f64, prec: usize, upper: bool) -> String {
    let sig_digits = prec.saturating_sub(1);
    let formatted = format_scientific(n, sig_digits, upper);
    trim_scientific_mantissa(formatted, upper)
}

fn trim_decimal_zeros(s: String) -> String {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

fn trim_scientific_mantissa(s: String, upper: bool) -> String {
    let e_char = if upper { 'E' } else { 'e' };
    let Some(e_pos) = s.find(e_char) else {
        return s;
    };
    let (mantissa_part, exp_part) = s.split_at(e_pos);
    if mantissa_part.contains('.') {
        let trimmed = mantissa_part.trim_end_matches('0').trim_end_matches('.');
        format!("{trimmed}{exp_part}")
    } else {
        s
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

struct AwkArgs<'a> {
    fs: Option<String>,
    pre_vars: Vec<(String, String)>,
    prog_text: Option<String>,
    file_args: Vec<&'a str>,
}

/// Parse command-line arguments for awk, returning structured args or an error code.
fn parse_awk_args<'a>(ctx: &mut UtilContext<'_>, argv: &'a [&'a str]) -> Result<AwkArgs<'a>, i32> {
    let mut args = &argv[1..];
    let mut fs: Option<String> = None;
    let mut pre_vars: Vec<(String, String)> = Vec::new();
    let mut prog_file: Option<String> = None;

    while !args.is_empty() {
        if !parse_awk_option(ctx, &mut args, &mut fs, &mut pre_vars, &mut prog_file)? {
            break;
        }
    }

    let prog_text = load_awk_program_text(ctx, &mut args, prog_file)?;
    if prog_text.is_none() {
        ctx.output.stderr(b"awk: no program text\n");
        return Err(1);
    }

    Ok(AwkArgs {
        fs,
        pre_vars,
        prog_text,
        file_args: args.to_vec(),
    })
}

fn gather_inputs(
    ctx: &mut UtilContext<'_>,
    file_args: &[&str],
    interp: &mut AwkInterpreter,
) -> Result<Vec<(String, String)>, i32> {
    if file_args.is_empty() {
        return Ok(vec![(String::new(), stdin_text(ctx.stdin))]);
    }

    let mut v = Vec::new();
    for path in file_args {
        if apply_awk_var_assignment(interp, path) {
            continue;
        }
        let full = resolve_path(ctx.cwd, path);
        match read_text(ctx.fs, &full) {
            Ok(text) => v.push(((*path).to_string(), text)),
            Err(e) => {
                emit_error(ctx.output, "awk", path, &e);
                return Err(1);
            }
        }
    }
    if let (true, Some(stdin_data)) = (v.is_empty(), ctx.stdin) {
        v.push((
            String::new(),
            String::from_utf8_lossy(stdin_data).to_string(),
        ));
    }
    Ok(v)
}

fn parse_awk_option<'a>(
    ctx: &mut UtilContext<'_>,
    args: &mut &'a [&'a str],
    fs: &mut Option<String>,
    pre_vars: &mut Vec<(String, String)>,
    prog_file: &mut Option<String>,
) -> Result<bool, i32> {
    let Some(arg) = args.first() else {
        return Ok(false);
    };
    if *arg == "-F" {
        return parse_awk_fs_option(ctx, args, fs);
    }
    if let Some(sep) = arg.strip_prefix("-F") {
        *fs = Some(sep.to_string());
        *args = &args[1..];
        return Ok(true);
    }
    if *arg == "-v" {
        return parse_awk_var_option(ctx, args, pre_vars);
    }
    if *arg == "-f" {
        return parse_awk_file_option(ctx, args, prog_file);
    }
    if arg.starts_with('-') && arg.len() > 1 {
        let msg = format!("awk: unknown option: '{arg}'\n");
        ctx.output.stderr(msg.as_bytes());
        return Err(1);
    }
    Ok(false)
}

fn parse_awk_fs_option<'a>(
    ctx: &mut UtilContext<'_>,
    args: &mut &'a [&'a str],
    fs: &mut Option<String>,
) -> Result<bool, i32> {
    let Some(value) = args.get(1) else {
        ctx.output.stderr(b"awk: -F requires an argument\n");
        return Err(1);
    };
    *fs = Some((*value).to_string());
    *args = &args[2..];
    Ok(true)
}

fn parse_awk_var_option<'a>(
    ctx: &mut UtilContext<'_>,
    args: &mut &'a [&'a str],
    pre_vars: &mut Vec<(String, String)>,
) -> Result<bool, i32> {
    let Some(value) = args.get(1) else {
        ctx.output.stderr(b"awk: -v requires an argument\n");
        return Err(1);
    };
    if let Some((name, val)) = value.split_once('=') {
        pre_vars.push((name.to_string(), val.to_string()));
        *args = &args[2..];
        return Ok(true);
    }
    let msg = format!("awk: invalid -v argument: '{value}'\n");
    ctx.output.stderr(msg.as_bytes());
    Err(1)
}

fn parse_awk_file_option<'a>(
    ctx: &mut UtilContext<'_>,
    args: &mut &'a [&'a str],
    prog_file: &mut Option<String>,
) -> Result<bool, i32> {
    let Some(value) = args.get(1) else {
        ctx.output.stderr(b"awk: -f requires an argument\n");
        return Err(1);
    };
    *prog_file = Some((*value).to_string());
    *args = &args[2..];
    Ok(true)
}

fn load_awk_program_text<'a>(
    ctx: &mut UtilContext<'_>,
    args: &mut &'a [&'a str],
    prog_file: Option<String>,
) -> Result<Option<String>, i32> {
    if let Some(file_path) = prog_file {
        let full = resolve_path(ctx.cwd, &file_path);
        return match read_text(ctx.fs, &full) {
            Ok(text) => Ok(Some(text)),
            Err(e) => {
                emit_error(ctx.output, "awk", &file_path, &e);
                Err(1)
            }
        };
    }
    let Some(arg) = args.first() else {
        return Ok(None);
    };
    *args = &args[1..];
    Ok(Some((*arg).to_string()))
}

fn stdin_text(stdin: Option<&[u8]>) -> String {
    stdin.map_or_else(String::new, |data| {
        String::from_utf8_lossy(data).to_string()
    })
}

fn apply_awk_var_assignment(interp: &mut AwkInterpreter, path: &str) -> bool {
    let Some((name, val)) = path.split_once('=') else {
        return false;
    };
    if !is_awk_assignment_name(name) {
        return false;
    }
    if let Ok(n) = val.parse::<f64>() {
        interp.set_var(name, AwkValue::Num(n));
    } else {
        interp.set_var(name, AwkValue::Str(val.to_string()));
    }
    true
}

fn is_awk_assignment_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
}

fn compile_awk_program(ctx: &mut UtilContext<'_>, source: &str) -> Result<AwkProgram, i32> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().map_err(|e| {
        ctx.output.stderr(format!("awk: {e}\n").as_bytes());
        2
    })?;
    let mut parser = Parser::new(tokens);
    parser.parse_program().map_err(|e| {
        ctx.output.stderr(format!("awk: {e}\n").as_bytes());
        2
    })
}

fn init_interpreter(awk_args: &AwkArgs<'_>) -> AwkInterpreter {
    let mut interp = AwkInterpreter::new();
    if let Some(ref sep) = awk_args.fs {
        interp.set_var("FS", AwkValue::Str(sep.clone()));
    }
    for (name, val) in &awk_args.pre_vars {
        if let Ok(n) = val.parse::<f64>() {
            interp.set_var(name, AwkValue::Num(n));
        } else {
            interp.set_var(name, AwkValue::Str(val.clone()));
        }
    }
    interp
}

pub(crate) fn util_awk(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let awk_args = match parse_awk_args(ctx, argv) {
        Ok(a) => a,
        Err(code) => return code,
    };

    let program = match compile_awk_program(ctx, awk_args.prog_text.as_deref().unwrap()) {
        Ok(p) => p,
        Err(code) => return code,
    };

    let mut interp = init_interpreter(&awk_args);

    let inputs = match gather_inputs(ctx, &awk_args.file_args, &mut interp) {
        Ok(v) => v,
        Err(code) => return code,
    };

    let exit_code = interp.run(&program, &inputs);
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
                network: None,
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
                network: None,
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

    // ------------------------------------------------------------------
    // -f flag: read program from file
    // ------------------------------------------------------------------

    fn run_awk_with_prog_file(
        prog_path: &str,
        prog_content: &str,
        input_path: Option<(&str, &str)>,
        stdin: &str,
    ) -> (i32, String, String) {
        let mut fs = MemoryFs::new();
        // Write program file
        let h = fs.open(prog_path, OpenOptions::write()).unwrap();
        fs.write_file(h, prog_content.as_bytes()).unwrap();
        fs.close(h);
        // Optionally write an input file
        let mut argv: Vec<&str> = vec!["awk", "-f", prog_path];
        if let Some((path, content)) = input_path {
            let h = fs.open(path, OpenOptions::write()).unwrap();
            fs.write_file(h, content.as_bytes()).unwrap();
            fs.close(h);
            argv.push(path);
        }
        let mut output = VecOutput::default();
        let stdin_data = stdin.as_bytes();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut output,
                cwd: "/",
                stdin: if stdin.is_empty() && input_path.is_some() {
                    None
                } else {
                    Some(stdin_data)
                },
                state: None,
                network: None,
            };
            util_awk(&mut ctx, &argv)
        };
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        (status, stdout, stderr)
    }

    #[test]
    fn test_f_flag_basic() {
        let (status, out, _) =
            run_awk_with_prog_file("/prog.awk", "{ print $1 }", None, "hello world\nfoo bar\n");
        assert_eq!(status, 0);
        assert_eq!(out, "hello\nfoo\n");
    }

    #[test]
    fn test_f_flag_with_input_file() {
        let (status, out, _) = run_awk_with_prog_file(
            "/prog.awk",
            "{ print NR, $0 }",
            Some(("/data.txt", "alpha\nbeta\n")),
            "",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "1 alpha\n2 beta\n");
    }

    #[test]
    fn test_f_flag_begin_end() {
        let (status, out, _) = run_awk_with_prog_file(
            "/prog.awk",
            "BEGIN { print \"start\" } { print } END { print \"done\" }",
            None,
            "middle\n",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "start\nmiddle\ndone\n");
    }

    #[test]
    fn test_f_flag_missing_file() {
        let (status, _, err) = run_awk_with_args(&["awk", "-f", "/nonexistent.awk"], "hello\n");
        assert_ne!(status, 0);
        assert!(!err.is_empty());
    }

    #[test]
    fn test_f_flag_requires_argument() {
        let (status, _, err) = run_awk_with_args(&["awk", "-f"], "");
        assert_ne!(status, 0);
        assert!(err.contains("-f requires"));
    }

    // ------------------------------------------------------------------
    // Error paths
    // ------------------------------------------------------------------

    #[test]
    fn test_malformed_program_unclosed_brace() {
        let (status, _, err) = run_awk("{ print", "hello\n");
        assert_eq!(status, 2);
        assert!(!err.is_empty());
    }

    #[test]
    fn test_unknown_option() {
        let (status, _, err) = run_awk_with_args(&["awk", "-Z", "{ print }"], "hello\n");
        assert_ne!(status, 0);
        assert!(err.contains("unknown option"));
    }

    #[test]
    fn test_v_requires_argument() {
        let (status, _, err) = run_awk_with_args(&["awk", "-v"], "");
        assert_ne!(status, 0);
        assert!(err.contains("-v requires"));
    }

    #[test]
    fn test_v_invalid_argument() {
        let (status, _, err) = run_awk_with_args(&["awk", "-v", "badarg", "{ print }"], "");
        assert_ne!(status, 0);
        assert!(err.contains("invalid -v"));
    }

    // ------------------------------------------------------------------
    // printf format specifiers
    // ------------------------------------------------------------------

    #[test]
    fn test_printf_zero_padded_int() {
        let (status, out, _) = run_awk("BEGIN { printf \"%05d\\n\", 42 }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "00042\n");
    }

    #[test]
    fn test_printf_wide_float() {
        let (status, out, _) = run_awk("BEGIN { printf \"%8.2f\\n\", 3.14159 }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "    3.14\n");
    }

    #[test]
    fn test_printf_left_aligned_string() {
        let (status, out, _) = run_awk("BEGIN { printf \"%-20s|\\n\", \"hello\" }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "hello               |\n");
    }

    #[test]
    fn test_printf_octal() {
        let (status, out, _) = run_awk("BEGIN { printf \"%o\\n\", 8 }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "10\n");
    }

    #[test]
    fn test_printf_hex_lower() {
        let (status, out, _) = run_awk("BEGIN { printf \"%x\\n\", 255 }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "ff\n");
    }

    #[test]
    fn test_printf_hex_upper() {
        let (status, out, _) = run_awk("BEGIN { printf \"%X\\n\", 255 }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "FF\n");
    }

    #[test]
    fn test_printf_char() {
        let (status, out, _) = run_awk("BEGIN { printf \"%c\\n\", \"A\" }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "A\n");
    }

    #[test]
    fn test_printf_percent_literal() {
        let (status, out, _) = run_awk("BEGIN { printf \"50%%\\n\" }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "50%\n");
    }

    #[test]
    fn test_printf_multiple_specifiers() {
        let (status, out, _) = run_awk(
            "BEGIN { printf \"%s is %d years old\\n\", \"Alice\", 30 }",
            "",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "Alice is 30 years old\n");
    }

    #[test]
    fn test_printf_scientific() {
        let (status, out, _) = run_awk("BEGIN { printf \"%e\\n\", 1234.5 }", "");
        assert_eq!(status, 0);
        // Should contain scientific notation
        assert!(out.contains('e'));
    }

    // ------------------------------------------------------------------
    // User-defined functions
    // ------------------------------------------------------------------

    #[test]
    fn test_user_function_add() {
        let (status, out, _) = run_awk(
            "function add(a, b) { return a + b } BEGIN { print add(3, 4) }",
            "",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "7\n");
    }

    #[test]
    fn test_user_function_recursive_factorial() {
        let (status, out, _) = run_awk(
            "function fact(n) { if (n <= 1) return 1; return n * fact(n - 1) } BEGIN { print fact(5) }",
            "",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "120\n");
    }

    #[test]
    fn test_user_function_with_locals() {
        let (status, out, _) = run_awk(
            "function greet(name) { return \"hello \" name } BEGIN { print greet(\"world\") }",
            "",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "hello world\n");
    }

    // ------------------------------------------------------------------
    // RS/ORS/OFS changes
    // ------------------------------------------------------------------

    #[test]
    fn test_ofs_comma_separated() {
        let (status, out, _) = run_awk(
            "BEGIN { OFS = \",\" } { print $1, $2 }",
            "hello world\nfoo bar\n",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "hello,world\nfoo,bar\n");
    }

    #[test]
    fn test_ors_custom() {
        let (status, out, _) = run_awk("BEGIN { ORS = \"---\" } { print $0 }", "a\nb\nc\n");
        assert_eq!(status, 0);
        assert_eq!(out, "a---b---c---");
    }

    #[test]
    fn test_ofs_tab() {
        let (status, out, _) = run_awk(
            "BEGIN { OFS = \"\\t\" } { print $1, $2, $3 }",
            "a b c\nx y z\n",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "a\tb\tc\nx\ty\tz\n");
    }

    // ------------------------------------------------------------------
    // Delete array element
    // ------------------------------------------------------------------

    #[test]
    fn test_delete_array_element_multiple() {
        let (status, out, _) = run_awk(
            "BEGIN { a[1]=\"x\"; a[2]=\"y\"; a[3]=\"z\"; delete a[2]; for (k in a) print k, a[k] }",
            "",
        );
        assert_eq!(status, 0);
        assert!(out.contains("1 x"));
        assert!(out.contains("3 z"));
        assert!(!out.contains("2 y"));
    }

    #[test]
    fn test_delete_nonexistent_key() {
        let (status, out, _) = run_awk(
            "BEGIN { a[1]=\"x\"; delete a[99]; for (k in a) print k, a[k] }",
            "",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "1 x\n");
    }

    // ------------------------------------------------------------------
    // Range patterns
    // ------------------------------------------------------------------

    #[test]
    fn test_range_pattern() {
        let (status, out, _) = run_awk(
            "/start/,/end/ { print }",
            "before\nstart here\nmiddle\nend here\nafter\n",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "start here\nmiddle\nend here\n");
    }

    #[test]
    fn test_range_pattern_multiple_ranges() {
        let (status, out, _) = run_awk(
            "/BEGIN/,/END/ { print }",
            "skip\nBEGIN\nfirst\nEND\nskip\nBEGIN\nsecond\nEND\nskip\n",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "BEGIN\nfirst\nEND\nBEGIN\nsecond\nEND\n");
    }

    // ------------------------------------------------------------------
    // FILENAME variable
    // ------------------------------------------------------------------

    #[test]
    fn test_filename_variable() {
        let (status, out, _) =
            run_awk_with_file("{ print FILENAME }", "/mydata.txt", "line1\nline2\n");
        assert_eq!(status, 0);
        assert!(out.contains("/mydata.txt"));
    }

    // ------------------------------------------------------------------
    // Multi-file processing
    // ------------------------------------------------------------------

    #[test]
    fn test_multi_file_processing() {
        let mut fs = MemoryFs::new();
        let h1 = fs.open("/file1.txt", OpenOptions::write()).unwrap();
        fs.write_file(h1, b"a\nb\n").unwrap();
        fs.close(h1);
        let h2 = fs.open("/file2.txt", OpenOptions::write()).unwrap();
        fs.write_file(h2, b"c\nd\n").unwrap();
        fs.close(h2);
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut output,
                cwd: "/",
                stdin: None,
                state: None,
                network: None,
            };
            let argv = vec!["awk", "{ print FILENAME, $0 }", "/file1.txt", "/file2.txt"];
            util_awk(&mut ctx, &argv)
        };
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        assert_eq!(status, 0);
        assert!(stdout.contains("/file1.txt a"));
        assert!(stdout.contains("/file1.txt b"));
        assert!(stdout.contains("/file2.txt c"));
        assert!(stdout.contains("/file2.txt d"));
    }

    // ------------------------------------------------------------------
    // next statement (additional test)
    // ------------------------------------------------------------------

    #[test]
    fn test_next_in_begin_pattern() {
        let (status, out, _) = run_awk("NR == 2 { next } { print NR, $0 }", "one\ntwo\nthree\n");
        assert_eq!(status, 0);
        assert!(out.contains("1 one"));
        assert!(!out.contains("2 two"));
        assert!(out.contains("3 three"));
    }

    // ------------------------------------------------------------------
    // Ternary operator
    // ------------------------------------------------------------------

    #[test]
    fn test_ternary_in_print() {
        let (status, out, _) = run_awk("{ print ($1 > 0 ? \"pos\" : \"neg\") }", "5\n-3\n0\n");
        assert_eq!(status, 0);
        assert_eq!(out, "pos\nneg\nneg\n");
    }

    #[test]
    fn test_ternary_nested() {
        let (status, out, _) = run_awk(
            "{ print ($1 > 10 ? \"big\" : ($1 > 0 ? \"small\" : \"zero\")) }",
            "20\n5\n0\n",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "big\nsmall\nzero\n");
    }

    // ------------------------------------------------------------------
    // String concatenation
    // ------------------------------------------------------------------

    #[test]
    fn test_string_concat_explicit() {
        let (status, out, _) = run_awk("BEGIN { a = \"hello\" \" \" \"world\"; print a }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "hello world\n");
    }

    #[test]
    fn test_string_concat_with_vars() {
        let (status, out, _) = run_awk("BEGIN { a = \"foo\"; b = \"bar\"; print a b }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "foobar\n");
    }

    // ------------------------------------------------------------------
    // match() function returning RSTART/RLENGTH
    // ------------------------------------------------------------------

    #[test]
    fn test_match_function_rstart_rlength() {
        let (status, out, _) = run_awk(
            "{ match($0, /[0-9]+/); print RSTART, RLENGTH }",
            "abc123def\n",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "4 3\n");
    }

    #[test]
    fn test_match_function_no_match() {
        let (status, out, _) =
            run_awk("{ match($0, /[0-9]+/); print RSTART, RLENGTH }", "abcdef\n");
        assert_eq!(status, 0);
        assert_eq!(out, "0 -1\n");
    }

    #[test]
    fn test_match_function_at_start() {
        let (status, out, _) = run_awk(
            "{ match($0, /^[a-z]+/); print RSTART, RLENGTH }",
            "hello123\n",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "1 5\n");
    }

    // ------------------------------------------------------------------
    // -v variable assignment
    // ------------------------------------------------------------------

    #[test]
    fn test_v_option_numeric() {
        let (status, out, _) =
            run_awk_with_args(&["awk", "-v", "n=42", "BEGIN { print n + 8 }"], "");
        assert_eq!(status, 0);
        assert_eq!(out, "50\n");
    }

    #[test]
    fn test_v_option_multiple() {
        let (status, out, _) = run_awk_with_args(
            &[
                "awk",
                "-v",
                "a=hello",
                "-v",
                "b=world",
                "BEGIN { print a, b }",
            ],
            "",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "hello world\n");
    }

    // ------------------------------------------------------------------
    // Regex field separator
    // ------------------------------------------------------------------

    #[test]
    fn test_field_separator_comma() {
        let (status, out, _) = run_awk_with_args(&["awk", "-F,", "{ print $2 }"], "a,b,c\n");
        assert_eq!(status, 0);
        assert_eq!(out, "b\n");
    }

    #[test]
    fn test_field_separator_tab() {
        let (status, out, _) =
            run_awk_with_args(&["awk", "-F", "\t", "{ print $1 }"], "hello\tworld\n");
        assert_eq!(status, 0);
        assert_eq!(out, "hello\n");
    }

    // ------------------------------------------------------------------
    // Miscellaneous additional coverage
    // ------------------------------------------------------------------

    #[test]
    fn test_sin_cos_functions() {
        let (status, out, _) = run_awk("BEGIN { printf \"%.4f\\n\", sin(0) }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "0.0000\n");
    }

    #[test]
    fn test_log_exp_functions() {
        let (status, out, _) = run_awk("BEGIN { printf \"%.0f\\n\", exp(0) }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "1\n");
    }

    #[test]
    fn test_substr_no_length() {
        let (status, out, _) = run_awk("{ print substr($0, 3) }", "abcdef\n");
        assert_eq!(status, 0);
        assert_eq!(out, "cdef\n");
    }

    #[test]
    fn test_nf_assignment_extends_fields() {
        let (status, out, _) = run_awk("{ $4 = \"d\"; print }", "a b c\n");
        assert_eq!(status, 0);
        assert_eq!(out, "a b c d\n");
    }

    #[test]
    fn test_print_multiple_with_comma() {
        let (status, out, _) = run_awk("{ print $1, $2, $3 }", "alpha beta gamma\n");
        assert_eq!(status, 0);
        assert_eq!(out, "alpha beta gamma\n");
    }

    #[test]
    fn test_string_ne_comparison() {
        let (status, out, _) = run_awk("$1 != \"skip\" { print }", "keep\nskip\nalso keep\n");
        assert_eq!(status, 0);
        assert_eq!(out, "keep\nalso keep\n");
    }

    #[test]
    fn test_numeric_string_coercion() {
        let (status, out, _) = run_awk("{ print $1 + 0 }", "42abc\n");
        assert_eq!(status, 0);
        assert_eq!(out, "42\n");
    }

    #[test]
    fn test_empty_pattern_prints_all() {
        let (status, out, _) = run_awk("{ print }", "a\nb\nc\n");
        assert_eq!(status, 0);
        assert_eq!(out, "a\nb\nc\n");
    }

    #[test]
    fn test_printf_no_trailing_newline() {
        let (status, out, _) = run_awk("{ printf \"%s \", $0 }", "a\nb\nc\n");
        assert_eq!(status, 0);
        assert_eq!(out, "a b c ");
    }

    #[test]
    fn test_comparison_le_ge() {
        let (status, out, _) = run_awk("$1 >= 3 && $1 <= 5 { print }", "2\n3\n4\n5\n6\n");
        assert_eq!(status, 0);
        assert_eq!(out, "3\n4\n5\n");
    }

    #[test]
    fn test_negative_field_index() {
        // $0 is the whole line; negative/zero field should not crash
        let (status, _out, _) = run_awk("{ print $0 }", "hello\n");
        assert_eq!(status, 0);
    }

    #[test]
    fn test_gsub_returns_count() {
        let (status, out, _) = run_awk("{ n = gsub(/o/, \"0\"); print n, $0 }", "foooo\n");
        assert_eq!(status, 0);
        assert!(out.contains('4'));
        assert!(out.contains("f0000"));
    }

    #[test]
    fn test_split_custom_separator() {
        let (status, out, _) = run_awk(
            "{ n = split($0, a, \",\"); for (i = 1; i <= n; i++) print a[i] }",
            "one,two,three\n",
        );
        assert_eq!(status, 0);
        assert_eq!(out, "one\ntwo\nthree\n");
    }

    #[test]
    fn test_sprintf_hex() {
        let (status, out, _) = run_awk("BEGIN { s = sprintf(\"%x\", 255); print s }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "ff\n");
    }

    #[test]
    fn test_sprintf_octal() {
        let (status, out, _) = run_awk("BEGIN { s = sprintf(\"%o\", 255); print s }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "377\n");
    }

    #[test]
    fn test_compound_assign_caret() {
        let (status, out, _) = run_awk("BEGIN { x = 2; x ^= 3; print x }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "8\n");
    }

    #[test]
    fn test_pre_decrement() {
        let (status, out, _) = run_awk("BEGIN { x = 5; print --x }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "4\n");
    }

    #[test]
    fn test_post_decrement() {
        let (status, out, _) = run_awk("BEGIN { x = 5; print x-- }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "5\n");
    }

    #[test]
    fn test_regex_optional() {
        let (status, out, _) = run_awk("/ab?c/ { print }", "ac\nabc\nabbc\n");
        assert_eq!(status, 0);
        assert_eq!(out, "ac\nabc\n");
    }

    #[test]
    fn test_regex_anchored_start() {
        let (status, out, _) = run_awk("/^hello/ { print }", "hello world\nworld hello\n");
        assert_eq!(status, 0);
        assert_eq!(out, "hello world\n");
    }

    #[test]
    fn test_regex_anchored_end() {
        let (status, out, _) = run_awk("/world$/ { print }", "hello world\nworld hello\n");
        assert_eq!(status, 0);
        assert_eq!(out, "hello world\n");
    }

    #[test]
    fn test_multiline_program() {
        let prog = r"
BEGIN { count = 0 }
/error/ { count++ }
END { print count }
";
        let (status, out, _) = run_awk(prog, "ok\nerror one\nok\nerror two\nerror three\n");
        assert_eq!(status, 0);
        assert_eq!(out, "3\n");
    }

    #[test]
    fn test_array_with_compound_key() {
        // Simulate multi-dimensional with compound key
        let (status, out, _) = run_awk("BEGIN { a[\"x,y\"] = 42; print a[\"x,y\"] }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "42\n");
    }

    #[test]
    fn test_fs_set_in_begin() {
        let (status, out, _) = run_awk("BEGIN { FS = \":\" } { print $2 }", "a:b:c\nd:e:f\n");
        assert_eq!(status, 0);
        assert_eq!(out, "b\ne\n");
    }

    #[test]
    fn test_printf_g_format() {
        let (status, out, _) = run_awk("BEGIN { printf \"%g\\n\", 0.00123 }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "0.00123\n");
    }

    #[test]
    fn test_printf_plus_flag() {
        let (status, out, _) = run_awk("BEGIN { printf \"%+d\\n\", 42 }", "");
        assert_eq!(status, 0);
        assert_eq!(out, "+42\n");
    }

    #[test]
    fn test_printf_space_flag() {
        let (status, out, _) = run_awk("BEGIN { printf \"% d\\n\", 42 }", "");
        assert_eq!(status, 0);
        assert_eq!(out, " 42\n");
    }
}
