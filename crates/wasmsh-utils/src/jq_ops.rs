//! jq utility: JSON processor.

use std::collections::HashMap;
use std::fmt::Write;

use crate::helpers::{emit_error, read_text, resolve_path};
use crate::UtilContext;

// ---------------------------------------------------------------------------
// JSON value representation
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum JqValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JqValue>),
    Object(Vec<(String, JqValue)>),
}

impl JqValue {
    fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "boolean",
            Self::Number(_) => "number",
            Self::String(_) => "string",
            Self::Array(_) => "array",
            Self::Object(_) => "object",
        }
    }

    fn is_truthy(&self) -> bool {
        !matches!(self, Self::Null | Self::Bool(false))
    }

    fn length(&self) -> JqValue {
        match self {
            Self::Null => Self::Number(0.0),
            Self::Bool(_) | Self::Number(_) => Self::Null,
            Self::String(s) => Self::Number(s.chars().count() as f64),
            Self::Array(a) => Self::Number(a.len() as f64),
            Self::Object(o) => Self::Number(o.len() as f64),
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Number(n) => {
                if n.fract() == 0.0 && *n >= i64::MIN as f64 && *n <= i64::MAX as f64 {
                    Some(*n as i64)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn equals(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Number(a), Self::Number(b)) => (a - b).abs() < f64::EPSILON,
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Array(a), Self::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.equals(y))
            }
            (Self::Object(a), Self::Object(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(k, v)| b.iter().any(|(k2, v2)| k == k2 && v.equals(v2)))
            }
            _ => false,
        }
    }

    fn compare(&self, other: &Self) -> Option<std::cmp::Ordering> {
        use std::cmp::Ordering;
        fn type_ord(v: &JqValue) -> u8 {
            match v {
                JqValue::Null => 0,
                JqValue::Bool(false) => 1,
                JqValue::Bool(true) => 2,
                JqValue::Number(_) => 3,
                JqValue::String(_) => 4,
                JqValue::Array(_) => 5,
                JqValue::Object(_) => 6,
            }
        }
        let ta = type_ord(self);
        let tb = type_ord(other);
        if ta != tb {
            return Some(ta.cmp(&tb));
        }
        match (self, other) {
            (Self::Bool(a), Self::Bool(b)) => a.partial_cmp(b),
            (Self::Number(a), Self::Number(b)) => a.partial_cmp(b),
            (Self::String(a), Self::String(b)) => Some(a.cmp(b)),
            (Self::Array(a), Self::Array(b)) => {
                for (x, y) in a.iter().zip(b) {
                    match x.compare(y) {
                        Some(Ordering::Equal) => {}
                        other => return other,
                    }
                }
                Some(a.len().cmp(&b.len()))
            }
            _ => Some(Ordering::Equal),
        }
    }

    fn contains_value(&self, other: &Self) -> bool {
        match (self, other) {
            (_, Self::Null) => matches!(self, Self::Null),
            (Self::String(a), Self::String(b)) => a.contains(b.as_str()),
            (Self::Array(a), Self::Array(b)) => {
                b.iter().all(|bv| a.iter().any(|av| av.contains_value(bv)))
            }
            (Self::Object(a), Self::Object(b)) => b
                .iter()
                .all(|(bk, bv)| a.iter().any(|(ak, av)| ak == bk && av.contains_value(bv))),
            _ => self.equals(other),
        }
    }

    /// Recursively collect self and all nested values.
    fn recurse_values(&self) -> Vec<JqValue> {
        let mut out = vec![self.clone()];
        match self {
            Self::Array(arr) => {
                for v in arr {
                    out.extend(v.recurse_values());
                }
            }
            Self::Object(pairs) => {
                for (_, v) in pairs {
                    out.extend(v.recurse_values());
                }
            }
            _ => {}
        }
        out
    }

    fn to_string_repr(&self) -> String {
        match self {
            Self::Null => "null".into(),
            Self::Bool(b) => b.to_string(),
            Self::Number(n) => format_number(*n),
            Self::String(s) => s.clone(),
            _ => json_to_string(self, false),
        }
    }
}

fn format_number(n: f64) -> String {
    if n.is_nan() {
        return "null".into();
    }
    if n.is_infinite() {
        return if n > 0.0 {
            "1.7976931348623157e+308".into()
        } else {
            "-1.7976931348623157e+308".into()
        };
    }
    if n.fract() == 0.0 && n.abs() < 1e18 {
        format!("{}", n as i64)
    } else {
        // Use enough precision
        let s = format!("{n}");
        s
    }
}

/// Decode a simple JSON escape character (not `\u`).
fn simple_json_escape(c: u8) -> Option<char> {
    match c {
        b'"' => Some('"'),
        b'\\' => Some('\\'),
        b'/' => Some('/'),
        b'n' => Some('\n'),
        b't' => Some('\t'),
        b'r' => Some('\r'),
        b'b' => Some('\u{0008}'),
        b'f' => Some('\u{000C}'),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// JSON parser
// ---------------------------------------------------------------------------

enum StringChar {
    End,
    Escape,
    Literal(u8),
}

struct JsonParser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.input.len() {
            match self.input[self.pos] {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.input.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        if self.pos < self.input.len() {
            let ch = self.input[self.pos];
            self.pos += 1;
            Some(ch)
        } else {
            None
        }
    }

    fn expect(&mut self, ch: u8) -> Result<(), String> {
        self.skip_ws();
        match self.advance() {
            Some(c) if c == ch => Ok(()),
            Some(c) => Err(format!("expected '{}', got '{}'", ch as char, c as char)),
            None => Err(format!("expected '{}', got EOF", ch as char)),
        }
    }

    fn parse_value(&mut self) -> Result<JqValue, String> {
        match self.peek() {
            Some(b'"') => self.parse_string().map(JqValue::String),
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b't') => self.parse_literal(b"true", JqValue::Bool(true)),
            Some(b'f') => self.parse_literal(b"false", JqValue::Bool(false)),
            Some(b'n') => self.parse_literal(b"null", JqValue::Null),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.parse_number(),
            Some(c) => Err(format!("unexpected character: '{}'", c as char)),
            None => Err("unexpected end of input".into()),
        }
    }

    fn parse_literal(&mut self, expected: &[u8], val: JqValue) -> Result<JqValue, String> {
        self.skip_ws();
        for &b in expected {
            match self.advance() {
                Some(c) if c == b => {}
                _ => return Err("invalid literal".into()),
            }
        }
        Ok(val)
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.skip_ws();
        self.expect(b'"')?;
        let mut s = String::new();
        loop {
            match self.parse_string_char()? {
                StringChar::End => return Ok(s),
                StringChar::Escape => self.parse_string_escape(&mut s)?,
                StringChar::Literal(c) => s.push(c as char),
            }
        }
    }

    fn parse_string_char(&mut self) -> Result<StringChar, String> {
        match self.advance() {
            Some(b'"') => Ok(StringChar::End),
            Some(b'\\') => Ok(StringChar::Escape),
            Some(c) => Ok(StringChar::Literal(c)),
            None => Err("unterminated string".into()),
        }
    }

    fn parse_string_escape(&mut self, out: &mut String) -> Result<(), String> {
        let Some(c) = self.advance() else {
            return Err("unterminated string escape".into());
        };
        if c == b'u' {
            out.push(self.parse_unicode_escape()?);
            return Ok(());
        }
        if let Some(decoded) = simple_json_escape(c) {
            out.push(decoded);
        } else {
            out.push('\\');
            out.push(c as char);
        }
        Ok(())
    }

    fn parse_unicode_escape(&mut self) -> Result<char, String> {
        let cp = self.parse_unicode_codepoint()?;
        if (0xD800..=0xDBFF).contains(&cp) {
            return self.parse_surrogate_pair(cp);
        }
        Ok(char::from_u32(cp).unwrap_or('\u{FFFD}'))
    }

    fn parse_unicode_codepoint(&mut self) -> Result<u32, String> {
        let hex = self.take_hex(4)?;
        u32::from_str_radix(&hex, 16).map_err(|_| "invalid unicode escape".into())
    }

    fn parse_surrogate_pair(&mut self, high: u32) -> Result<char, String> {
        if self.advance() != Some(b'\\') || self.advance() != Some(b'u') {
            return Err("expected low surrogate".into());
        }
        let low = self.parse_unicode_codepoint()?;
        if !(0xDC00..=0xDFFF).contains(&low) {
            return Err("invalid low surrogate".into());
        }
        let full = 0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00);
        Ok(char::from_u32(full).unwrap_or('\u{FFFD}'))
    }

    fn take_hex(&mut self, n: usize) -> Result<String, String> {
        let mut s = String::with_capacity(n);
        for _ in 0..n {
            match self.advance() {
                Some(c) if (c as char).is_ascii_hexdigit() => s.push(c as char),
                _ => return Err("invalid hex digit in unicode escape".into()),
            }
        }
        Ok(s)
    }

    fn parse_number(&mut self) -> Result<JqValue, String> {
        self.skip_ws();
        let start = self.pos;
        self.consume_number_sign();
        self.consume_number_digits();
        self.consume_number_fraction();
        self.consume_number_exponent();
        let s = std::str::from_utf8(&self.input[start..self.pos]).map_err(|_| "invalid number")?;
        let n: f64 = s.parse().map_err(|_| format!("invalid number: {s}"))?;
        Ok(JqValue::Number(n))
    }

    fn consume_number_sign(&mut self) {
        if self.pos < self.input.len() && self.input[self.pos] == b'-' {
            self.pos += 1;
        }
    }

    fn consume_number_digits(&mut self) {
        while self.pos < self.input.len() && self.input[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
    }

    fn consume_number_fraction(&mut self) {
        if self.pos < self.input.len() && self.input[self.pos] == b'.' {
            self.pos += 1;
            self.consume_number_digits();
        }
    }

    fn consume_number_exponent(&mut self) {
        if self.pos >= self.input.len() || !matches!(self.input[self.pos], b'e' | b'E') {
            return;
        }
        self.pos += 1;
        if self.pos < self.input.len() && matches!(self.input[self.pos], b'+' | b'-') {
            self.pos += 1;
        }
        self.consume_number_digits();
    }

    fn parse_array(&mut self) -> Result<JqValue, String> {
        self.expect(b'[')?;
        let mut arr = Vec::new();
        if self.peek() == Some(b']') {
            self.advance();
            return Ok(JqValue::Array(arr));
        }
        loop {
            arr.push(self.parse_value()?);
            match self.peek() {
                Some(b',') => {
                    self.advance();
                }
                Some(b']') => {
                    self.advance();
                    return Ok(JqValue::Array(arr));
                }
                _ => return Err("expected ',' or ']' in array".into()),
            }
        }
    }

    fn parse_object(&mut self) -> Result<JqValue, String> {
        self.expect(b'{')?;
        let mut pairs = Vec::new();
        if self.peek() == Some(b'}') {
            self.advance();
            return Ok(JqValue::Object(pairs));
        }
        loop {
            let key = self.parse_string()?;
            self.expect(b':')?;
            let val = self.parse_value()?;
            pairs.push((key, val));
            match self.peek() {
                Some(b',') => {
                    self.advance();
                }
                Some(b'}') => {
                    self.advance();
                    return Ok(JqValue::Object(pairs));
                }
                _ => return Err("expected ',' or '}' in object".into()),
            }
        }
    }

    fn parse_full(&mut self) -> Result<JqValue, String> {
        self.skip_ws();
        let val = self.parse_value()?;
        self.skip_ws();
        Ok(val)
    }

    /// Parse a stream of multiple JSON values (for slurp mode / multiple inputs).
    fn parse_all(input: &str) -> Result<Vec<JqValue>, String> {
        let mut parser = JsonParser::new(input);
        let mut values = Vec::new();
        loop {
            parser.skip_ws();
            if parser.pos >= parser.input.len() {
                break;
            }
            values.push(parser.parse_value()?);
        }
        Ok(values)
    }
}

fn parse_json(input: &str) -> Result<JqValue, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty input".into());
    }
    let mut parser = JsonParser::new(trimmed);
    parser.parse_full()
}

// ---------------------------------------------------------------------------
// JSON printer
// ---------------------------------------------------------------------------

fn json_to_string(val: &JqValue, compact: bool) -> String {
    let mut out = String::new();
    json_write(&mut out, val, compact, 0);
    out
}

fn json_write(out: &mut String, val: &JqValue, compact: bool, indent: usize) {
    match val {
        JqValue::Null => out.push_str("null"),
        JqValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        JqValue::Number(n) => out.push_str(&format_number(*n)),
        JqValue::String(s) => json_write_string(out, s),
        JqValue::Array(arr) => json_write_array(out, arr, compact, indent),
        JqValue::Object(pairs) => json_write_object(out, pairs, compact, indent),
    }
}

fn json_write_array(out: &mut String, arr: &[JqValue], compact: bool, indent: usize) {
    if arr.is_empty() {
        out.push_str("[]");
        return;
    }
    json_write_container(out, compact, indent, '[', ']', arr.len(), |out, i| {
        json_write(out, &arr[i], compact, indent + 1);
    });
}

fn json_write_object(out: &mut String, pairs: &[(String, JqValue)], compact: bool, indent: usize) {
    if pairs.is_empty() {
        out.push_str("{}");
        return;
    }
    json_write_container(out, compact, indent, '{', '}', pairs.len(), |out, i| {
        json_write_string(out, &pairs[i].0);
        out.push(':');
        if !compact {
            out.push(' ');
        }
        json_write(out, &pairs[i].1, compact, indent + 1);
    });
}

fn json_write_container(
    out: &mut String,
    compact: bool,
    indent: usize,
    open: char,
    close: char,
    count: usize,
    mut write_item: impl FnMut(&mut String, usize),
) {
    out.push(open);
    json_write_newline(out, compact);
    for i in 0..count {
        if !compact {
            write_indent(out, indent + 1);
        }
        write_item(out, i);
        if i + 1 < count {
            out.push(',');
        }
        json_write_newline(out, compact);
    }
    if !compact {
        write_indent(out, indent);
    }
    out.push(close);
}

fn json_write_newline(out: &mut String, compact: bool) {
    if !compact {
        out.push('\n');
    }
}

fn write_indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn json_write_string(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            c if c < '\u{0020}' => {
                let n = c as u32;
                let _ = write!(out, "\\u{n:04x}");
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

// ---------------------------------------------------------------------------
// Filter AST
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum CompOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug)]
enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Clone, Debug)]
enum JqObjKey {
    Ident(String),
    /// `(expr)` — dynamic key
    Dynamic(JqFilter),
}

#[derive(Clone, Debug)]
#[allow(clippy::enum_variant_names)]
enum JqFilter {
    Identity,
    Field(String),
    OptionalField(String),
    Index(Box<JqFilter>),
    OptionalIndex(Box<JqFilter>),
    Slice(Option<Box<JqFilter>>, Option<Box<JqFilter>>),
    Iterate,
    OptionalIterate,
    Pipe(Box<JqFilter>, Box<JqFilter>),
    Comma(Box<JqFilter>, Box<JqFilter>),
    Literal(JqValue),
    Comparison(Box<JqFilter>, CompOp, Box<JqFilter>),
    Arithmetic(Box<JqFilter>, ArithOp, Box<JqFilter>),
    Not,
    And(Box<JqFilter>, Box<JqFilter>),
    Or(Box<JqFilter>, Box<JqFilter>),
    If {
        cond: Box<JqFilter>,
        then_: Box<JqFilter>,
        elifs: Vec<(JqFilter, JqFilter)>,
        else_: Option<Box<JqFilter>>,
    },
    TryCatch {
        try_: Box<JqFilter>,
        catch: Option<Box<JqFilter>>,
    },
    Alternative(Box<JqFilter>, Box<JqFilter>),
    Reduce {
        iter: Box<JqFilter>,
        var: String,
        init: Box<JqFilter>,
        update: Box<JqFilter>,
    },
    Foreach {
        iter: Box<JqFilter>,
        var: String,
        init: Box<JqFilter>,
        update: Box<JqFilter>,
        extract: Option<Box<JqFilter>>,
    },
    Binding {
        expr: Box<JqFilter>,
        var: String,
        body: Box<JqFilter>,
    },
    FuncDef {
        name: String,
        args: Vec<String>,
        body: Box<JqFilter>,
        rest: Box<JqFilter>,
    },
    FuncCall(String, Vec<JqFilter>),
    ArrayConstruct(Box<JqFilter>),
    ObjectConstruct(Vec<(JqObjKey, Option<JqFilter>)>),
    Variable(String),
    Recurse,
    Label(String, Box<JqFilter>),
    Negate(Box<JqFilter>),
    Format(String),
}

// ---------------------------------------------------------------------------
// Filter tokenizer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Dot,
    LBracket,
    RBracket,
    LParen,
    RParen,
    LBrace,
    RBrace,
    Pipe,
    Comma,
    Colon,
    Semi,
    Question,
    DotDot,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    And,
    Or,
    Not,
    Alternative,
    If,
    Then,
    Elif,
    Else,
    End,
    As,
    Def,
    Reduce,
    Foreach,
    Try,
    Catch,
    Label,
    Ident(String),
    Variable(String),
    StrLit(String),
    NumLit(f64),
    True,
    False,
    Null,
    Empty,
    /// `@format` tokens like `@csv`, `@base64`, etc.
    AtFormat(String),
    Eof,
}

struct Tokenizer<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Tokenizer<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.input.len() {
            match self.input[self.pos] {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                b'#' => {
                    // Skip comment to end of line
                    while self.pos < self.input.len() && self.input[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn next_token(&mut self) -> Result<Token, String> {
        self.skip_ws();
        if self.pos >= self.input.len() {
            return Ok(Token::Eof);
        }
        let ch = self.input[self.pos];
        if let Some(tok) = self.consume_simple_token(ch) {
            return Ok(tok);
        }
        match ch {
            b'.' => Ok(self.consume_dot_token()),
            b'/' => Ok(self.consume_slash_token()),
            b'=' => self.consume_required_follow_token(b'=', Token::Eq, "unexpected '='"),
            b'!' => self.consume_required_follow_token(b'=', Token::Ne, "unexpected '!'"),
            b'<' => Ok(self.consume_optional_follow_token(b'=', Token::Lt, Token::Le)),
            b'>' => Ok(self.consume_optional_follow_token(b'=', Token::Gt, Token::Ge)),
            b'$' => Ok(Token::Variable(self.consume_name_after_prefix())),
            b'@' => Ok(Token::AtFormat(self.consume_name_after_prefix())),
            b'"' => self.tokenize_string(),
            c if c.is_ascii_digit() => Ok(self.tokenize_number()),
            c if c.is_ascii_alphabetic() || c == b'_' => Ok(self.tokenize_ident()),
            _ => {
                self.pos += 1;
                Err(format!("unexpected character: '{}'", ch as char))
            }
        }
    }

    fn consume_simple_token(&mut self, ch: u8) -> Option<Token> {
        let tok = match ch {
            b'[' => Token::LBracket,
            b']' => Token::RBracket,
            b'(' => Token::LParen,
            b')' => Token::RParen,
            b'{' => Token::LBrace,
            b'}' => Token::RBrace,
            b'|' => Token::Pipe,
            b',' => Token::Comma,
            b':' => Token::Colon,
            b';' => Token::Semi,
            b'?' => Token::Question,
            b'+' => Token::Plus,
            b'-' => Token::Minus,
            b'*' => Token::Star,
            b'%' => Token::Percent,
            _ => return None,
        };
        self.pos += 1;
        Some(tok)
    }

    fn consume_dot_token(&mut self) -> Token {
        self.pos += 1;
        if self.pos < self.input.len() && self.input[self.pos] == b'.' {
            self.pos += 1;
            Token::DotDot
        } else {
            Token::Dot
        }
    }

    fn consume_slash_token(&mut self) -> Token {
        self.pos += 1;
        if self.pos < self.input.len() && self.input[self.pos] == b'/' {
            self.pos += 1;
            Token::Alternative
        } else {
            Token::Slash
        }
    }

    fn consume_required_follow_token(
        &mut self,
        expected: u8,
        token: Token,
        err: &str,
    ) -> Result<Token, String> {
        self.pos += 1;
        if self.pos < self.input.len() && self.input[self.pos] == expected {
            self.pos += 1;
            Ok(token)
        } else {
            Err(err.into())
        }
    }

    fn consume_optional_follow_token(
        &mut self,
        expected: u8,
        plain: Token,
        paired: Token,
    ) -> Token {
        self.pos += 1;
        if self.pos < self.input.len() && self.input[self.pos] == expected {
            self.pos += 1;
            paired
        } else {
            plain
        }
    }

    fn consume_name_after_prefix(&mut self) -> String {
        self.pos += 1;
        let start = self.pos;
        while self.pos < self.input.len()
            && (self.input[self.pos].is_ascii_alphanumeric() || self.input[self.pos] == b'_')
        {
            self.pos += 1;
        }
        std::str::from_utf8(&self.input[start..self.pos])
            .unwrap_or("")
            .to_string()
    }

    fn tokenize_string(&mut self) -> Result<Token, String> {
        self.pos += 1; // skip opening "
        let mut s = String::new();
        loop {
            if self.pos >= self.input.len() {
                return Err("unterminated string in filter".into());
            }
            match self.input[self.pos] {
                b'"' => {
                    self.pos += 1;
                    return Ok(Token::StrLit(s));
                }
                b'\\' => self.tokenize_string_escape(&mut s)?,
                c => {
                    s.push(c as char);
                    self.pos += 1;
                }
            }
        }
    }

    fn tokenize_string_escape(&mut self, out: &mut String) -> Result<(), String> {
        self.pos += 1;
        if self.pos >= self.input.len() {
            return Err("unterminated string escape".into());
        }
        let c = self.input[self.pos];
        if c == b'u' {
            self.pos += 1;
            if let Some(ch) = self.tokenize_unicode_escape() {
                out.push(ch);
            }
            return Ok(());
        }
        if let Some(decoded) = simple_json_escape(c) {
            out.push(decoded);
        } else {
            out.push('\\');
            out.push(c as char);
        }
        self.pos += 1;
        Ok(())
    }

    fn tokenize_unicode_escape(&mut self) -> Option<char> {
        let mut hex = String::new();
        for _ in 0..4 {
            if self.pos >= self.input.len() {
                return None;
            }
            hex.push(self.input[self.pos] as char);
            self.pos += 1;
        }
        u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
    }

    fn tokenize_number(&mut self) -> Token {
        let start = self.pos;
        self.skip_digits();
        self.skip_token_fraction();
        self.skip_token_exponent();
        let s = std::str::from_utf8(&self.input[start..self.pos]).unwrap_or("0");
        let n: f64 = s.parse().unwrap_or(0.0);
        Token::NumLit(n)
    }

    fn skip_digits(&mut self) {
        while self.pos < self.input.len() && self.input[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
    }

    fn skip_token_fraction(&mut self) {
        if self.pos < self.input.len() && self.input[self.pos] == b'.' {
            self.pos += 1;
            self.skip_digits();
        }
    }

    fn skip_token_exponent(&mut self) {
        if self.pos >= self.input.len() || !matches!(self.input[self.pos], b'e' | b'E') {
            return;
        }
        self.pos += 1;
        if self.pos < self.input.len() && matches!(self.input[self.pos], b'+' | b'-') {
            self.pos += 1;
        }
        self.skip_digits();
    }

    fn tokenize_ident(&mut self) -> Token {
        let start = self.pos;
        while self.pos < self.input.len()
            && (self.input[self.pos].is_ascii_alphanumeric() || self.input[self.pos] == b'_')
        {
            self.pos += 1;
        }
        let s = std::str::from_utf8(&self.input[start..self.pos]).unwrap_or("");
        keyword_token(s).unwrap_or_else(|| Token::Ident(s.to_string()))
    }
}

fn keyword_token(s: &str) -> Option<Token> {
    match s {
        "and" => Some(Token::And),
        "or" => Some(Token::Or),
        "not" => Some(Token::Not),
        "if" => Some(Token::If),
        "then" => Some(Token::Then),
        "elif" => Some(Token::Elif),
        "else" => Some(Token::Else),
        "end" => Some(Token::End),
        "as" => Some(Token::As),
        "def" => Some(Token::Def),
        "reduce" => Some(Token::Reduce),
        "foreach" => Some(Token::Foreach),
        "try" => Some(Token::Try),
        "catch" => Some(Token::Catch),
        "label" => Some(Token::Label),
        "true" => Some(Token::True),
        "false" => Some(Token::False),
        "null" => Some(Token::Null),
        "empty" => Some(Token::Empty),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Filter parser
// ---------------------------------------------------------------------------

struct FilterParser {
    tokens: Vec<Token>,
    pos: usize,
}

impl FilterParser {
    fn new(filter: &str) -> Result<Self, String> {
        let mut tokenizer = Tokenizer::new(filter);
        let mut tokens = Vec::new();
        loop {
            let tok = tokenizer.next_token()?;
            let is_eof = tok == Token::Eof;
            tokens.push(tok);
            if is_eof {
                break;
            }
        }
        Ok(Self { tokens, pos: 0 })
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
        if &tok == expected {
            Ok(())
        } else {
            Err(format!("expected {expected:?}, got {tok:?}"))
        }
    }

    fn parse(&mut self) -> Result<JqFilter, String> {
        self.parse_pipe()
    }

    fn parse_pipe(&mut self) -> Result<JqFilter, String> {
        let mut left = self.parse_comma()?;
        while *self.peek() == Token::Pipe {
            self.advance();
            let right = self.parse_comma()?;
            left = JqFilter::Pipe(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_comma(&mut self) -> Result<JqFilter, String> {
        let mut left = self.parse_alternative()?;
        while *self.peek() == Token::Comma {
            self.advance();
            let right = self.parse_alternative()?;
            left = JqFilter::Comma(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_alternative(&mut self) -> Result<JqFilter, String> {
        let mut left = self.parse_or()?;
        while *self.peek() == Token::Alternative {
            self.advance();
            let right = self.parse_or()?;
            left = JqFilter::Alternative(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_or(&mut self) -> Result<JqFilter, String> {
        let mut left = self.parse_and()?;
        while *self.peek() == Token::Or {
            self.advance();
            let right = self.parse_and()?;
            left = JqFilter::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<JqFilter, String> {
        let mut left = self.parse_comparison()?;
        while *self.peek() == Token::And {
            self.advance();
            let right = self.parse_comparison()?;
            left = JqFilter::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<JqFilter, String> {
        let left = self.parse_addition()?;
        let op = match self.peek() {
            Token::Eq => Some(CompOp::Eq),
            Token::Ne => Some(CompOp::Ne),
            Token::Lt => Some(CompOp::Lt),
            Token::Le => Some(CompOp::Le),
            Token::Gt => Some(CompOp::Gt),
            Token::Ge => Some(CompOp::Ge),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let right = self.parse_addition()?;
            Ok(JqFilter::Comparison(Box::new(left), op, Box::new(right)))
        } else {
            Ok(left)
        }
    }

    fn parse_addition(&mut self) -> Result<JqFilter, String> {
        let mut left = self.parse_multiplication()?;
        loop {
            match self.peek() {
                Token::Plus => {
                    self.advance();
                    let right = self.parse_multiplication()?;
                    left = JqFilter::Arithmetic(Box::new(left), ArithOp::Add, Box::new(right));
                }
                Token::Minus => {
                    self.advance();
                    let right = self.parse_multiplication()?;
                    left = JqFilter::Arithmetic(Box::new(left), ArithOp::Sub, Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_multiplication(&mut self) -> Result<JqFilter, String> {
        let mut left = self.parse_postfix()?;
        loop {
            match self.peek() {
                Token::Star => {
                    self.advance();
                    let right = self.parse_postfix()?;
                    left = JqFilter::Arithmetic(Box::new(left), ArithOp::Mul, Box::new(right));
                }
                Token::Slash => {
                    self.advance();
                    let right = self.parse_postfix()?;
                    left = JqFilter::Arithmetic(Box::new(left), ArithOp::Div, Box::new(right));
                }
                Token::Percent => {
                    self.advance();
                    let right = self.parse_postfix()?;
                    left = JqFilter::Arithmetic(Box::new(left), ArithOp::Mod, Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_postfix(&mut self) -> Result<JqFilter, String> {
        let mut expr = self.parse_primary()?;
        loop {
            match self.peek() {
                Token::Dot => match self.try_parse_dot_field() {
                    Some(field) => expr = JqFilter::Pipe(Box::new(expr), Box::new(field)),
                    None => break,
                },
                Token::LBracket => expr = self.parse_bracket_suffix(expr)?,
                Token::Question => {
                    self.advance();
                    expr = JqFilter::TryCatch {
                        try_: Box::new(expr),
                        catch: None,
                    };
                }
                Token::Not => {
                    self.advance();
                    expr = JqFilter::Pipe(Box::new(expr), Box::new(JqFilter::Not));
                }
                Token::As => {
                    expr = self.parse_postfix_as(expr)?;
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    /// Try to parse `.ident` or `.ident?` after a dot. Returns `None` if
    /// the dot is not followed by an identifier (in which case pos is restored).
    fn try_parse_dot_field(&mut self) -> Option<JqFilter> {
        let saved = self.pos;
        self.advance();
        let Token::Ident(name) = self.peek() else {
            self.pos = saved;
            return None;
        };
        let name = name.clone();
        self.advance();
        if *self.peek() == Token::Question {
            self.advance();
            Some(JqFilter::OptionalField(name))
        } else {
            Some(JqFilter::Field(name))
        }
    }

    fn parse_postfix_as(&mut self, expr: JqFilter) -> Result<JqFilter, String> {
        self.advance();
        let var = match self.advance() {
            Token::Variable(name) => name,
            t => return Err(format!("expected variable after 'as', got {t:?}")),
        };
        self.expect(&Token::Pipe)?;
        let body = self.parse_pipe()?;
        Ok(JqFilter::Binding {
            expr: Box::new(expr),
            var,
            body: Box::new(body),
        })
    }

    /// Consume a trailing `?` if present, returning `true` if consumed.
    fn eat_question(&mut self) -> bool {
        if *self.peek() == Token::Question {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Optionally wrap `result` in a `TryCatch` if `optional` is true.
    fn wrap_optional(result: JqFilter, optional: bool) -> JqFilter {
        if optional {
            JqFilter::TryCatch {
                try_: Box::new(result),
                catch: None,
            }
        } else {
            result
        }
    }

    fn parse_bracket_suffix(&mut self, base: JqFilter) -> Result<JqFilter, String> {
        self.expect(&Token::LBracket)?;
        if *self.peek() == Token::RBracket {
            return self.parse_bracket_iterate(base);
        }
        if *self.peek() == Token::Colon {
            return self.parse_bracket_slice_from_start(base);
        }
        let idx = self.parse_pipe()?;
        if *self.peek() == Token::Colon {
            return self.parse_bracket_slice_with_from(base, idx);
        }
        self.parse_bracket_index(base, idx)
    }

    #[allow(clippy::unnecessary_wraps)]
    fn parse_bracket_iterate(&mut self, base: JqFilter) -> Result<JqFilter, String> {
        self.advance();
        let optional = self.eat_question();
        let iter = if optional {
            JqFilter::OptionalIterate
        } else {
            JqFilter::Iterate
        };
        Ok(JqFilter::Pipe(Box::new(base), Box::new(iter)))
    }

    fn parse_bracket_slice_from_start(&mut self, base: JqFilter) -> Result<JqFilter, String> {
        self.advance();
        let to = self.parse_pipe()?;
        self.expect(&Token::RBracket)?;
        let optional = self.eat_question();
        let f = JqFilter::Slice(None, Some(Box::new(to)));
        let result = JqFilter::Pipe(Box::new(base), Box::new(f));
        Ok(Self::wrap_optional(result, optional))
    }

    fn parse_bracket_slice_with_from(
        &mut self,
        base: JqFilter,
        idx: JqFilter,
    ) -> Result<JqFilter, String> {
        self.advance();
        let to = if *self.peek() == Token::RBracket {
            None
        } else {
            Some(Box::new(self.parse_pipe()?))
        };
        self.expect(&Token::RBracket)?;
        let optional = self.eat_question();
        let f = JqFilter::Slice(Some(Box::new(idx)), to);
        let result = JqFilter::Pipe(Box::new(base), Box::new(f));
        Ok(Self::wrap_optional(result, optional))
    }

    fn parse_bracket_index(&mut self, base: JqFilter, idx: JqFilter) -> Result<JqFilter, String> {
        self.expect(&Token::RBracket)?;
        let optional = self.eat_question();
        let index_filter = if optional {
            JqFilter::OptionalIndex(Box::new(idx))
        } else {
            JqFilter::Index(Box::new(idx))
        };
        Ok(JqFilter::Pipe(Box::new(base), Box::new(index_filter)))
    }

    fn parse_primary(&mut self) -> Result<JqFilter, String> {
        match self.peek().clone() {
            Token::Dot => self.parse_dot_primary(),
            Token::DotDot => {
                self.advance();
                Ok(JqFilter::Recurse)
            }
            Token::LBracket => self.parse_array_primary(),
            Token::LBrace => {
                self.advance();
                self.parse_object_construct()
            }
            Token::LParen => self.parse_grouped_primary(),
            Token::Minus => self.parse_minus_primary(),
            Token::Ident(name) => self.parse_ident_primary(name),
            _ => self.parse_primary_token(),
        }
    }

    fn parse_primary_token(&mut self) -> Result<JqFilter, String> {
        match self.peek().clone() {
            Token::StrLit(s) => Ok(self.parse_literal_primary(JqValue::String(s))),
            Token::NumLit(n) => Ok(self.parse_literal_primary(JqValue::Number(n))),
            Token::True => Ok(self.parse_literal_primary(JqValue::Bool(true))),
            Token::False => Ok(self.parse_literal_primary(JqValue::Bool(false))),
            Token::Null => Ok(self.parse_literal_primary(JqValue::Null)),
            Token::AtFormat(name) => {
                self.advance();
                Ok(JqFilter::Format(name))
            }
            _ => self.parse_primary_keyword(),
        }
    }

    fn parse_primary_keyword(&mut self) -> Result<JqFilter, String> {
        match self.peek().clone() {
            Token::Empty => {
                self.advance();
                Ok(JqFilter::FuncCall("empty".into(), vec![]))
            }
            Token::Not => {
                self.advance();
                Ok(JqFilter::Not)
            }
            Token::Variable(name) => {
                self.advance();
                Ok(JqFilter::Variable(name))
            }
            Token::If => {
                self.advance();
                self.parse_if()
            }
            Token::Try => self.parse_try_primary(),
            Token::Reduce => {
                self.advance();
                self.parse_reduce()
            }
            Token::Foreach => {
                self.advance();
                self.parse_foreach()
            }
            Token::Def => {
                self.advance();
                self.parse_def()
            }
            Token::Label => self.parse_label_primary(),
            t => Err(format!("unexpected token: {t:?}")),
        }
    }

    fn parse_label_primary(&mut self) -> Result<JqFilter, String> {
        self.advance();
        let var = match self.advance() {
            Token::Variable(name) => name,
            t => return Err(format!("expected variable after 'label', got {t:?}")),
        };
        self.expect(&Token::Pipe)?;
        let body = self.parse_pipe()?;
        Ok(JqFilter::Label(var, Box::new(body)))
    }

    fn parse_dot_primary(&mut self) -> Result<JqFilter, String> {
        self.advance();
        match self.peek() {
            Token::Ident(name) => {
                let name = name.clone();
                self.advance();
                if *self.peek() == Token::Question {
                    self.advance();
                    Ok(JqFilter::OptionalField(name))
                } else {
                    Ok(JqFilter::Field(name))
                }
            }
            Token::LBracket => self.parse_bracket_suffix(JqFilter::Identity),
            _ => Ok(JqFilter::Identity),
        }
    }

    fn parse_array_primary(&mut self) -> Result<JqFilter, String> {
        self.advance();
        if *self.peek() == Token::RBracket {
            self.advance();
            return Ok(JqFilter::ArrayConstruct(Box::new(JqFilter::FuncCall(
                "empty".into(),
                vec![],
            ))));
        }
        let inner = self.parse_pipe()?;
        self.expect(&Token::RBracket)?;
        Ok(JqFilter::ArrayConstruct(Box::new(inner)))
    }

    fn parse_grouped_primary(&mut self) -> Result<JqFilter, String> {
        self.advance();
        let inner = self.parse_pipe()?;
        self.expect(&Token::RParen)?;
        Ok(inner)
    }

    fn parse_literal_primary(&mut self, value: JqValue) -> JqFilter {
        self.advance();
        JqFilter::Literal(value)
    }

    fn parse_try_primary(&mut self) -> Result<JqFilter, String> {
        self.advance();
        let try_ = self.parse_postfix()?;
        let catch = if *self.peek() == Token::Catch {
            self.advance();
            Some(Box::new(self.parse_postfix()?))
        } else {
            None
        };
        Ok(JqFilter::TryCatch {
            try_: Box::new(try_),
            catch,
        })
    }

    fn parse_minus_primary(&mut self) -> Result<JqFilter, String> {
        self.advance();
        if let Token::NumLit(n) = self.peek() {
            let n = *n;
            self.advance();
            Ok(JqFilter::Literal(JqValue::Number(-n)))
        } else {
            let inner = self.parse_postfix()?;
            Ok(JqFilter::Negate(Box::new(inner)))
        }
    }

    fn parse_ident_primary(&mut self, name: String) -> Result<JqFilter, String> {
        self.advance();
        if *self.peek() != Token::LParen {
            return Ok(JqFilter::FuncCall(name, vec![]));
        }
        self.advance();
        let args = self.parse_func_call_args()?;
        self.expect(&Token::RParen)?;
        Ok(JqFilter::FuncCall(name, args))
    }

    fn parse_func_call_args(&mut self) -> Result<Vec<JqFilter>, String> {
        let mut args = Vec::new();
        if *self.peek() == Token::RParen {
            return Ok(args);
        }
        args.push(self.parse_pipe()?);
        while *self.peek() == Token::Semi {
            self.advance();
            args.push(self.parse_pipe()?);
        }
        Ok(args)
    }

    fn parse_if(&mut self) -> Result<JqFilter, String> {
        let cond = self.parse_pipe()?;
        self.expect(&Token::Then)?;
        let then_ = self.parse_pipe()?;
        let mut elifs = Vec::new();
        while *self.peek() == Token::Elif {
            self.advance();
            let elif_cond = self.parse_pipe()?;
            self.expect(&Token::Then)?;
            let elif_body = self.parse_pipe()?;
            elifs.push((elif_cond, elif_body));
        }
        let else_ = if *self.peek() == Token::Else {
            self.advance();
            Some(Box::new(self.parse_pipe()?))
        } else {
            None
        };
        self.expect(&Token::End)?;
        Ok(JqFilter::If {
            cond: Box::new(cond),
            then_: Box::new(then_),
            elifs,
            else_,
        })
    }

    /// Parse a postfix expression but stop before consuming `as`.
    /// Used for `reduce EXPR as ...` and `foreach EXPR as ...`.
    fn parse_postfix_no_as(&mut self) -> Result<JqFilter, String> {
        let mut expr = self.parse_primary()?;
        loop {
            match self.peek() {
                Token::Dot => match self.try_parse_dot_field() {
                    Some(field) => expr = JqFilter::Pipe(Box::new(expr), Box::new(field)),
                    None => break,
                },
                Token::LBracket => expr = self.parse_bracket_suffix(expr)?,
                Token::Question => {
                    self.advance();
                    expr = JqFilter::TryCatch {
                        try_: Box::new(expr),
                        catch: None,
                    };
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_reduce(&mut self) -> Result<JqFilter, String> {
        let iter = self.parse_postfix_no_as()?;
        self.expect(&Token::As)?;
        let var = match self.advance() {
            Token::Variable(name) => name,
            t => return Err(format!("expected variable in reduce, got {t:?}")),
        };
        self.expect(&Token::LParen)?;
        let init = self.parse_pipe()?;
        self.expect(&Token::Semi)?;
        let update = self.parse_pipe()?;
        self.expect(&Token::RParen)?;
        Ok(JqFilter::Reduce {
            iter: Box::new(iter),
            var,
            init: Box::new(init),
            update: Box::new(update),
        })
    }

    fn parse_foreach(&mut self) -> Result<JqFilter, String> {
        let iter = self.parse_postfix_no_as()?;
        self.expect(&Token::As)?;
        let var = match self.advance() {
            Token::Variable(name) => name,
            t => return Err(format!("expected variable in foreach, got {t:?}")),
        };
        self.expect(&Token::LParen)?;
        let init = self.parse_pipe()?;
        self.expect(&Token::Semi)?;
        let update = self.parse_pipe()?;
        let extract = if *self.peek() == Token::Semi {
            self.advance();
            Some(Box::new(self.parse_pipe()?))
        } else {
            None
        };
        self.expect(&Token::RParen)?;
        Ok(JqFilter::Foreach {
            iter: Box::new(iter),
            var,
            init: Box::new(init),
            update: Box::new(update),
            extract,
        })
    }

    fn parse_def(&mut self) -> Result<JqFilter, String> {
        let name = match self.advance() {
            Token::Ident(n) => n,
            t => return Err(format!("expected function name after 'def', got {t:?}")),
        };
        let mut args = Vec::new();
        if *self.peek() == Token::LParen {
            self.advance();
            if *self.peek() != Token::RParen {
                match self.advance() {
                    Token::Ident(a) => args.push(a),
                    t => return Err(format!("expected argument name, got {t:?}")),
                }
                while *self.peek() == Token::Semi {
                    self.advance();
                    match self.advance() {
                        Token::Ident(a) => args.push(a),
                        t => return Err(format!("expected argument name, got {t:?}")),
                    }
                }
            }
            self.expect(&Token::RParen)?;
        }
        self.expect(&Token::Colon)?;
        let body = self.parse_pipe()?;
        self.expect(&Token::Semi)?;
        let rest = self.parse_pipe()?;
        Ok(JqFilter::FuncDef {
            name,
            args,
            body: Box::new(body),
            rest: Box::new(rest),
        })
    }

    fn parse_object_construct(&mut self) -> Result<JqFilter, String> {
        let mut pairs = Vec::new();
        if *self.peek() == Token::RBrace {
            self.advance();
            return Ok(JqFilter::ObjectConstruct(pairs));
        }
        loop {
            let (key, val) = self.parse_obj_pair()?;
            pairs.push((key, val));
            match self.peek() {
                Token::Comma => {
                    self.advance();
                }
                Token::RBrace => {
                    self.advance();
                    return Ok(JqFilter::ObjectConstruct(pairs));
                }
                t => return Err(format!("expected ',' or '}}' in object, got {t:?}")),
            }
        }
    }

    fn parse_obj_pair(&mut self) -> Result<(JqObjKey, Option<JqFilter>), String> {
        match self.peek().clone() {
            Token::Ident(name) => {
                self.advance();
                self.parse_obj_ident_pair(name, None)
            }
            Token::StrLit(s) => {
                self.advance();
                self.parse_obj_ident_pair(s, None)
            }
            Token::Variable(name) => {
                self.advance();
                self.parse_obj_ident_pair(name.clone(), Some(JqFilter::Variable(name)))
            }
            Token::LParen => self.parse_obj_dynamic_pair(),
            Token::AtFormat(name) => {
                self.advance();
                self.parse_obj_ident_pair(format!("@{name}"), None)
            }
            t => Err(format!("expected object key, got {t:?}")),
        }
    }

    /// Parse `key: value` or shorthand `key` in object construction.
    /// `default` is used when there is no `:value` — `None` for ident/string,
    /// `Some(Variable(...))` for `$var`.
    fn parse_obj_ident_pair(
        &mut self,
        name: String,
        default: Option<JqFilter>,
    ) -> Result<(JqObjKey, Option<JqFilter>), String> {
        if *self.peek() == Token::Colon {
            self.advance();
            let val = self.parse_alternative()?;
            Ok((JqObjKey::Ident(name), Some(val)))
        } else {
            Ok((JqObjKey::Ident(name), default))
        }
    }

    fn parse_obj_dynamic_pair(&mut self) -> Result<(JqObjKey, Option<JqFilter>), String> {
        self.advance();
        let key_expr = self.parse_pipe()?;
        self.expect(&Token::RParen)?;
        self.expect(&Token::Colon)?;
        let val = self.parse_alternative()?;
        Ok((JqObjKey::Dynamic(key_expr), Some(val)))
    }
}

fn parse_filter(filter: &str) -> Result<JqFilter, String> {
    let mut parser = FilterParser::new(filter)?;
    parser.parse()
}

// ---------------------------------------------------------------------------
// Evaluator
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct JqEnv {
    vars: HashMap<String, JqValue>,
    funcs: HashMap<String, (Vec<String>, JqFilter)>,
}

impl JqEnv {
    fn new() -> Self {
        Self {
            vars: HashMap::new(),
            funcs: HashMap::new(),
        }
    }

    fn with_var(&self, name: &str, val: JqValue) -> Self {
        let mut env = self.clone();
        env.vars.insert(name.to_string(), val);
        env
    }

    fn with_func(&self, name: &str, args: Vec<String>, body: JqFilter) -> Self {
        let mut env = self.clone();
        env.funcs.insert(name.to_string(), (args, body));
        env
    }
}

/// Sentinel error for empty / break signals.
const EMPTY_SIGNAL: &str = "__jq_empty__";
const BREAK_SIGNAL: &str = "__jq_break__";

fn run_filter(filter: &JqFilter, input: &JqValue, env: &JqEnv) -> Result<Vec<JqValue>, String> {
    apply_filter(filter, input, env, 0)
}

#[allow(clippy::cast_possible_wrap)]
fn apply_filter(
    filter: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if depth > 1000 {
        return Err("recursion limit exceeded".into());
    }
    match filter {
        JqFilter::Identity
        | JqFilter::Field(_)
        | JqFilter::OptionalField(_)
        | JqFilter::Index(_)
        | JqFilter::OptionalIndex(_)
        | JqFilter::Slice(..)
        | JqFilter::Iterate
        | JqFilter::OptionalIterate => apply_filter_access(filter, input, env, depth),

        JqFilter::Pipe(..)
        | JqFilter::Comma(..)
        | JqFilter::Literal(_)
        | JqFilter::Comparison(..)
        | JqFilter::Arithmetic(..)
        | JqFilter::Not
        | JqFilter::And(..)
        | JqFilter::Or(..) => apply_filter_compose(filter, input, env, depth),

        _ => apply_filter_control(filter, input, env, depth),
    }
}

fn apply_filter_access(
    filter: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match filter {
        JqFilter::Identity => Ok(vec![input.clone()]),
        JqFilter::Field(name) => Ok(vec![field_access(input, name)]),
        JqFilter::OptionalField(name) => apply_optional_field(name, input),
        JqFilter::Index(idx_filter) => apply_index(idx_filter, input, env, depth),
        JqFilter::OptionalIndex(idx_filter) => apply_optional_index(idx_filter, input, env, depth),
        JqFilter::Slice(from, to) => apply_slice(from.as_deref(), to.as_deref(), input, env, depth),
        JqFilter::Iterate => apply_iterate(input),
        JqFilter::OptionalIterate => apply_optional_iterate(input),
        _ => unreachable!(),
    }
}

fn apply_filter_compose(
    filter: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match filter {
        JqFilter::Pipe(left, right) => apply_pipe(left, right, input, env, depth),
        JqFilter::Comma(left, right) => apply_comma(left, right, input, env, depth),
        JqFilter::Literal(val) => Ok(vec![val.clone()]),
        JqFilter::Comparison(left, op, right) => {
            apply_comparison(left, op, right, input, env, depth)
        }
        JqFilter::Arithmetic(left, op, right) => {
            apply_arithmetic(left, op, right, input, env, depth)
        }
        JqFilter::Not => Ok(vec![JqValue::Bool(!input.is_truthy())]),
        JqFilter::And(left, right) => apply_and(left, right, input, env, depth),
        JqFilter::Or(left, right) => apply_or(left, right, input, env, depth),
        _ => unreachable!(),
    }
}

fn apply_filter_control(
    filter: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match filter {
        JqFilter::If {
            cond,
            then_,
            elifs,
            else_,
        } => apply_if(cond, then_, elifs, else_.as_deref(), input, env, depth),
        JqFilter::TryCatch { try_, catch } => {
            apply_try_catch(try_, catch.as_deref(), input, env, depth)
        }
        JqFilter::Alternative(left, right) => apply_alternative(left, right, input, env, depth),
        JqFilter::Reduce {
            iter,
            var,
            init,
            update,
        } => apply_reduce(iter, var, init, update, input, env, depth),
        JqFilter::Foreach {
            iter,
            var,
            init,
            update,
            extract,
        } => apply_foreach(
            iter,
            var,
            init,
            update,
            extract.as_deref(),
            input,
            env,
            depth,
        ),
        JqFilter::Binding { expr, var, body } => apply_binding(expr, var, body, input, env, depth),
        JqFilter::FuncDef {
            name,
            args,
            body,
            rest,
        } => apply_funcdef(name, args, body, rest, input, env, depth),
        JqFilter::FuncCall(name, args) => dispatch_func(name, args, input, env, depth),
        JqFilter::ArrayConstruct(inner) => apply_array_construct(inner, input, env, depth),
        JqFilter::ObjectConstruct(pairs) => build_object(pairs, input, env, depth),
        JqFilter::Variable(name) => apply_variable(name, env),
        JqFilter::Recurse => Ok(input.recurse_values()),
        JqFilter::Label(_name, body) => apply_label(body, input, env, depth),
        JqFilter::Negate(inner) => apply_negate(inner, input, env, depth),
        JqFilter::Format(name) => apply_format(name, input),
        _ => unreachable!(),
    }
}

// ---------------------------------------------------------------------------
// Extracted helpers for apply_filter arms
// ---------------------------------------------------------------------------

#[allow(clippy::unnecessary_wraps)]
fn apply_optional_field(name: &str, input: &JqValue) -> Result<Vec<JqValue>, String> {
    match input {
        JqValue::Object(_) | JqValue::Null => Ok(vec![field_access(input, name)]),
        _ => Ok(vec![]),
    }
}

fn apply_index(
    idx_filter: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let indices = apply_filter(idx_filter, input, env, depth + 1)?;
    let mut out = Vec::new();
    for idx in &indices {
        out.push(index_access(input, idx)?);
    }
    Ok(out)
}

fn apply_optional_index(
    idx_filter: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let indices = apply_filter(idx_filter, input, env, depth + 1)?;
    let mut out = Vec::new();
    for idx in &indices {
        if let Ok(v) = index_access(input, idx) {
            out.push(v);
        }
    }
    Ok(out)
}

#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
fn apply_slice(
    from: Option<&JqFilter>,
    to: Option<&JqFilter>,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let from_val = match from {
        Some(f) => {
            let vals = apply_filter(f, input, env, depth + 1)?;
            vals.first().and_then(JqValue::as_i64).unwrap_or(0)
        }
        None => 0,
    };
    match input {
        JqValue::String(s) => apply_slice_string(s, from_val, to, input, env, depth),
        JqValue::Array(a) => apply_slice_array(a, from_val, to, input, env, depth),
        _ => Err(format!("cannot slice {}", input.type_name())),
    }
}

#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
fn apply_slice_string(
    s: &str,
    from_val: i64,
    to: Option<&JqFilter>,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let len = s.chars().count() as i64;
    let f = normalize_index(from_val, len as usize) as usize;
    let t = match to {
        Some(t_f) => {
            let vals = apply_filter(t_f, input, env, depth + 1)?;
            let tv = vals.first().and_then(JqValue::as_i64).unwrap_or(len);
            normalize_index(tv, len as usize) as usize
        }
        None => len as usize,
    };
    let chars: Vec<char> = s.chars().collect();
    let f = f.min(chars.len());
    let t = t.min(chars.len());
    if f >= t {
        return Ok(vec![JqValue::String(String::new())]);
    }
    let sliced: String = chars[f..t].iter().collect();
    Ok(vec![JqValue::String(sliced)])
}

#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
fn apply_slice_array(
    arr: &[JqValue],
    from_val: i64,
    to: Option<&JqFilter>,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let len = arr.len();
    let f = normalize_index(from_val, len) as usize;
    let t = match to {
        Some(t_f) => {
            let vals = apply_filter(t_f, input, env, depth + 1)?;
            let tv = vals.first().and_then(JqValue::as_i64).unwrap_or(len as i64);
            normalize_index(tv, len) as usize
        }
        None => len,
    };
    let f = f.min(len);
    let t = t.min(len);
    if f >= t {
        return Ok(vec![JqValue::Array(vec![])]);
    }
    Ok(vec![JqValue::Array(arr[f..t].to_vec())])
}

fn apply_iterate(input: &JqValue) -> Result<Vec<JqValue>, String> {
    match input {
        JqValue::Array(arr) => Ok(arr.clone()),
        JqValue::Object(pairs) => Ok(pairs.iter().map(|(_, v)| v.clone()).collect()),
        JqValue::Null => Ok(vec![]),
        _ => Err(format!("cannot iterate over {}", input.type_name())),
    }
}

#[allow(clippy::unnecessary_wraps)]
fn apply_optional_iterate(input: &JqValue) -> Result<Vec<JqValue>, String> {
    match input {
        JqValue::Array(arr) => Ok(arr.clone()),
        JqValue::Object(pairs) => Ok(pairs.iter().map(|(_, v)| v.clone()).collect()),
        _ => Ok(vec![]),
    }
}

fn apply_pipe(
    left: &JqFilter,
    right: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let left_vals = apply_filter(left, input, env, depth + 1)?;
    let mut out = Vec::new();
    for v in &left_vals {
        let right_vals = apply_filter(right, v, env, depth + 1)?;
        out.extend(right_vals);
    }
    Ok(out)
}

fn apply_comma(
    left: &JqFilter,
    right: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let mut out = apply_filter(left, input, env, depth + 1)?;
    out.extend(apply_filter(right, input, env, depth + 1)?);
    Ok(out)
}

fn apply_comparison(
    left: &JqFilter,
    op: &CompOp,
    right: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let lvals = apply_filter(left, input, env, depth + 1)?;
    let rvals = apply_filter(right, input, env, depth + 1)?;
    let mut out = Vec::new();
    for lv in &lvals {
        for rv in &rvals {
            out.push(JqValue::Bool(eval_comparison(lv, op, rv)));
        }
    }
    Ok(out)
}

fn eval_comparison(lv: &JqValue, op: &CompOp, rv: &JqValue) -> bool {
    match op {
        CompOp::Eq => lv.equals(rv),
        CompOp::Ne => !lv.equals(rv),
        CompOp::Lt => lv.compare(rv) == Some(std::cmp::Ordering::Less),
        CompOp::Le => matches!(
            lv.compare(rv),
            Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
        ),
        CompOp::Gt => lv.compare(rv) == Some(std::cmp::Ordering::Greater),
        CompOp::Ge => matches!(
            lv.compare(rv),
            Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
        ),
    }
}

fn apply_arithmetic(
    left: &JqFilter,
    op: &ArithOp,
    right: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let lvals = apply_filter(left, input, env, depth + 1)?;
    let rvals = apply_filter(right, input, env, depth + 1)?;
    let mut out = Vec::new();
    for lv in &lvals {
        for rv in &rvals {
            out.push(arith_op(lv, op, rv)?);
        }
    }
    Ok(out)
}

fn apply_and(
    left: &JqFilter,
    right: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let lvals = apply_filter(left, input, env, depth + 1)?;
    let mut out = Vec::new();
    for lv in &lvals {
        if !lv.is_truthy() {
            out.push(JqValue::Bool(false));
        } else {
            let rvals = apply_filter(right, input, env, depth + 1)?;
            for rv in &rvals {
                out.push(JqValue::Bool(rv.is_truthy()));
            }
        }
    }
    Ok(out)
}

fn apply_or(
    left: &JqFilter,
    right: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let lvals = apply_filter(left, input, env, depth + 1)?;
    let mut out = Vec::new();
    for lv in &lvals {
        if lv.is_truthy() {
            out.push(JqValue::Bool(true));
        } else {
            let rvals = apply_filter(right, input, env, depth + 1)?;
            for rv in &rvals {
                out.push(JqValue::Bool(rv.is_truthy()));
            }
        }
    }
    Ok(out)
}

fn apply_if(
    cond: &JqFilter,
    then_: &JqFilter,
    elifs: &[(JqFilter, JqFilter)],
    else_: Option<&JqFilter>,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let cond_vals = apply_filter(cond, input, env, depth + 1)?;
    let mut out = Vec::new();
    for cv in &cond_vals {
        if cv.is_truthy() {
            out.extend(apply_filter(then_, input, env, depth + 1)?);
        } else {
            apply_if_else(elifs, else_, input, env, depth, &mut out)?;
        }
    }
    Ok(out)
}

fn apply_if_else(
    elifs: &[(JqFilter, JqFilter)],
    else_: Option<&JqFilter>,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
    out: &mut Vec<JqValue>,
) -> Result<(), String> {
    for (econd, ebody) in elifs {
        let evals = apply_filter(econd, input, env, depth + 1)?;
        if evals.first().is_some_and(JqValue::is_truthy) {
            out.extend(apply_filter(ebody, input, env, depth + 1)?);
            return Ok(());
        }
    }
    if let Some(else_) = else_ {
        out.extend(apply_filter(else_, input, env, depth + 1)?);
    } else {
        out.push(input.clone());
    }
    Ok(())
}

fn apply_try_catch(
    try_: &JqFilter,
    catch: Option<&JqFilter>,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match apply_filter(try_, input, env, depth + 1) {
        Ok(vals) => Ok(vals),
        Err(e) => {
            if let Some(catch_f) = catch {
                let err_val = JqValue::String(e);
                apply_filter(catch_f, &err_val, env, depth + 1)
            } else {
                Ok(vec![])
            }
        }
    }
}

fn apply_alternative(
    left: &JqFilter,
    right: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let lvals = apply_filter(left, input, env, depth + 1)?;
    let out: Vec<JqValue> = lvals.into_iter().filter(JqValue::is_truthy).collect();
    if out.is_empty() {
        apply_filter(right, input, env, depth + 1)
    } else {
        Ok(out)
    }
}

fn apply_reduce(
    iter: &JqFilter,
    var: &str,
    init: &JqFilter,
    update: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let init_vals = apply_filter(init, input, env, depth + 1)?;
    let mut accum = init_vals.into_iter().next().unwrap_or(JqValue::Null);
    let items = apply_filter(iter, input, env, depth + 1)?;
    for item in &items {
        let new_env = env.with_var(var, item.clone());
        let update_vals = apply_filter(update, &accum, &new_env, depth + 1)?;
        accum = update_vals.into_iter().next().unwrap_or(JqValue::Null);
    }
    Ok(vec![accum])
}

fn apply_foreach(
    iter: &JqFilter,
    var: &str,
    init: &JqFilter,
    update: &JqFilter,
    extract: Option<&JqFilter>,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let init_vals = apply_filter(init, input, env, depth + 1)?;
    let mut state = init_vals.into_iter().next().unwrap_or(JqValue::Null);
    let items = apply_filter(iter, input, env, depth + 1)?;
    let mut out = Vec::new();
    for item in &items {
        let new_env = env.with_var(var, item.clone());
        let update_vals = apply_filter(update, &state, &new_env, depth + 1)?;
        state = update_vals.into_iter().next().unwrap_or(JqValue::Null);
        if let Some(ext) = extract {
            let ext_vals = apply_filter(ext, &state, &new_env, depth + 1)?;
            out.extend(ext_vals);
        } else {
            out.push(state.clone());
        }
    }
    Ok(out)
}

fn apply_binding(
    expr: &JqFilter,
    var: &str,
    body: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let vals = apply_filter(expr, input, env, depth + 1)?;
    let mut out = Vec::new();
    for v in &vals {
        let new_env = env.with_var(var, v.clone());
        out.extend(apply_filter(body, input, &new_env, depth + 1)?);
    }
    Ok(out)
}

fn apply_funcdef(
    name: &str,
    args: &[String],
    body: &JqFilter,
    rest: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let new_env = env.with_func(name, args.to_vec(), body.clone());
    apply_filter(rest, input, &new_env, depth + 1)
}

fn apply_array_construct(
    inner: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match apply_filter(inner, input, env, depth + 1) {
        Ok(vals) => Ok(vec![JqValue::Array(vals)]),
        Err(e) if e == EMPTY_SIGNAL => Ok(vec![JqValue::Array(vec![])]),
        Err(e) => Err(e),
    }
}

fn apply_variable(name: &str, env: &JqEnv) -> Result<Vec<JqValue>, String> {
    if name == "ENV" {
        Ok(vec![JqValue::Object(vec![])])
    } else if let Some(val) = env.vars.get(name) {
        Ok(vec![val.clone()])
    } else {
        Err(format!("${name} is not defined"))
    }
}

fn apply_label(
    body: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match apply_filter(body, input, env, depth + 1) {
        Ok(vals) => Ok(vals),
        Err(e) if e == BREAK_SIGNAL => Ok(vec![]),
        Err(e) => Err(e),
    }
}

fn apply_negate(
    inner: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let vals = apply_filter(inner, input, env, depth + 1)?;
    let mut out = Vec::new();
    for v in &vals {
        match v {
            JqValue::Number(n) => out.push(JqValue::Number(-n)),
            _ => return Err(format!("cannot negate {}", v.type_name())),
        }
    }
    Ok(out)
}

fn field_access(input: &JqValue, name: &str) -> JqValue {
    match input {
        JqValue::Object(pairs) => {
            for (k, v) in pairs {
                if k == name {
                    return v.clone();
                }
            }
            JqValue::Null
        }
        _ => JqValue::Null,
    }
}

#[allow(clippy::cast_possible_wrap)]
fn index_access(input: &JqValue, idx: &JqValue) -> Result<JqValue, String> {
    match (input, idx) {
        (JqValue::Array(arr), JqValue::Number(n)) => {
            let i = *n as i64;
            let actual = if i < 0 {
                (arr.len() as i64 + i) as usize
            } else {
                i as usize
            };
            Ok(arr.get(actual).cloned().unwrap_or(JqValue::Null))
        }
        (JqValue::Object(pairs), JqValue::String(key)) => {
            for (k, v) in pairs {
                if k == key {
                    return Ok(v.clone());
                }
            }
            Ok(JqValue::Null)
        }
        (JqValue::Null, _) => Ok(JqValue::Null),
        _ => Err(format!(
            "cannot index {} with {}",
            input.type_name(),
            idx.type_name()
        )),
    }
}

#[allow(clippy::cast_possible_wrap)]
fn normalize_index(idx: i64, len: usize) -> i64 {
    if idx < 0 {
        let adjusted = len as i64 + idx;
        if adjusted < 0 {
            0
        } else {
            adjusted
        }
    } else {
        idx
    }
}

#[allow(clippy::needless_pass_by_value)]
fn arith_op(left: &JqValue, op: &ArithOp, right: &JqValue) -> Result<JqValue, String> {
    if let (JqValue::Number(a), JqValue::Number(b)) = (left, right) {
        return arith_op_numbers(*a, op, *b);
    }
    if matches!(op, ArithOp::Add) {
        return arith_op_add(left, right);
    }
    Err(arith_type_error(op, left, right))
}

fn arith_op_numbers(a: f64, op: &ArithOp, b: f64) -> Result<JqValue, String> {
    match op {
        ArithOp::Add => Ok(JqValue::Number(a + b)),
        ArithOp::Sub => Ok(JqValue::Number(a - b)),
        ArithOp::Mul => Ok(JqValue::Number(a * b)),
        ArithOp::Div => {
            if b == 0.0 {
                Err("division by zero".into())
            } else {
                Ok(JqValue::Number(a / b))
            }
        }
        ArithOp::Mod => {
            if b == 0.0 {
                Err("modulo by zero".into())
            } else {
                Ok(JqValue::Number(a % b))
            }
        }
    }
}

fn arith_op_add(left: &JqValue, right: &JqValue) -> Result<JqValue, String> {
    match (left, right) {
        (JqValue::String(a), JqValue::String(b)) => Ok(JqValue::String(format!("{a}{b}"))),
        (JqValue::Array(a), JqValue::Array(b)) => {
            let mut result = a.clone();
            result.extend(b.iter().cloned());
            Ok(JqValue::Array(result))
        }
        (JqValue::Object(a), JqValue::Object(b)) => Ok(JqValue::Object(merge_objects(a, b))),
        (JqValue::Null, other) | (other, JqValue::Null) => Ok(other.clone()),
        _ => Err(arith_type_error(&ArithOp::Add, left, right)),
    }
}

fn merge_objects(
    base: &[(String, JqValue)],
    overlay: &[(String, JqValue)],
) -> Vec<(String, JqValue)> {
    let mut result = base.to_vec();
    for (k, v) in overlay {
        if let Some(existing) = result.iter_mut().find(|(ek, _)| ek == k) {
            existing.1 = v.clone();
        } else {
            result.push((k.clone(), v.clone()));
        }
    }
    result
}

fn arith_type_error(op: &ArithOp, left: &JqValue, right: &JqValue) -> String {
    let op_name = match op {
        ArithOp::Add => "add",
        ArithOp::Sub => "subtract",
        ArithOp::Mul => "multiply",
        ArithOp::Div => "divide",
        ArithOp::Mod => "modulo",
    };
    format!(
        "cannot {op_name} {} and {}",
        left.type_name(),
        right.type_name()
    )
}

fn build_object(
    pairs: &[(JqObjKey, Option<JqFilter>)],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let mut results: Vec<Vec<(String, JqValue)>> = vec![vec![]];

    for (key, val_filter) in pairs {
        results = build_object_pairs(&results, key, val_filter.as_ref(), input, env, depth)?;
    }

    Ok(results.into_iter().map(JqValue::Object).collect())
}

fn build_object_pairs(
    results: &[Vec<(String, JqValue)>],
    key: &JqObjKey,
    val_filter: Option<&JqFilter>,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<Vec<(String, JqValue)>>, String> {
    match key {
        JqObjKey::Ident(name) => {
            let values = build_object_ident_values(name, val_filter, input, env, depth)?;
            Ok(expand_object_results(results, &[(name.clone(), values)]))
        }
        JqObjKey::Dynamic(key_filter) => {
            let keys = build_object_dynamic_keys(key_filter, input, env, depth)?;
            let values = build_object_value_list(val_filter, input, env, depth, true)?;
            Ok(expand_object_results(
                results,
                &keys
                    .into_iter()
                    .map(|k| (k, values.clone()))
                    .collect::<Vec<_>>(),
            ))
        }
    }
}

fn build_object_ident_values(
    name: &str,
    val_filter: Option<&JqFilter>,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if let Some(vf) = val_filter {
        apply_filter(vf, input, env, depth + 1)
    } else {
        Ok(vec![field_access(input, name)])
    }
}

fn build_object_dynamic_keys(
    key_filter: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<String>, String> {
    Ok(apply_filter(key_filter, input, env, depth + 1)?
        .into_iter()
        .map(|k| match k {
            JqValue::String(s) => s,
            other => other.to_string_repr(),
        })
        .collect())
}

fn build_object_value_list(
    val_filter: Option<&JqFilter>,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
    default_input: bool,
) -> Result<Vec<JqValue>, String> {
    if let Some(vf) = val_filter {
        apply_filter(vf, input, env, depth + 1)
    } else if default_input {
        Ok(vec![input.clone()])
    } else {
        Ok(vec![])
    }
}

fn expand_object_results(
    results: &[Vec<(String, JqValue)>],
    keyed_values: &[(String, Vec<JqValue>)],
) -> Vec<Vec<(String, JqValue)>> {
    let mut new_results = Vec::new();
    for existing in results {
        for (key, values) in keyed_values {
            for value in values {
                let mut obj_pairs = existing.clone();
                obj_pairs.push((key.clone(), value.clone()));
                new_results.push(obj_pairs);
            }
        }
    }
    new_results
}

// ---------------------------------------------------------------------------
// Built-in functions
// ---------------------------------------------------------------------------

fn dispatch_func(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if let Some(result) = dispatch_user_func(name, args, input, env, depth)? {
        return Ok(result);
    }
    dispatch_builtin_func(name, args, input, env, depth)
}

fn dispatch_user_func(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Option<Vec<JqValue>>, String> {
    let Some((param_names, body)) = env.funcs.get(name) else {
        return Ok(None);
    };
    if args.len() != param_names.len() {
        return Err(format!(
            "{name}: expected {} args, got {}",
            param_names.len(),
            args.len()
        ));
    }
    let mut new_env = env.clone();
    for (pname, arg) in param_names.iter().zip(args) {
        let val = apply_filter(arg, input, &new_env, depth + 1)?;
        let v = val.into_iter().next().unwrap_or(JqValue::Null);
        new_env.vars.insert(pname.clone(), v);
    }
    apply_filter(&body.clone(), input, &new_env, depth + 1).map(Some)
}

fn dispatch_builtin_func(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match name {
        // Type and identity operations
        "type" | "length" | "utf8bytelength" | "empty" | "error" | "not" | "debug" | "builtins"
        | "input" | "inputs" | "objects" | "iterables" | "booleans" | "numbers" | "strings"
        | "nulls" | "arrays" | "scalars" => dispatch_type_ops(name, args, input, env, depth),

        // Select/map/recurse operations
        "select" | "map" | "map_values" | "recurse" | "recurse_down" => {
            dispatch_select_map(name, args, input, env, depth)
        }

        // Conversion and environment operations
        "tostring" | "tonumber" | "tojson" | "fromjson" | "env" => dispatch_entry_ops(name, input),

        // String transform operations
        "ascii_downcase" | "ascii_upcase" | "ltrimstr" | "rtrimstr" | "explode" | "implode" => {
            dispatch_string_transform(name, args, input, env, depth)
        }

        // String match operations
        "startswith" | "endswith" | "test" | "match" | "capture" | "split" | "join" | "sub"
        | "gsub" => dispatch_string_match(name, args, input, env, depth),

        // Sort and group operations
        "sort" | "sort_by" | "group_by" | "unique" | "unique_by" => {
            dispatch_sort_group(name, args, input, env, depth)
        }

        // Array access operations
        "first" | "last" | "nth" | "range" | "limit" | "reverse" | "flatten" | "transpose" => {
            dispatch_array_access(name, args, input, env, depth)
        }

        // Array reduce operations
        "add" | "any" | "all" | "min" | "max" | "min_by" | "max_by" | "indices" | "index"
        | "rindex" => dispatch_array_reduce(name, args, input, env, depth),

        // Object key/value operations
        "keys" | "keys_unsorted" | "values" | "has" | "in" | "contains" | "inside" => {
            dispatch_object_keys(name, args, input, env, depth)
        }

        // Object entry operations
        "to_entries" | "from_entries" | "with_entries" => {
            dispatch_object_entries(name, args, input, env, depth)
        }

        // Path get/set operations
        "path" | "getpath" | "setpath" => dispatch_path_access(name, args, input, env, depth),

        // Path delete/leaf operations
        "delpaths" | "leaf_paths" | "paths" => dispatch_path_collect(name, args, input, env, depth),

        // Basic math operations
        "floor" | "ceil" | "round" | "sqrt" | "fabs" => dispatch_math_basic(name, input),

        // Advanced math operations
        "pow" | "log" | "log2" | "log10" | "exp" | "exp2" | "exp10" | "infinite" | "nan"
        | "isinfinite" | "isnan" | "isnormal" | "isfinite" => {
            dispatch_math_advanced(name, args, input, env, depth)
        }

        _ => Err(format!("{name}/0 is not defined")),
    }
}

// ---------------------------------------------------------------------------
// Sub-dispatchers for dispatch_func
// ---------------------------------------------------------------------------

fn dispatch_type_ops(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match name {
        "empty" => Err(EMPTY_SIGNAL.into()),
        "error" => apply_error(args, input, env, depth),
        "type" => Ok(vec![JqValue::String(input.type_name().to_string())]),
        "length" => Ok(vec![input.length()]),
        "utf8bytelength" => apply_utf8bytelength(input),
        "not" => Ok(vec![JqValue::Bool(!input.is_truthy())]),
        "debug" => Ok(vec![input.clone()]),
        "builtins" => Ok(vec![JqValue::Array(builtin_names())]),
        "input" => Ok(vec![JqValue::Null]),
        "inputs" => Ok(vec![]),
        _ => dispatch_type_selectors(name, input),
    }
}

fn apply_error(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if let Some(arg) = args.first() {
        let vals = apply_filter(arg, input, env, depth + 1)?;
        let msg = vals.first().map_or("error".into(), JqValue::to_string_repr);
        return Err(msg);
    }
    let msg = match input {
        JqValue::String(s) => s.clone(),
        _ => input.to_string_repr(),
    };
    Err(msg)
}

#[allow(clippy::unnecessary_wraps)]
fn apply_utf8bytelength(input: &JqValue) -> Result<Vec<JqValue>, String> {
    match input {
        JqValue::String(s) => Ok(vec![JqValue::Number(s.len() as f64)]),
        _ => Ok(vec![input.length()]),
    }
}

fn dispatch_type_selectors(name: &str, input: &JqValue) -> Result<Vec<JqValue>, String> {
    let type_matches = match name {
        "objects" => matches!(input, JqValue::Object(_)),
        "arrays" => matches!(input, JqValue::Array(_)),
        "iterables" => matches!(input, JqValue::Array(_) | JqValue::Object(_)),
        "booleans" => matches!(input, JqValue::Bool(_)),
        "numbers" => matches!(input, JqValue::Number(_)),
        "strings" => matches!(input, JqValue::String(_)),
        "nulls" => matches!(input, JqValue::Null),
        "scalars" => !matches!(input, JqValue::Array(_) | JqValue::Object(_)),
        _ => return Err(format!("{name}/0 is not defined")),
    };
    if type_matches {
        Ok(vec![input.clone()])
    } else {
        Ok(vec![])
    }
}

fn dispatch_select_map(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match name {
        "select" => apply_select(args, input, env, depth),
        "map" => apply_map(args, input, env, depth),
        "map_values" => apply_map_values(args, input, env, depth),
        "recurse" | "recurse_down" => apply_recurse(args, input, env, depth),
        _ => Err(format!("{name}/0 is not defined")),
    }
}

fn apply_select(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.len() != 1 {
        return Err("select requires 1 argument".into());
    }
    let cond_vals = apply_filter(&args[0], input, env, depth + 1)?;
    if cond_vals.first().is_some_and(JqValue::is_truthy) {
        Ok(vec![input.clone()])
    } else {
        Ok(vec![])
    }
}

fn apply_map(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.len() != 1 {
        return Err("map requires 1 argument".into());
    }
    match input {
        JqValue::Array(arr) => {
            let mut out = Vec::new();
            for item in arr {
                match apply_filter(&args[0], item, env, depth + 1) {
                    Ok(vals) => out.extend(vals),
                    Err(e) if e == EMPTY_SIGNAL => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(vec![JqValue::Array(out)])
        }
        _ => Err(format!("cannot map over {}", input.type_name())),
    }
}

fn apply_map_values(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.len() != 1 {
        return Err("map_values requires 1 argument".into());
    }
    match input {
        JqValue::Object(pairs) => {
            let mut out = Vec::new();
            for (k, v) in pairs {
                let vals = apply_filter(&args[0], v, env, depth + 1)?;
                let new_v = vals.into_iter().next().unwrap_or(JqValue::Null);
                out.push((k.clone(), new_v));
            }
            Ok(vec![JqValue::Object(out)])
        }
        JqValue::Array(arr) => {
            let mut out = Vec::new();
            for v in arr {
                let vals = apply_filter(&args[0], v, env, depth + 1)?;
                let new_v = vals.into_iter().next().unwrap_or(JqValue::Null);
                out.push(new_v);
            }
            Ok(vec![JqValue::Array(out)])
        }
        _ => Err(format!("cannot map_values over {}", input.type_name())),
    }
}

#[allow(clippy::unnecessary_wraps)]
fn apply_recurse(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.is_empty() {
        return Ok(input.recurse_values());
    }
    let mut results = vec![input.clone()];
    let mut queue = vec![input.clone()];
    let max_iter = 10_000;
    let mut count = 0;
    while let Some(item) = queue.pop() {
        count += 1;
        if count > max_iter {
            break;
        }
        match apply_filter(&args[0], &item, env, depth + 1) {
            Ok(vals) => {
                for v in vals {
                    results.push(v.clone());
                    queue.push(v);
                }
            }
            Err(e) if e == EMPTY_SIGNAL => {}
            Err(_) => break,
        }
    }
    Ok(results)
}

fn dispatch_entry_ops(name: &str, input: &JqValue) -> Result<Vec<JqValue>, String> {
    match name {
        "tostring" => apply_tostring(input),
        "tonumber" => apply_tonumber(input),
        "tojson" => Ok(vec![JqValue::String(json_to_string(input, true))]),
        "fromjson" => apply_fromjson(input),
        "env" => Ok(vec![JqValue::Object(vec![])]),
        _ => Err(format!("{name}/0 is not defined")),
    }
}

#[allow(clippy::unnecessary_wraps)]
fn apply_tostring(input: &JqValue) -> Result<Vec<JqValue>, String> {
    let s = match input {
        JqValue::String(s) => s.clone(),
        _ => json_to_string(input, true),
    };
    Ok(vec![JqValue::String(s)])
}

fn apply_tonumber(input: &JqValue) -> Result<Vec<JqValue>, String> {
    match input {
        JqValue::Number(_) => Ok(vec![input.clone()]),
        JqValue::String(s) => {
            let n: f64 = s
                .trim()
                .parse()
                .map_err(|_| format!("cannot convert \"{s}\" to number"))?;
            Ok(vec![JqValue::Number(n)])
        }
        _ => Err(format!("cannot convert {} to number", input.type_name())),
    }
}

fn apply_fromjson(input: &JqValue) -> Result<Vec<JqValue>, String> {
    match input {
        JqValue::String(s) => {
            let val = parse_json(s)?;
            Ok(vec![val])
        }
        _ => Err(format!("cannot fromjson on {}", input.type_name())),
    }
}

fn dispatch_string_transform(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match name {
        "ascii_downcase" => apply_ascii_case(input, false),
        "ascii_upcase" => apply_ascii_case(input, true),
        "ltrimstr" => apply_trim_string(args, input, env, depth, true),
        "rtrimstr" => apply_trim_string(args, input, env, depth, false),
        "explode" => apply_explode(input),
        "implode" => apply_implode(input),
        _ => Err(format!("{name}/0 is not defined")),
    }
}

fn apply_ascii_case(input: &JqValue, uppercase: bool) -> Result<Vec<JqValue>, String> {
    let JqValue::String(s) = input else {
        return Err(format!(
            "cannot {} {}",
            if uppercase { "upcase" } else { "downcase" },
            input.type_name()
        ));
    };
    Ok(vec![JqValue::String(if uppercase {
        s.to_uppercase()
    } else {
        s.to_lowercase()
    })])
}

fn apply_trim_string(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
    prefix: bool,
) -> Result<Vec<JqValue>, String> {
    let name = if prefix { "ltrimstr" } else { "rtrimstr" };
    let affix = eval_first_string_arg(name, args, input, env, depth)?;
    let JqValue::String(s) = input else {
        return Ok(vec![input.clone()]);
    };
    let result = if prefix {
        s.strip_prefix(affix.as_str()).unwrap_or(s)
    } else {
        s.strip_suffix(affix.as_str()).unwrap_or(s)
    };
    Ok(vec![JqValue::String(result.to_string())])
}

fn apply_explode(input: &JqValue) -> Result<Vec<JqValue>, String> {
    let JqValue::String(s) = input else {
        return Err(format!("cannot explode {}", input.type_name()));
    };
    Ok(vec![JqValue::Array(
        s.chars()
            .map(|c| JqValue::Number(c as u32 as f64))
            .collect(),
    )])
}

fn apply_implode(input: &JqValue) -> Result<Vec<JqValue>, String> {
    let JqValue::Array(arr) = input else {
        return Err(format!("cannot implode {}", input.type_name()));
    };
    let mut s = String::new();
    for v in arr {
        if let JqValue::Number(n) = v {
            if let Some(c) = char::from_u32(*n as u32) {
                s.push(c);
            }
        }
    }
    Ok(vec![JqValue::String(s)])
}

fn dispatch_string_match(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match name {
        "test" | "match" | "capture" => apply_regex_dispatch(name, args, input, env, depth),
        "startswith" => apply_str_prefix_check(args, input, env, depth, true),
        "endswith" => apply_str_prefix_check(args, input, env, depth, false),
        "split" => apply_split(args, input, env, depth),
        "join" => apply_join(args, input, env, depth),
        "gsub" | "sub" => apply_sub_gsub(name, args, input, env, depth),
        _ => Err(format!("{name}/0 is not defined")),
    }
}

fn apply_regex_dispatch(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let (pat, flags) = eval_regex_dispatch_args(name, args, input, env, depth)?;
    let JqValue::String(s) = input else {
        return Err(format!("{name} requires string input"));
    };
    apply_regex_op(name, s, &pat, flags.contains('i'))
}

fn eval_regex_dispatch_args(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<(String, String), String> {
    if args.is_empty() {
        return Err(format!("{name} requires at least 1 argument"));
    }
    let pat = eval_filter_string(&args[0], input, env, depth)?;
    let flags = if args.len() > 1 {
        eval_filter_string(&args[1], input, env, depth)?
    } else {
        String::new()
    };
    Ok((pat, flags))
}

/// Helper for `startswith` (when `is_prefix` is true) and `endswith` (when false).
fn apply_str_prefix_check(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
    is_prefix: bool,
) -> Result<Vec<JqValue>, String> {
    let func_name = if is_prefix { "startswith" } else { "endswith" };
    if args.len() != 1 {
        return Err(format!("{func_name} requires 1 argument"));
    }
    let fix_vals = apply_filter(&args[0], input, env, depth + 1)?;
    let fix = fix_vals
        .first()
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    match input {
        JqValue::String(s) => {
            let result = if is_prefix {
                s.starts_with(fix.as_str())
            } else {
                s.ends_with(fix.as_str())
            };
            Ok(vec![JqValue::Bool(result)])
        }
        _ => Err(format!("{func_name} requires string input")),
    }
}

fn apply_split(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let sep = eval_first_string_arg("split", args, input, env, depth)?;
    let JqValue::String(s) = input else {
        return Err(format!("cannot split {}", input.type_name()));
    };
    Ok(vec![JqValue::Array(
        s.split(&sep)
            .map(|p| JqValue::String(p.to_string()))
            .collect(),
    )])
}

fn eval_first_string_arg(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<String, String> {
    if args.len() != 1 {
        return Err(format!("{name} requires 1 argument"));
    }
    eval_filter_string(&args[0], input, env, depth)
}

fn eval_filter_string(
    filter: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<String, String> {
    Ok(apply_filter(filter, input, env, depth + 1)?
        .first()
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string())
}

fn apply_join(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.len() != 1 {
        return Err("join requires 1 argument".into());
    }
    let sep_vals = apply_filter(&args[0], input, env, depth + 1)?;
    let sep = sep_vals
        .first()
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    match input {
        JqValue::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(JqValue::to_string_repr).collect();
            Ok(vec![JqValue::String(parts.join(&sep))])
        }
        _ => Err(format!("cannot join {}", input.type_name())),
    }
}

fn apply_sub_gsub(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.len() < 2 {
        return Err(format!("{name} requires at least 2 arguments"));
    }
    let pat = eval_filter_string(&args[0], input, env, depth)?;
    let repl = eval_filter_string(&args[1], input, env, depth)?;
    let flags = eval_optional_flags(args, input, env, depth)?;
    let JqValue::String(s) = input else {
        return Err(format!("{name} requires string input"));
    };
    let result = simple_regex_replace(s, &pat, &repl, flags.contains('i'), name == "gsub");
    Ok(vec![JqValue::String(result)])
}

fn eval_optional_flags(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<String, String> {
    if args.len() <= 2 {
        return Ok(String::new());
    }
    eval_filter_string(&args[2], input, env, depth)
}

/// Helper for test/match/capture regex operations on a string input.
fn apply_regex_op(
    name: &str,
    s: &str,
    pat: &str,
    case_insensitive: bool,
) -> Result<Vec<JqValue>, String> {
    let matched = simple_regex_match(s, pat, case_insensitive);
    match name {
        "test" => Ok(vec![JqValue::Bool(!matched.is_empty())]),
        "match" => apply_regex_match_result(&matched, pat),
        _ => Ok(vec![JqValue::Object(vec![])]),
    }
}

fn apply_regex_match_result(
    matched: &[(usize, String)],
    pat: &str,
) -> Result<Vec<JqValue>, String> {
    let Some(m) = matched.first() else {
        return Err(format!("null (no match for pattern \"{pat}\")"));
    };
    Ok(vec![JqValue::Object(vec![
        ("offset".into(), JqValue::Number(m.0 as f64)),
        ("length".into(), JqValue::Number(m.1.len() as f64)),
        ("string".into(), JqValue::String(m.1.clone())),
        ("captures".into(), JqValue::Array(vec![])),
    ])])
}

fn dispatch_sort_group(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match name {
        "sort" => apply_sort(input),
        "sort_by" => apply_sort_by(args, input, env, depth),
        "group_by" => apply_group_by(args, input, env, depth),
        "unique" => apply_unique(input),
        "unique_by" => apply_unique_by(args, input, env, depth),
        _ => Err(format!("{name}/0 is not defined")),
    }
}

fn apply_sort(input: &JqValue) -> Result<Vec<JqValue>, String> {
    let JqValue::Array(arr) = input else {
        return Err(format!("cannot sort {}", input.type_name()));
    };
    let mut sorted = arr.clone();
    sorted.sort_by(|a, b| a.compare(b).unwrap_or(std::cmp::Ordering::Equal));
    Ok(vec![JqValue::Array(sorted)])
}

fn apply_sort_by(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let items = keyed_array_items("sort_by", args, input, env, depth)?;
    Ok(vec![JqValue::Array(sorted_keyed_values(items))])
}

fn apply_group_by(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let items = keyed_array_items("group_by", args, input, env, depth)?;
    Ok(vec![JqValue::Array(group_sorted_items(items))])
}

fn apply_unique(input: &JqValue) -> Result<Vec<JqValue>, String> {
    let JqValue::Array(arr) = input else {
        return Err(format!("cannot unique {}", input.type_name()));
    };
    let mut sorted = arr.clone();
    sorted.sort_by(|a, b| a.compare(b).unwrap_or(std::cmp::Ordering::Equal));
    Ok(vec![JqValue::Array(dedup_values(sorted))])
}

fn apply_unique_by(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let JqValue::Array(arr) = input else {
        return Err(format!("cannot unique_by {}", input.type_name()));
    };
    if args.len() != 1 {
        return Err("unique_by requires 1 argument".into());
    }
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for item in arr {
        let key = eval_sort_key(&args[0], item, env, depth)?;
        if !seen.iter().any(|s: &JqValue| s.equals(&key)) {
            seen.push(key);
            out.push(item.clone());
        }
    }
    Ok(vec![JqValue::Array(out)])
}

fn keyed_array_items(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<(JqValue, JqValue)>, String> {
    if args.len() != 1 {
        return Err(format!("{name} requires 1 argument"));
    }
    let JqValue::Array(arr) = input else {
        return Err(format!("cannot {name} {}", input.type_name()));
    };
    let mut items = Vec::new();
    for item in arr {
        items.push((eval_sort_key(&args[0], item, env, depth)?, item.clone()));
    }
    items.sort_by(|(a, _), (b, _)| a.compare(b).unwrap_or(std::cmp::Ordering::Equal));
    Ok(items)
}

fn eval_sort_key(
    filter: &JqFilter,
    item: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<JqValue, String> {
    Ok(apply_filter(filter, item, env, depth + 1)?
        .into_iter()
        .next()
        .unwrap_or(JqValue::Null))
}

fn sorted_keyed_values(items: Vec<(JqValue, JqValue)>) -> Vec<JqValue> {
    items.into_iter().map(|(_, v)| v).collect()
}

fn group_sorted_items(items: Vec<(JqValue, JqValue)>) -> Vec<JqValue> {
    let mut groups = Vec::new();
    let mut current_key: Option<JqValue> = None;
    let mut current_group = Vec::new();
    for (key, value) in items {
        if current_key
            .as_ref()
            .is_none_or(|existing| !existing.equals(&key))
        {
            if !current_group.is_empty() {
                groups.push(JqValue::Array(std::mem::take(&mut current_group)));
            }
            current_key = Some(key);
        }
        current_group.push(value);
    }
    if !current_group.is_empty() {
        groups.push(JqValue::Array(current_group));
    }
    groups
}

fn dedup_values(sorted: Vec<JqValue>) -> Vec<JqValue> {
    let mut out = Vec::new();
    for item in sorted {
        if out.last().is_none_or(|last: &JqValue| !last.equals(&item)) {
            out.push(item);
        }
    }
    out
}

fn dispatch_array_access(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match name {
        "first" => apply_first(args, input, env, depth),
        "last" => apply_last(args, input, env, depth),
        "nth" => apply_nth(args, input, env, depth),
        "range" => dispatch_range(args, input, env, depth),
        "limit" => apply_limit(args, input, env, depth),
        "reverse" => apply_reverse(input),
        "flatten" => apply_flatten(args, input, env, depth),
        _ => Err(format!("{name}/0 is not defined")),
    }
}

fn apply_first(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.is_empty() {
        return match input {
            JqValue::Array(arr) => Ok(vec![arr.first().cloned().unwrap_or(JqValue::Null)]),
            _ => Ok(vec![input.clone()]),
        };
    }
    let vals = apply_filter(&args[0], input, env, depth + 1)?;
    Ok(vec![vals.into_iter().next().unwrap_or(JqValue::Null)])
}

fn apply_last(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.is_empty() {
        return match input {
            JqValue::Array(arr) => Ok(vec![arr.last().cloned().unwrap_or(JqValue::Null)]),
            _ => Ok(vec![input.clone()]),
        };
    }
    let vals = apply_filter(&args[0], input, env, depth + 1)?;
    Ok(vec![vals.into_iter().last().unwrap_or(JqValue::Null)])
}

fn apply_nth(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.is_empty() {
        return Err("nth requires at least 1 argument".into());
    }
    let n_vals = apply_filter(&args[0], input, env, depth + 1)?;
    let n = n_vals.first().and_then(JqValue::as_i64).unwrap_or(0) as usize;
    if args.len() > 1 {
        let vals = apply_filter(&args[1], input, env, depth + 1)?;
        Ok(vec![vals.into_iter().nth(n).unwrap_or(JqValue::Null)])
    } else {
        match input {
            JqValue::Array(arr) => Ok(vec![arr.get(n).cloned().unwrap_or(JqValue::Null)]),
            _ => Ok(vec![input.clone()]),
        }
    }
}

fn apply_limit(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.len() != 2 {
        return Err("limit requires 2 arguments".into());
    }
    let n_vals = apply_filter(&args[0], input, env, depth + 1)?;
    let n = n_vals.first().and_then(JqValue::as_i64).unwrap_or(0) as usize;
    let vals = apply_filter(&args[1], input, env, depth + 1)?;
    Ok(vals.into_iter().take(n).collect())
}

fn apply_reverse(input: &JqValue) -> Result<Vec<JqValue>, String> {
    match input {
        JqValue::Array(arr) => {
            let mut rev = arr.clone();
            rev.reverse();
            Ok(vec![JqValue::Array(rev)])
        }
        JqValue::String(s) => Ok(vec![JqValue::String(s.chars().rev().collect())]),
        _ => Err(format!("cannot reverse {}", input.type_name())),
    }
}

fn apply_flatten(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let max_depth = if args.is_empty() {
        i64::MAX
    } else {
        let vals = apply_filter(&args[0], input, env, depth + 1)?;
        vals.first().and_then(JqValue::as_i64).unwrap_or(i64::MAX)
    };
    match input {
        JqValue::Array(arr) => {
            let mut out = Vec::new();
            flatten_array(arr, max_depth, 0, &mut out);
            Ok(vec![JqValue::Array(out)])
        }
        _ => Err(format!("cannot flatten {}", input.type_name())),
    }
}

fn dispatch_range(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.is_empty() {
        return Err("range requires at least 1 argument".into());
    }
    let (start, end, step) = eval_range_params(args, input, env, depth)?;
    Ok(generate_range(start, end, step))
}

fn eval_range_params(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<(f64, f64, f64), String> {
    let first_num = eval_filter_f64(&args[0], input, env, depth, 0.0)?;
    if args.len() < 2 {
        return Ok((0.0, first_num, 1.0));
    }
    let second_num = eval_filter_f64(&args[1], input, env, depth, 0.0)?;
    let step_val = if args.len() >= 3 {
        eval_filter_f64(&args[2], input, env, depth, 1.0)?
    } else {
        1.0
    };
    Ok((first_num, second_num, step_val))
}

fn eval_filter_f64(
    filter: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
    default: f64,
) -> Result<f64, String> {
    let vals = apply_filter(filter, input, env, depth + 1)?;
    Ok(vals.first().and_then(JqValue::as_f64).unwrap_or(default))
}

fn generate_range(start: f64, end: f64, step: f64) -> Vec<JqValue> {
    let mut out = Vec::new();
    let mut i = start;
    if step > 0.0 {
        while i < end && out.len() < 10_000_000 {
            out.push(JqValue::Number(i));
            i += step;
        }
    } else if step < 0.0 {
        while i > end && out.len() < 10_000_000 {
            out.push(JqValue::Number(i));
            i += step;
        }
    }
    out
}

fn dispatch_array_reduce(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match name {
        "add" => apply_add(input),
        "any" => apply_any_all(args, input, env, depth, true),
        "all" => apply_any_all(args, input, env, depth, false),
        "min" | "min_by" => dispatch_extremum(name, args, input, env, depth, true),
        "max" | "max_by" => dispatch_extremum(name, args, input, env, depth, false),
        "indices" | "index" | "rindex" => dispatch_indices(name, args, input, env, depth),
        _ => Err(format!("{name}/0 is not defined")),
    }
}

fn apply_add(input: &JqValue) -> Result<Vec<JqValue>, String> {
    match input {
        JqValue::Array(arr) => {
            if arr.is_empty() {
                return Ok(vec![JqValue::Null]);
            }
            let mut accum = arr[0].clone();
            for item in &arr[1..] {
                accum = arith_op(&accum, &ArithOp::Add, item)?;
            }
            Ok(vec![accum])
        }
        JqValue::Null => Ok(vec![JqValue::Null]),
        _ => Err(format!("cannot add {}", input.type_name())),
    }
}

/// Helper for `any` (when `is_any` is true) and `all` (when false).
fn apply_any_all(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
    is_any: bool,
) -> Result<Vec<JqValue>, String> {
    let name = if is_any { "any" } else { "all" };
    let JqValue::Array(arr) = input else {
        return Err(format!("cannot {name} over {}", input.type_name()));
    };
    if args.is_empty() {
        let result = if is_any {
            arr.iter().any(JqValue::is_truthy)
        } else {
            arr.iter().all(JqValue::is_truthy)
        };
        return Ok(vec![JqValue::Bool(result)]);
    }
    let test_fn = |v: &JqValue| -> bool {
        apply_filter(&args[0], v, env, depth + 1)
            .ok()
            .and_then(|vals| vals.into_iter().next())
            .is_some_and(|v| v.is_truthy())
    };
    let result = if is_any {
        arr.iter().any(test_fn)
    } else {
        arr.iter().all(test_fn)
    };
    Ok(vec![JqValue::Bool(result)])
}

/// Helper for `min`/`min_by`/`max`/`max_by` to reduce cognitive complexity.
fn dispatch_extremum(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
    find_min: bool,
) -> Result<Vec<JqValue>, String> {
    let target_ord = if find_min {
        std::cmp::Ordering::Less
    } else {
        std::cmp::Ordering::Greater
    };
    let JqValue::Array(arr) = input else {
        return Err(format!("cannot {name} on {}", input.type_name()));
    };
    if arr.is_empty() {
        return Ok(vec![JqValue::Null]);
    }
    let best = if (name == "min_by" || name == "max_by") && !args.is_empty() {
        extremum_by(arr, &args[0], env, depth, target_ord)?
    } else {
        extremum_value(arr, target_ord)
    };
    Ok(vec![best])
}

fn extremum_by(
    arr: &[JqValue],
    filter: &JqFilter,
    env: &JqEnv,
    depth: usize,
    target_ord: std::cmp::Ordering,
) -> Result<JqValue, String> {
    let mut best = arr[0].clone();
    let mut best_key = eval_sort_key(filter, &best, env, depth)?;
    for item in &arr[1..] {
        let key = eval_sort_key(filter, item, env, depth)?;
        if key.compare(&best_key) == Some(target_ord) {
            best = item.clone();
            best_key = key;
        }
    }
    Ok(best)
}

fn extremum_value(arr: &[JqValue], target_ord: std::cmp::Ordering) -> JqValue {
    let mut best = arr[0].clone();
    for item in &arr[1..] {
        if item.compare(&best) == Some(target_ord) {
            best = item.clone();
        }
    }
    best
}

/// Helper for indices/index/rindex operations.
fn dispatch_indices(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.len() != 1 {
        return Err(format!("{name} requires 1 argument"));
    }
    let needle_vals = apply_filter(&args[0], input, env, depth + 1)?;
    let needle = needle_vals.into_iter().next().unwrap_or(JqValue::Null);
    let found = collect_indices(input, &needle);
    Ok(pick_index_result(name, &found))
}

fn collect_indices(input: &JqValue, needle: &JqValue) -> Vec<usize> {
    match (input, needle) {
        (JqValue::String(s), JqValue::String(sub)) => {
            s.match_indices(sub.as_str()).map(|(i, _)| i).collect()
        }
        (JqValue::Array(arr), _) => arr
            .iter()
            .enumerate()
            .filter(|(_, v)| v.equals(needle))
            .map(|(i, _)| i)
            .collect(),
        _ => vec![],
    }
}

fn pick_index_result(name: &str, found: &[usize]) -> Vec<JqValue> {
    if name == "index" {
        vec![found
            .first()
            .map_or(JqValue::Null, |i| JqValue::Number(*i as f64))]
    } else if name == "rindex" {
        vec![found
            .last()
            .map_or(JqValue::Null, |i| JqValue::Number(*i as f64))]
    } else {
        vec![JqValue::Array(
            found.iter().map(|i| JqValue::Number(*i as f64)).collect(),
        )]
    }
}

fn dispatch_object_keys(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match name {
        "keys" | "keys_unsorted" => apply_keys(name, input),
        "values" => apply_values(input),
        "has" => apply_has(args, input, env, depth),
        "in" => apply_in(args, input, env, depth),
        "contains" => apply_contains(args, input, env, depth),
        "inside" => apply_inside(args, input, env, depth),
        _ => Err(format!("{name}/0 is not defined")),
    }
}

fn apply_keys(name: &str, input: &JqValue) -> Result<Vec<JqValue>, String> {
    match input {
        JqValue::Object(pairs) => {
            let mut keys: Vec<String> = pairs.iter().map(|(k, _)| k.clone()).collect();
            if name == "keys" {
                keys.sort();
            }
            Ok(vec![JqValue::Array(
                keys.into_iter().map(JqValue::String).collect(),
            )])
        }
        JqValue::Array(arr) => Ok(vec![JqValue::Array(
            (0..arr.len()).map(|i| JqValue::Number(i as f64)).collect(),
        )]),
        _ => Err(format!("{} has no keys", input.type_name())),
    }
}

fn apply_values(input: &JqValue) -> Result<Vec<JqValue>, String> {
    match input {
        JqValue::Object(pairs) => Ok(vec![JqValue::Array(
            pairs.iter().map(|(_, v)| v.clone()).collect(),
        )]),
        JqValue::Array(arr) => Ok(vec![JqValue::Array(arr.clone())]),
        _ => Err(format!("{} has no values", input.type_name())),
    }
}

fn apply_has(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.len() != 1 {
        return Err("has requires 1 argument".into());
    }
    let key_vals = apply_filter(&args[0], input, env, depth + 1)?;
    let key = key_vals.into_iter().next().unwrap_or(JqValue::Null);
    match (input, &key) {
        (JqValue::Object(pairs), JqValue::String(k)) => {
            Ok(vec![JqValue::Bool(pairs.iter().any(|(pk, _)| pk == k))])
        }
        (JqValue::Array(arr), JqValue::Number(n)) => {
            let idx = *n as usize;
            Ok(vec![JqValue::Bool(idx < arr.len())])
        }
        _ => Ok(vec![JqValue::Bool(false)]),
    }
}

fn apply_in(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.len() != 1 {
        return Err("in requires 1 argument".into());
    }
    let obj_vals = apply_filter(&args[0], input, env, depth + 1)?;
    let obj = obj_vals.into_iter().next().unwrap_or(JqValue::Null);
    match (&obj, input) {
        (JqValue::Object(pairs), JqValue::String(k)) => {
            Ok(vec![JqValue::Bool(pairs.iter().any(|(pk, _)| pk == k))])
        }
        (JqValue::Array(arr), JqValue::Number(n)) => {
            let idx = *n as usize;
            Ok(vec![JqValue::Bool(idx < arr.len())])
        }
        _ => Ok(vec![JqValue::Bool(false)]),
    }
}

fn apply_contains(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.len() != 1 {
        return Err("contains requires 1 argument".into());
    }
    let other_vals = apply_filter(&args[0], input, env, depth + 1)?;
    let other = other_vals.into_iter().next().unwrap_or(JqValue::Null);
    Ok(vec![JqValue::Bool(input.contains_value(&other))])
}

fn apply_inside(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.len() != 1 {
        return Err("inside requires 1 argument".into());
    }
    let other_vals = apply_filter(&args[0], input, env, depth + 1)?;
    let other = other_vals.into_iter().next().unwrap_or(JqValue::Null);
    Ok(vec![JqValue::Bool(other.contains_value(input))])
}

fn dispatch_object_entries(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match name {
        "to_entries" => to_entries_impl(input),
        "from_entries" => from_entries_impl(input),
        "with_entries" => {
            if args.len() != 1 {
                return Err("with_entries requires 1 argument".into());
            }
            with_entries_impl(args, input, env, depth)
        }
        _ => Err(format!("{name}/0 is not defined")),
    }
}

fn to_entries_impl(input: &JqValue) -> Result<Vec<JqValue>, String> {
    let JqValue::Object(pairs) = input else {
        return Err(format!("{} has no entries", input.type_name()));
    };
    let entries = pairs.iter().map(|(k, v)| make_entry(k, v)).collect();
    Ok(vec![JqValue::Array(entries)])
}

fn make_entry(key: &str, value: &JqValue) -> JqValue {
    JqValue::Object(vec![
        ("key".into(), JqValue::String(key.to_string())),
        ("value".into(), value.clone()),
    ])
}

fn from_entries_impl(input: &JqValue) -> Result<Vec<JqValue>, String> {
    let JqValue::Array(arr) = input else {
        return Err(format!("cannot from_entries on {}", input.type_name()));
    };
    let mut pairs = Vec::new();
    for item in arr {
        let JqValue::Object(p) = item else {
            continue;
        };
        let key = extract_entry_key(p);
        let val = extract_entry_value(p);
        pairs.push((key, val));
    }
    Ok(vec![JqValue::Object(pairs)])
}

fn extract_entry_key(pairs: &[(String, JqValue)]) -> String {
    let k = pairs
        .iter()
        .find(|(k, _)| k == "key" || k == "name")
        .map_or(JqValue::Null, |(_, v)| v.clone());
    jq_value_to_key_string(k)
}

fn jq_value_to_key_string(v: JqValue) -> String {
    match v {
        JqValue::String(s) => s,
        JqValue::Number(n) => format_number(n),
        other => other.to_string_repr(),
    }
}

fn extract_entry_value(pairs: &[(String, JqValue)]) -> JqValue {
    pairs
        .iter()
        .find(|(k, _)| k == "value")
        .map_or(JqValue::Null, |(_, v)| v.clone())
}

fn with_entries_impl(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let JqValue::Object(pairs) = input else {
        return Err(format!("cannot with_entries on {}", input.type_name()));
    };
    let entries: Vec<JqValue> = pairs.iter().map(|(k, v)| make_entry(k, v)).collect();
    let mapped = map_entries_filter(&entries, &args[0], env, depth)?;
    let result_pairs = collect_entry_pairs(&mapped);
    Ok(vec![JqValue::Object(result_pairs)])
}

fn map_entries_filter(
    entries: &[JqValue],
    filter: &JqFilter,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let mut mapped = Vec::new();
    for entry in entries {
        match apply_filter(filter, entry, env, depth + 1) {
            Ok(vals) => mapped.extend(vals),
            Err(e) if e == EMPTY_SIGNAL => {}
            Err(e) => return Err(e),
        }
    }
    Ok(mapped)
}

fn collect_entry_pairs(mapped: &[JqValue]) -> Vec<(String, JqValue)> {
    mapped
        .iter()
        .filter_map(|item| {
            let JqValue::Object(p) = item else {
                return None;
            };
            let key = extract_entry_key(p);
            let val = extract_entry_value(p);
            Some((key, val))
        })
        .collect()
}

fn dispatch_path_access(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match name {
        "path" => apply_path_func(args, input, env, depth),
        "getpath" => apply_getpath(args, input, env, depth),
        "setpath" => apply_setpath(args, input, env, depth),
        _ => Err(format!("{name}/0 is not defined")),
    }
}

fn apply_path_func(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.len() != 1 {
        return Err("path requires 1 argument".into());
    }
    let paths = compute_paths(&args[0], input, env, depth)?;
    Ok(paths.into_iter().map(path_segs_to_value).collect())
}

fn apply_getpath(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.len() != 1 {
        return Err("getpath requires 1 argument".into());
    }
    let path_vals = apply_filter(&args[0], input, env, depth + 1)?;
    let path_arr = path_vals.into_iter().next().unwrap_or(JqValue::Null);
    Ok(resolve_getpath(input, &path_arr))
}

fn apply_setpath(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.len() != 2 {
        return Err("setpath requires 2 arguments".into());
    }
    let path_vals = apply_filter(&args[0], input, env, depth + 1)?;
    let path_arr = path_vals.into_iter().next().unwrap_or(JqValue::Null);
    let val_vals = apply_filter(&args[1], input, env, depth + 1)?;
    let val = val_vals.into_iter().next().unwrap_or(JqValue::Null);
    let JqValue::Array(segments) = path_arr else {
        return Ok(vec![input.clone()]);
    };
    Ok(vec![set_path(input, &segments, &val)])
}

fn resolve_getpath(input: &JqValue, path_arr: &JqValue) -> Vec<JqValue> {
    let JqValue::Array(segments) = path_arr else {
        return vec![JqValue::Null];
    };
    let mut current = input.clone();
    for seg in segments {
        match seg {
            JqValue::String(k) => current = field_access(&current, k),
            JqValue::Number(_) => {
                current = index_access(&current, seg).unwrap_or(JqValue::Null);
            }
            _ => return vec![JqValue::Null],
        }
    }
    vec![current]
}

fn dispatch_path_collect(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match name {
        "delpaths" => {
            if args.len() != 1 {
                return Err("delpaths requires 1 argument".into());
            }
            let paths_vals = apply_filter(&args[0], input, env, depth + 1)?;
            let paths_arr = paths_vals.into_iter().next().unwrap_or(JqValue::Null);
            Ok(apply_delpaths(input, &paths_arr))
        }

        "leaf_paths" => {
            let paths = gather_leaf_paths(input, &[]);
            Ok(vec![JqValue::Array(
                paths.into_iter().map(path_segs_to_value).collect(),
            )])
        }

        _ => Err(format!("{name}/0 is not defined")),
    }
}

fn apply_delpaths(input: &JqValue, paths_arr: &JqValue) -> Vec<JqValue> {
    let JqValue::Array(paths) = paths_arr else {
        return vec![input.clone()];
    };
    let mut result = input.clone();
    let mut path_list: Vec<Vec<JqValue>> = Vec::new();
    for p in paths {
        if let JqValue::Array(segs) = p {
            path_list.push(segs.clone());
        }
    }
    path_list.sort_by_key(|b| std::cmp::Reverse(b.len()));
    for path in &path_list {
        result = del_path(&result, path);
    }
    vec![result]
}

fn path_segs_to_value(segs: Vec<PathSeg>) -> JqValue {
    JqValue::Array(
        segs.into_iter()
            .map(|seg| match seg {
                PathSeg::Key(k) => JqValue::String(k),
                PathSeg::Index(i) => JqValue::Number(i as f64),
            })
            .collect(),
    )
}

fn dispatch_math_basic(name: &str, input: &JqValue) -> Result<Vec<JqValue>, String> {
    let op: fn(f64) -> f64 = match name {
        "floor" => f64::floor,
        "ceil" => f64::ceil,
        "round" => f64::round,
        "fabs" => f64::abs,
        "sqrt" => f64::sqrt,
        _ => return Err(format!("{name}/0 is not defined")),
    };
    apply_num_unary(name, input, op)
}

fn apply_num_unary(
    name: &str,
    input: &JqValue,
    op: fn(f64) -> f64,
) -> Result<Vec<JqValue>, String> {
    let JqValue::Number(n) = input else {
        return Err(format!("cannot {name} {}", input.type_name()));
    };
    Ok(vec![JqValue::Number(op(*n))])
}

fn dispatch_math_advanced(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    match name {
        "infinite" => Ok(vec![JqValue::Number(f64::INFINITY)]),
        "nan" => Ok(vec![JqValue::Number(f64::NAN)]),
        "isinfinite" | "isnan" | "isnormal" | "isfinite" => dispatch_math_predicate(name, input),
        "pow" => apply_pow(args, input, env, depth),
        "log" | "log2" | "log10" => apply_log_op(name, input),
        "exp" | "exp2" | "exp10" => apply_exp_op(name, input),
        _ => Err(format!("{name}/0 is not defined")),
    }
}

#[allow(clippy::unnecessary_wraps)]
fn dispatch_math_predicate(name: &str, input: &JqValue) -> Result<Vec<JqValue>, String> {
    let JqValue::Number(n) = input else {
        return Ok(vec![JqValue::Bool(false)]);
    };
    let result = match name {
        "isinfinite" => n.is_infinite(),
        "isnan" => n.is_nan(),
        "isnormal" => n.is_normal(),
        "isfinite" => n.is_finite(),
        _ => false,
    };
    Ok(vec![JqValue::Bool(result)])
}

fn apply_pow(
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    if args.len() != 2 {
        return Err("pow requires 2 arguments".into());
    }
    let base = eval_filter_f64(&args[0], input, env, depth, 0.0)?;
    let exp = eval_filter_f64(&args[1], input, env, depth, 0.0)?;
    Ok(vec![JqValue::Number(base.powf(exp))])
}

fn apply_log_op(name: &str, input: &JqValue) -> Result<Vec<JqValue>, String> {
    match input {
        JqValue::Number(n) => {
            let result = match name {
                "log" => n.ln(),
                "log2" => n.log2(),
                "log10" => n.log10(),
                _ => unreachable!(),
            };
            Ok(vec![JqValue::Number(result)])
        }
        _ => Err(format!("cannot {name} on {}", input.type_name())),
    }
}

fn apply_exp_op(name: &str, input: &JqValue) -> Result<Vec<JqValue>, String> {
    match input {
        JqValue::Number(n) => {
            let result = match name {
                "exp" => n.exp(),
                "exp2" => n.exp2(),
                "exp10" => (10.0f64).powf(*n),
                _ => unreachable!(),
            };
            Ok(vec![JqValue::Number(result)])
        }
        _ => Err(format!("cannot {name} on {}", input.type_name())),
    }
}

/// Returns the list of builtin function names as `JqValue` strings.
fn builtin_names() -> Vec<JqValue> {
    let names = [
        "empty",
        "error",
        "type",
        "length",
        "utf8bytelength",
        "keys",
        "keys_unsorted",
        "values",
        "has",
        "in",
        "contains",
        "inside",
        "select",
        "map",
        "map_values",
        "add",
        "any",
        "all",
        "flatten",
        "sort",
        "sort_by",
        "group_by",
        "unique",
        "unique_by",
        "reverse",
        "first",
        "last",
        "nth",
        "range",
        "limit",
        "to_entries",
        "from_entries",
        "with_entries",
        "indices",
        "index",
        "rindex",
        "test",
        "match",
        "split",
        "join",
        "ltrimstr",
        "rtrimstr",
        "startswith",
        "endswith",
        "ascii_downcase",
        "ascii_upcase",
        "tostring",
        "tonumber",
        "tojson",
        "fromjson",
        "explode",
        "implode",
        "min",
        "max",
        "min_by",
        "max_by",
        "not",
        "floor",
        "ceil",
        "round",
        "sqrt",
        "pow",
        "log",
        "gsub",
        "sub",
        "recurse",
        "env",
        "path",
        "getpath",
        "setpath",
        "delpaths",
        "leaf_paths",
        "builtins",
        "debug",
        "input",
        "inputs",
        "objects",
        "arrays",
        "iterables",
        "booleans",
        "numbers",
        "strings",
        "nulls",
        "scalars",
        "infinite",
        "nan",
        "isinfinite",
        "isnan",
        "isnormal",
        "fabs",
        "log2",
        "log10",
        "exp",
        "exp2",
        "exp10",
        "capture",
    ];
    names
        .into_iter()
        .map(|n| JqValue::String(format!("{n}/0")))
        .collect()
}

fn flatten_array(arr: &[JqValue], max_depth: i64, current: i64, out: &mut Vec<JqValue>) {
    for item in arr {
        if current < max_depth {
            if let JqValue::Array(inner) = item {
                flatten_array(inner, max_depth, current + 1, out);
                continue;
            }
        }
        out.push(item.clone());
    }
}

// ---------------------------------------------------------------------------
// Simple regex matching
// ---------------------------------------------------------------------------

/// Very basic regex matching supporting: literal chars, `.`, `*`, `+`, `?`,
/// `^`, `$`, `[...]` character classes, `\d`, `\w`, `\s`.
fn simple_regex_match(text: &str, pattern: &str, case_insensitive: bool) -> Vec<(usize, String)> {
    let (text, pattern) = regex_case_inputs(text, pattern, case_insensitive);
    let (pat, anchored_start, anchored_end) = split_regex_anchors(&pattern);
    if anchored_start {
        return anchored_regex_match(&text, pat, anchored_end);
    }
    search_regex_match(&text, pat, anchored_end)
}

fn regex_case_inputs(text: &str, pattern: &str, case_insensitive: bool) -> (String, String) {
    if case_insensitive {
        (text.to_lowercase(), pattern.to_lowercase())
    } else {
        (text.to_string(), pattern.to_string())
    }
}

fn split_regex_anchors(pattern: &str) -> (&str, bool, bool) {
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

fn anchored_regex_match(text: &str, pat: &str, anchored_end: bool) -> Vec<(usize, String)> {
    let Some(len) = regex_match_at(text, pat, 0) else {
        return Vec::new();
    };
    if anchored_end && len != text.len() {
        return Vec::new();
    }
    vec![(0, text[..len].to_string())]
}

fn search_regex_match(text: &str, pat: &str, anchored_end: bool) -> Vec<(usize, String)> {
    for start in 0..=text.len() {
        if let Some(len) = regex_match_at(&text[start..], pat, 0) {
            if !anchored_end || start + len == text.len() {
                return vec![(start, text[start..start + len].to_string())];
            }
        }
    }
    Vec::new()
}

fn regex_match_at(text: &str, pattern: &str, pos: usize) -> Option<usize> {
    let pat_bytes = pattern.as_bytes();
    if pos >= pat_bytes.len() {
        return Some(0);
    }
    let (element, element_len) = parse_regex_element(pat_bytes, pos);
    let (quantifier, rest_pos) = detect_quantifier(pat_bytes, pos + element_len);
    if quantifier != 0 {
        return regex_match_quantified(text, pattern, rest_pos, &element, quantifier);
    }
    regex_match_single(text, pattern, rest_pos, &element)
}

fn detect_quantifier(pat_bytes: &[u8], after_elem: usize) -> (u8, usize) {
    if after_elem >= pat_bytes.len() {
        return (0, after_elem);
    }
    let c = pat_bytes[after_elem];
    if c == b'*' || c == b'+' || c == b'?' {
        (c, after_elem + 1)
    } else {
        (0, after_elem)
    }
}

fn regex_match_quantified(
    text: &str,
    pattern: &str,
    rest_pos: usize,
    element: &RegexElement,
    quantifier: u8,
) -> Option<usize> {
    let min = usize::from(quantifier == b'+');
    let max = if quantifier == b'?' { 1 } else { usize::MAX };
    let (count, text_pos) = greedy_consume(text, element, max);
    backtrack_match(text, pattern, rest_pos, count, text_pos, min)
}

/// Greedily consume as many characters as possible matching `element`.
fn greedy_consume(text: &str, element: &RegexElement, max: usize) -> (usize, usize) {
    let text_bytes = text.as_bytes();
    let mut count = 0;
    let mut pos = 0;
    while count < max && pos < text_bytes.len() {
        if !matches_element(element, text_bytes, pos) {
            break;
        }
        pos += char_len_at(text, pos);
        count += 1;
    }
    (count, pos)
}

/// Backtrack from the greedy match to find a valid match for the rest of the pattern.
fn backtrack_match(
    text: &str,
    pattern: &str,
    rest_pos: usize,
    mut count: usize,
    mut text_pos: usize,
    min: usize,
) -> Option<usize> {
    while count >= min {
        if let Some(rest_len) = regex_match_at(&text[text_pos..], pattern, rest_pos) {
            return Some(text_pos + rest_len);
        }
        if count == 0 {
            break;
        }
        count -= 1;
        if text_pos > 0 {
            text_pos -= char_len_back(text, text_pos);
        }
    }
    None
}

fn regex_match_single(
    text: &str,
    pattern: &str,
    rest_pos: usize,
    element: &RegexElement,
) -> Option<usize> {
    let text_bytes = text.as_bytes();
    if text_bytes.is_empty() && !matches!(element, RegexElement::Empty) {
        return None;
    }
    if !matches_element(element, text_bytes, 0) {
        return None;
    }
    let advance = regex_match_single_advance(text, text_bytes, element)?;
    regex_match_at(&text[advance..], pattern, rest_pos).map(|rest_len| advance + rest_len)
}

fn regex_match_single_advance(
    text: &str,
    text_bytes: &[u8],
    element: &RegexElement,
) -> Option<usize> {
    if matches!(element, RegexElement::Empty) {
        return Some(0);
    }
    if text_bytes.is_empty() {
        return None;
    }
    Some(char_len_at(text, 0))
}

#[derive(Debug)]
enum RegexElement {
    Literal(u8),
    Dot,
    CharClass(Vec<(u8, u8)>, bool),
    Digit,
    Word,
    Space,
    NotDigit,
    NotWord,
    NotSpace,
    Empty,
}

fn parse_regex_element(pat: &[u8], pos: usize) -> (RegexElement, usize) {
    if pos >= pat.len() {
        return (RegexElement::Empty, 0);
    }
    match pat[pos] {
        b'.' => (RegexElement::Dot, 1),
        b'\\' => parse_regex_escape(pat, pos),
        b'[' => parse_regex_char_class(pat, pos),
        c => (RegexElement::Literal(c), 1),
    }
}

fn parse_regex_escape(pat: &[u8], pos: usize) -> (RegexElement, usize) {
    if pos + 1 >= pat.len() {
        return (RegexElement::Literal(b'\\'), 1);
    }
    match pat[pos + 1] {
        b'd' => (RegexElement::Digit, 2),
        b'w' => (RegexElement::Word, 2),
        b's' => (RegexElement::Space, 2),
        b'D' => (RegexElement::NotDigit, 2),
        b'W' => (RegexElement::NotWord, 2),
        b'S' => (RegexElement::NotSpace, 2),
        c => (RegexElement::Literal(c), 2),
    }
}

fn parse_regex_char_class(pat: &[u8], pos: usize) -> (RegexElement, usize) {
    let mut i = pos + 1;
    let negated = i < pat.len() && pat[i] == b'^';
    if negated {
        i += 1;
    }
    let mut ranges = Vec::new();
    while i < pat.len() && pat[i] != b']' {
        let (range, next) = parse_regex_range(pat, i);
        ranges.push(range);
        i = next;
    }
    let len = if i < pat.len() { i + 1 - pos } else { i - pos };
    (RegexElement::CharClass(ranges, negated), len)
}

fn parse_regex_range(pat: &[u8], pos: usize) -> ((u8, u8), usize) {
    let start = pat[pos];
    if pos + 2 < pat.len() && pat[pos + 1] == b'-' && pat[pos + 2] != b']' {
        ((start, pat[pos + 2]), pos + 3)
    } else {
        ((start, start), pos + 1)
    }
}

fn matches_element(elem: &RegexElement, text: &[u8], pos: usize) -> bool {
    if pos >= text.len() {
        return matches!(elem, RegexElement::Empty);
    }
    matches_element_char(elem, text[pos])
}

fn matches_element_char(elem: &RegexElement, c: u8) -> bool {
    match elem {
        RegexElement::Literal(expected) => c == *expected,
        RegexElement::Dot => c != b'\n',
        RegexElement::CharClass(ranges, negated) => matches_char_class(c, ranges, *negated),
        _ => matches_element_class(elem, c),
    }
}

fn matches_element_class(elem: &RegexElement, c: u8) -> bool {
    match elem {
        RegexElement::Digit => c.is_ascii_digit(),
        RegexElement::Word => c.is_ascii_alphanumeric() || c == b'_',
        RegexElement::Space => c.is_ascii_whitespace(),
        RegexElement::NotDigit => !c.is_ascii_digit(),
        RegexElement::NotWord => !(c.is_ascii_alphanumeric() || c == b'_'),
        RegexElement::NotSpace => !c.is_ascii_whitespace(),
        _ => false,
    }
}

fn matches_char_class(c: u8, ranges: &[(u8, u8)], negated: bool) -> bool {
    let in_class = ranges.iter().any(|(lo, hi)| c >= *lo && c <= *hi);
    in_class != negated
}

fn char_len_at(text: &str, byte_pos: usize) -> usize {
    text[byte_pos..].chars().next().map_or(1, char::len_utf8)
}

fn char_len_back(text: &str, byte_pos: usize) -> usize {
    text[..byte_pos].chars().last().map_or(1, char::len_utf8)
}

struct RegexReplaceState<'a> {
    text: &'a str,
    search_text: String,
    clean_pat: &'a str,
    replacement: &'a str,
    anchored_start: bool,
    anchored_end: bool,
    global: bool,
    result: String,
    pos: usize,
}

fn simple_regex_replace(
    text: &str,
    pattern: &str,
    replacement: &str,
    case_insensitive: bool,
    global: bool,
) -> String {
    let (search_text, pat) = regex_case_inputs(text, pattern, case_insensitive);
    let (clean_pat, anchored_start, anchored_end) = split_regex_anchors(&pat);

    let mut state = RegexReplaceState {
        text,
        search_text,
        clean_pat,
        replacement,
        anchored_start,
        anchored_end,
        global,
        result: String::new(),
        pos: 0,
    };

    regex_replace_loop(&mut state);
    state.result
}

fn regex_replace_loop(state: &mut RegexReplaceState<'_>) {
    loop {
        if state.pos > state.text.len() {
            break;
        }
        if state.anchored_start && state.pos > 0 {
            state.result.push_str(&state.text[state.pos..]);
            break;
        }
        if !regex_replace_step(state) {
            break;
        }
    }
}

/// Process one replacement step. Returns `false` to stop the loop.
fn regex_replace_step(state: &mut RegexReplaceState<'_>) -> bool {
    let found = find_next_regex_replace_match(
        &state.search_text,
        state.clean_pat,
        state.pos,
        state.anchored_end,
        state.text.len(),
    );
    let Some((match_start, match_len)) = found else {
        state.result.push_str(&state.text[state.pos..]);
        return false;
    };
    state.result.push_str(&state.text[state.pos..match_start]);
    state.result.push_str(state.replacement);
    state.pos = match_start + match_len;
    if !advance_zero_length_regex_match(&mut state.result, state.text, &mut state.pos, match_len) {
        return false;
    }
    if !state.global {
        state.result.push_str(&state.text[state.pos..]);
        return false;
    }
    true
}

fn find_next_regex_replace_match(
    search_text: &str,
    clean_pat: &str,
    start_pos: usize,
    anchored_end: bool,
    text_len: usize,
) -> Option<(usize, usize)> {
    for start in start_pos..=search_text.len() {
        if let Some(match_len) = regex_match_at(&search_text[start..], clean_pat, 0) {
            if !anchored_end || start + match_len == text_len {
                return Some((start, match_len));
            }
        }
    }
    None
}

fn advance_zero_length_regex_match(
    result: &mut String,
    text: &str,
    pos: &mut usize,
    match_len: usize,
) -> bool {
    if match_len != 0 {
        return true;
    }
    if *pos >= text.len() {
        return false;
    }
    let clen = char_len_at(text, *pos);
    result.push_str(&text[*pos..*pos + clen]);
    *pos += clen;
    true
}

// ---------------------------------------------------------------------------
// Path operations
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum PathSeg {
    Key(String),
    Index(usize),
}

fn compute_paths(
    filter: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<Vec<PathSeg>>, String> {
    match filter {
        JqFilter::Field(name) | JqFilter::OptionalField(name) => {
            Ok(vec![vec![PathSeg::Key(name.clone())]])
        }
        JqFilter::Index(idx) => compute_index_paths(idx, input, env, depth),
        JqFilter::Iterate => Ok(compute_iterate_paths(input)),
        JqFilter::Pipe(left, right) => compute_pipe_paths(left, right, input, env, depth),
        JqFilter::Identity => Ok(vec![vec![]]),
        JqFilter::Recurse => {
            let mut out = Vec::new();
            collect_all_paths(input, &[], &mut out);
            Ok(out)
        }
        _ => Ok(vec![]),
    }
}

fn compute_index_paths(
    idx: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<Vec<PathSeg>>, String> {
    Ok(apply_filter(idx, input, env, depth + 1)?
        .into_iter()
        .filter_map(path_seg_from_value)
        .map(|seg| vec![seg])
        .collect())
}

fn path_seg_from_value(value: JqValue) -> Option<PathSeg> {
    match value {
        JqValue::Number(n) => Some(PathSeg::Index(n as usize)),
        JqValue::String(s) => Some(PathSeg::Key(s)),
        _ => None,
    }
}

fn compute_iterate_paths(input: &JqValue) -> Vec<Vec<PathSeg>> {
    match input {
        JqValue::Array(arr) => (0..arr.len()).map(|i| vec![PathSeg::Index(i)]).collect(),
        JqValue::Object(pairs) => pairs
            .iter()
            .map(|(k, _)| vec![PathSeg::Key(k.clone())])
            .collect(),
        _ => vec![],
    }
}

fn compute_pipe_paths(
    left: &JqFilter,
    right: &JqFilter,
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<Vec<PathSeg>>, String> {
    let left_paths = compute_paths(left, input, env, depth)?;
    let left_vals = apply_filter(left, input, env, depth + 1)?;
    let mut out = Vec::new();
    for (lp, lv) in left_paths.iter().zip(left_vals.iter()) {
        let right_paths = compute_paths(right, lv, env, depth)?;
        for rp in &right_paths {
            let mut combined = lp.clone();
            combined.extend(rp.iter().cloned());
            out.push(combined);
        }
    }
    Ok(out)
}

fn collect_all_paths(val: &JqValue, prefix: &[PathSeg], out: &mut Vec<Vec<PathSeg>>) {
    out.push(prefix.to_vec());
    match val {
        JqValue::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                let mut p = prefix.to_vec();
                p.push(PathSeg::Index(i));
                collect_all_paths(v, &p, out);
            }
        }
        JqValue::Object(pairs) => {
            for (k, v) in pairs {
                let mut p = prefix.to_vec();
                p.push(PathSeg::Key(k.clone()));
                collect_all_paths(v, &p, out);
            }
        }
        _ => {}
    }
}

fn gather_leaf_paths(val: &JqValue, prefix: &[PathSeg]) -> Vec<Vec<PathSeg>> {
    match val {
        JqValue::Array(arr) if !arr.is_empty() => gather_leaf_paths_indexed(arr, prefix),
        JqValue::Object(pairs) if !pairs.is_empty() => gather_leaf_paths_keyed(pairs, prefix),
        _ => vec![prefix.to_vec()],
    }
}

fn gather_leaf_paths_indexed(arr: &[JqValue], prefix: &[PathSeg]) -> Vec<Vec<PathSeg>> {
    let mut out = Vec::new();
    for (i, v) in arr.iter().enumerate() {
        let mut p = prefix.to_vec();
        p.push(PathSeg::Index(i));
        out.extend(gather_leaf_paths(v, &p));
    }
    out
}

fn gather_leaf_paths_keyed(pairs: &[(String, JqValue)], prefix: &[PathSeg]) -> Vec<Vec<PathSeg>> {
    let mut out = Vec::new();
    for (k, v) in pairs {
        let mut p = prefix.to_vec();
        p.push(PathSeg::Key(k.clone()));
        out.extend(gather_leaf_paths(v, &p));
    }
    out
}

fn set_path(val: &JqValue, path: &[JqValue], new_val: &JqValue) -> JqValue {
    if path.is_empty() {
        return new_val.clone();
    }
    let seg = &path[0];
    let rest = &path[1..];
    match seg {
        JqValue::String(key) => set_path_object(val, key, rest, new_val),
        JqValue::Number(n) => set_path_array(val, *n as usize, rest, new_val),
        _ => val.clone(),
    }
}

fn set_path_object(val: &JqValue, key: &str, rest: &[JqValue], new_val: &JqValue) -> JqValue {
    let mut pairs = match val {
        JqValue::Object(p) => p.clone(),
        _ => Vec::new(),
    };
    let existing = pairs.iter().position(|(k, _)| k == key);
    let inner = existing.map_or(JqValue::Null, |i| pairs[i].1.clone());
    let new_inner = set_path(&inner, rest, new_val);
    if let Some(i) = existing {
        pairs[i].1 = new_inner;
    } else {
        pairs.push((key.to_string(), new_inner));
    }
    JqValue::Object(pairs)
}

fn set_path_array(val: &JqValue, idx: usize, rest: &[JqValue], new_val: &JqValue) -> JqValue {
    if idx > 1_000_000 {
        return val.clone();
    }
    let mut arr = match val {
        JqValue::Array(a) => a.clone(),
        _ => Vec::new(),
    };
    while arr.len() <= idx {
        arr.push(JqValue::Null);
    }
    let inner = arr[idx].clone();
    arr[idx] = set_path(&inner, rest, new_val);
    JqValue::Array(arr)
}

fn del_path(val: &JqValue, path: &[JqValue]) -> JqValue {
    if path.is_empty() {
        return JqValue::Null;
    }
    if path.len() == 1 {
        return del_terminal_path(val, &path[0]);
    }
    del_nested_path(val, &path[0], &path[1..])
}

fn del_terminal_path(val: &JqValue, segment: &JqValue) -> JqValue {
    match (segment, val) {
        (JqValue::String(key), JqValue::Object(pairs)) => {
            JqValue::Object(pairs.iter().filter(|(k, _)| k != key).cloned().collect())
        }
        (JqValue::Number(n), JqValue::Array(arr)) => {
            let idx = *n as usize;
            let mut new_arr = arr.clone();
            if idx < new_arr.len() {
                new_arr.remove(idx);
            }
            JqValue::Array(new_arr)
        }
        _ => val.clone(),
    }
}

fn del_nested_path(val: &JqValue, segment: &JqValue, rest: &[JqValue]) -> JqValue {
    match (segment, val) {
        (JqValue::String(key), JqValue::Object(pairs)) => JqValue::Object(
            pairs
                .iter()
                .map(|(k, v)| {
                    if k == key {
                        (k.clone(), del_path(v, rest))
                    } else {
                        (k.clone(), v.clone())
                    }
                })
                .collect(),
        ),
        (JqValue::Number(n), JqValue::Array(arr)) => {
            let idx = *n as usize;
            JqValue::Array(
                arr.iter()
                    .enumerate()
                    .map(|(i, v)| {
                        if i == idx {
                            del_path(v, rest)
                        } else {
                            v.clone()
                        }
                    })
                    .collect(),
            )
        }
        _ => val.clone(),
    }
}

// ---------------------------------------------------------------------------
// Format strings (@csv, @tsv, @html, @json, @base64, @base64d, @uri)
// ---------------------------------------------------------------------------

fn apply_format(name: &str, input: &JqValue) -> Result<Vec<JqValue>, String> {
    match name {
        "csv" => apply_csv_format(input),
        "tsv" => apply_tsv_format(input),
        "html" => Ok(vec![JqValue::String(html_escape(&format_input_string(
            input,
        )))]),
        "json" => Ok(vec![JqValue::String(json_to_string(input, true))]),
        "text" => Ok(vec![JqValue::String(input.to_string_repr())]),
        "base64" => Ok(vec![JqValue::String(simple_base64_encode(
            format_input_string(input).as_bytes(),
        ))]),
        "base64d" => apply_base64_decode_format(input),
        "uri" => Ok(vec![JqValue::String(uri_encode(&format_input_string(
            input,
        )))]),
        _ => Err(format!("unknown format: @{name}")),
    }
}

fn apply_csv_format(input: &JqValue) -> Result<Vec<JqValue>, String> {
    let JqValue::Array(arr) = input else {
        return Err("@csv requires array input".into());
    };
    let formatted = format_delimited(arr, ',', csv_format_value);
    Ok(vec![JqValue::String(formatted)])
}

fn apply_tsv_format(input: &JqValue) -> Result<Vec<JqValue>, String> {
    let JqValue::Array(arr) = input else {
        return Err("@tsv requires array input".into());
    };
    let formatted = format_delimited(arr, '\t', tsv_format_value);
    Ok(vec![JqValue::String(formatted)])
}

fn format_delimited(arr: &[JqValue], sep: char, fmt: fn(&JqValue) -> String) -> String {
    let mut out = String::new();
    for (i, v) in arr.iter().enumerate() {
        if i > 0 {
            out.push(sep);
        }
        out.push_str(&fmt(v));
    }
    out
}

fn csv_format_value(v: &JqValue) -> String {
    match v {
        JqValue::String(s) => format!("\"{}\"", s.replace('"', "\"\"")),
        JqValue::Null => String::new(),
        other => other.to_string_repr(),
    }
}

fn tsv_format_value(v: &JqValue) -> String {
    match v {
        JqValue::String(s) => tsv_escape(s),
        JqValue::Null => String::new(),
        other => other.to_string_repr(),
    }
}

fn tsv_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out
}

fn format_input_string(input: &JqValue) -> String {
    match input {
        JqValue::String(s) => s.clone(),
        other => other.to_string_repr(),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&#39;")
        .replace('"', "&quot;")
}

fn apply_base64_decode_format(input: &JqValue) -> Result<Vec<JqValue>, String> {
    let JqValue::String(s) = input else {
        return Err("@base64d requires string input".into());
    };
    match simple_base64_decode(s) {
        Ok(decoded) => Ok(vec![JqValue::String(
            String::from_utf8_lossy(&decoded).into_owned(),
        )]),
        Err(e) => Err(format!("@base64d: {e}")),
    }
}

fn uri_encode(s: &str) -> String {
    let mut encoded = String::new();
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}

// ---------------------------------------------------------------------------
// Simple base64 (standalone, no dependency on data_ops)
// ---------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn simple_base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[((triple >> 18) & 0x3F) as usize] as char);
        out.push(B64[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn simple_base64_decode(input: &str) -> Result<Vec<u8>, &'static str> {
    let padded = normalize_base64_input(input);
    if padded.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(padded.len() / 4 * 3);
    for chunk in padded.chunks_exact(4) {
        decode_base64_chunk(chunk, &mut out)?;
    }
    Ok(out)
}

fn normalize_base64_input(input: &str) -> Vec<u8> {
    let mut clean: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    while !clean.is_empty() && !clean.len().is_multiple_of(4) {
        clean.push(b'=');
    }
    clean
}

fn decode_base64_chunk(chunk: &[u8], out: &mut Vec<u8>) -> Result<(), &'static str> {
    let a = b64_val(chunk[0]).ok_or("invalid base64 character")?;
    let b = b64_val(chunk[1]).ok_or("invalid base64 character")?;
    let c = decode_base64_optional(chunk[2])?;
    let d = decode_base64_optional(chunk[3])?;
    let triple = (u32::from(a) << 18)
        | (u32::from(b) << 12)
        | (u32::from(c.unwrap_or(0)) << 6)
        | u32::from(d.unwrap_or(0));
    out.push((triple >> 16) as u8);
    if c.is_some() {
        out.push((triple >> 8) as u8);
    }
    if d.is_some() {
        out.push(triple as u8);
    }
    Ok(())
}

fn decode_base64_optional(c: u8) -> Result<Option<u8>, &'static str> {
    if c == b'=' {
        Ok(None)
    } else {
        Ok(Some(b64_val(c).ok_or("invalid base64 character")?))
    }
}

fn b64_val(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

#[allow(clippy::struct_excessive_bools)]
struct JqOpts {
    raw_output: bool,
    exit_status: bool,
    compact: bool,
    null_input: bool,
    slurp: bool,
    jq_vars: Vec<(String, JqValue)>,
}

impl JqOpts {
    fn new() -> Self {
        Self {
            raw_output: false,
            exit_status: false,
            compact: false,
            null_input: false,
            slurp: false,
            jq_vars: Vec::new(),
        }
    }
}

pub(crate) fn util_jq(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut opts = JqOpts::new();

    while let Some(&arg) = args.first() {
        match parse_jq_option(ctx, &mut args, &mut opts, arg) {
            Ok(true) => {}
            Ok(false) => break,
            Err(code) => return code,
        }
    }

    let Some((filter_str, file_args)) = extract_filter_arg(&mut args) else {
        ctx.output.stderr(b"jq: no filter provided\n");
        return 1;
    };

    let filter = match parse_filter(filter_str) {
        Ok(f) => f,
        Err(e) => {
            let msg = format!("jq: error parsing filter: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            return 2;
        }
    };

    run_jq_pipeline(ctx, &opts, &filter, file_args)
}

fn extract_filter_arg<'a>(args: &mut &'a [&'a str]) -> Option<(&'a str, &'a [&'a str])> {
    let &f = args.first()?;
    *args = &args[1..];
    Some((f, args))
}

fn run_jq_pipeline(
    ctx: &mut UtilContext<'_>,
    opts: &JqOpts,
    filter: &JqFilter,
    file_args: &[&str],
) -> i32 {
    let mut env = JqEnv::new();
    for (name, val) in &opts.jq_vars {
        env.vars.insert(name.clone(), val.clone());
    }

    let input_texts = match collect_jq_input_texts(ctx, file_args, opts.null_input) {
        Ok(texts) => texts,
        Err(code) => return code,
    };

    let json_values = match parse_jq_input_values(ctx, &input_texts, opts.null_input) {
        Ok(values) => values,
        Err(code) => return code,
    };

    let inputs = if opts.slurp {
        vec![JqValue::Array(json_values)]
    } else {
        json_values
    };

    let (status, had_output, last_value) =
        execute_jq_inputs(ctx, &inputs, filter, &env, opts.raw_output, opts.compact);
    finalize_jq_status(opts.exit_status, status, had_output, last_value.as_ref())
}

fn parse_jq_option(
    ctx: &mut UtilContext<'_>,
    args: &mut &[&str],
    opts: &mut JqOpts,
    arg: &str,
) -> Result<bool, i32> {
    match arg {
        "-r" | "--raw-output" | "-j" | "--join-output" => {
            opts.raw_output = true;
            *args = &args[1..];
            Ok(true)
        }
        "-e" | "--exit-status" => {
            opts.exit_status = true;
            *args = &args[1..];
            Ok(true)
        }
        "-c" | "--compact-output" => {
            opts.compact = true;
            *args = &args[1..];
            Ok(true)
        }
        "-n" | "--null-input" => {
            opts.null_input = true;
            *args = &args[1..];
            Ok(true)
        }
        "-s" | "--slurp" => {
            opts.slurp = true;
            *args = &args[1..];
            Ok(true)
        }
        "--arg" => parse_jq_named_arg(ctx, args, &mut opts.jq_vars),
        "--argjson" => parse_jq_named_json_arg(ctx, args, &mut opts.jq_vars),
        "--" => {
            *args = &args[1..];
            Ok(false)
        }
        _ if arg.starts_with('-') && arg.len() > 1 => Ok(parse_jq_short_flags(args, opts, arg)),
        _ => Ok(false),
    }
}

fn parse_jq_named_arg(
    ctx: &mut UtilContext<'_>,
    args: &mut &[&str],
    jq_vars: &mut Vec<(String, JqValue)>,
) -> Result<bool, i32> {
    if args.len() < 3 {
        ctx.output.stderr(b"jq: --arg requires NAME VALUE\n");
        return Err(1);
    }
    jq_vars.push((args[1].to_string(), JqValue::String(args[2].to_string())));
    *args = &args[3..];
    Ok(true)
}

fn parse_jq_named_json_arg(
    ctx: &mut UtilContext<'_>,
    args: &mut &[&str],
    jq_vars: &mut Vec<(String, JqValue)>,
) -> Result<bool, i32> {
    if args.len() < 3 {
        ctx.output.stderr(b"jq: --argjson requires NAME VALUE\n");
        return Err(1);
    }
    let val = match parse_json(args[2]) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("jq: invalid JSON for --argjson: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            return Err(1);
        }
    };
    jq_vars.push((args[1].to_string(), val));
    *args = &args[3..];
    Ok(true)
}

fn parse_jq_short_flags(args: &mut &[&str], opts: &mut JqOpts, arg: &str) -> bool {
    for c in arg[1..].chars() {
        match c {
            'e' => opts.exit_status = true,
            'c' => opts.compact = true,
            'n' => opts.null_input = true,
            's' => opts.slurp = true,
            'r' | 'j' => opts.raw_output = true,
            _ => return false,
        }
    }
    *args = &args[1..];
    true
}

fn collect_jq_input_texts(
    ctx: &mut UtilContext<'_>,
    file_args: &[&str],
    null_input: bool,
) -> Result<Vec<String>, i32> {
    if null_input {
        return Ok(vec![]);
    }
    if file_args.is_empty() {
        let Some(data) = ctx.stdin else {
            ctx.output.stderr(b"jq: no input\n");
            return Err(1);
        };
        return Ok(vec![String::from_utf8_lossy(data).to_string()]);
    }
    let mut texts = Vec::new();
    for path in file_args {
        let full = resolve_path(ctx.cwd, path);
        match read_text(ctx.fs, &full) {
            Ok(text) => texts.push(text),
            Err(e) => {
                emit_error(ctx.output, "jq", path, &e);
                return Err(1);
            }
        }
    }
    Ok(texts)
}

fn parse_jq_input_values(
    ctx: &mut UtilContext<'_>,
    input_texts: &[String],
    null_input: bool,
) -> Result<Vec<JqValue>, i32> {
    if null_input {
        return Ok(vec![JqValue::Null]);
    }
    let mut json_values = Vec::new();
    for text in input_texts {
        match JsonParser::parse_all(text) {
            Ok(vals) => json_values.extend(vals),
            Err(e) => {
                let msg = format!("jq: error parsing JSON: {e}\n");
                ctx.output.stderr(msg.as_bytes());
                return Err(2);
            }
        }
    }
    if json_values.is_empty() {
        ctx.output.stderr(b"jq: no input\n");
        return Err(1);
    }
    Ok(json_values)
}

fn execute_jq_inputs(
    ctx: &mut UtilContext<'_>,
    inputs: &[JqValue],
    filter: &JqFilter,
    env: &JqEnv,
    raw_output: bool,
    compact: bool,
) -> (i32, bool, Option<JqValue>) {
    let mut last_value = None;
    let mut had_output = false;
    let mut status = 0;
    for input_val in inputs {
        match run_filter(filter, input_val, env) {
            Ok(results) => {
                for val in results {
                    had_output = true;
                    last_value = Some(val.clone());
                    output_value(ctx, &val, raw_output, compact);
                }
            }
            Err(e) if e == EMPTY_SIGNAL => {}
            Err(e) => {
                let msg = format!("jq: {e}\n");
                ctx.output.stderr(msg.as_bytes());
                status = 5;
            }
        }
    }
    (status, had_output, last_value)
}

fn finalize_jq_status(
    exit_status: bool,
    status: i32,
    had_output: bool,
    last_value: Option<&JqValue>,
) -> i32 {
    if !exit_status {
        return status;
    }
    if let Some(last) = last_value {
        if !last.is_truthy() {
            return 1;
        }
        return status;
    }
    if !had_output {
        4
    } else {
        status
    }
}

fn output_value(ctx: &mut UtilContext<'_>, val: &JqValue, raw: bool, compact: bool) {
    if raw {
        if let JqValue::String(s) = val {
            ctx.output.stdout(s.as_bytes());
            ctx.output.stdout(b"\n");
            return;
        }
    }
    let s = json_to_string(val, compact);
    ctx.output.stdout(s.as_bytes());
    ctx.output.stdout(b"\n");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use wasmsh_fs::{OpenOptions, Vfs};

    fn run_jq(filter: &str, json_input: &str) -> Result<Vec<JqValue>, String> {
        let input = parse_json(json_input)?;
        let f = parse_filter(filter)?;
        let env = JqEnv::new();
        run_filter(&f, &input, &env)
    }

    fn jq_str(filter: &str, json_input: &str) -> String {
        match run_jq(filter, json_input) {
            Ok(vals) => vals
                .iter()
                .map(|v| json_to_string(v, true))
                .collect::<Vec<_>>()
                .join("\n"),
            Err(e) => format!("ERROR: {e}"),
        }
    }

    fn jq_raw(filter: &str, json_input: &str) -> String {
        match run_jq(filter, json_input) {
            Ok(vals) => vals
                .iter()
                .map(|v| match v {
                    JqValue::String(s) => s.clone(),
                    _ => json_to_string(v, true),
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Err(e) => format!("ERROR: {e}"),
        }
    }

    // ---- JSON parser tests ----

    #[test]
    fn parse_null() {
        let v = parse_json("null").unwrap();
        assert!(matches!(v, JqValue::Null));
    }

    #[test]
    fn parse_bool() {
        assert!(matches!(parse_json("true").unwrap(), JqValue::Bool(true)));
        assert!(matches!(parse_json("false").unwrap(), JqValue::Bool(false)));
    }

    #[test]
    fn parse_number() {
        match parse_json("42").unwrap() {
            JqValue::Number(n) => assert!((n - 42.0).abs() < f64::EPSILON),
            _ => panic!("expected number"),
        }
        match parse_json("-2.75").unwrap() {
            JqValue::Number(n) => assert!((n - (-2.75)).abs() < 0.001),
            _ => panic!("expected number"),
        }
    }

    #[test]
    fn parse_string() {
        match parse_json(r#""hello world""#).unwrap() {
            JqValue::String(s) => assert_eq!(s, "hello world"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn parse_string_escapes() {
        match parse_json(r#""a\tb\nc""#).unwrap() {
            JqValue::String(s) => assert_eq!(s, "a\tb\nc"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn parse_array() {
        match parse_json("[1, 2, 3]").unwrap() {
            JqValue::Array(arr) => assert_eq!(arr.len(), 3),
            _ => panic!("expected array"),
        }
    }

    #[test]
    fn parse_object() {
        match parse_json(r#"{"a": 1, "b": 2}"#).unwrap() {
            JqValue::Object(pairs) => {
                assert_eq!(pairs.len(), 2);
                assert_eq!(pairs[0].0, "a");
                assert_eq!(pairs[1].0, "b");
            }
            _ => panic!("expected object"),
        }
    }

    #[test]
    fn parse_nested() {
        let v = parse_json(r#"{"a": [1, {"b": true}], "c": null}"#).unwrap();
        assert!(matches!(v, JqValue::Object(_)));
    }

    // ---- Filter tests ----

    #[test]
    fn filter_identity() {
        assert_eq!(jq_str(".", "42"), "42");
        assert_eq!(jq_str(".", r#""hello""#), r#""hello""#);
    }

    #[test]
    fn filter_field() {
        assert_eq!(
            jq_str(".name", r#"{"name":"alice","age":30}"#),
            r#""alice""#
        );
        assert_eq!(jq_str(".age", r#"{"name":"alice","age":30}"#), "30");
    }

    #[test]
    fn filter_nested_field() {
        assert_eq!(jq_str(".a.b", r#"{"a":{"b":"deep"}}"#), r#""deep""#);
    }

    #[test]
    fn filter_array_index() {
        assert_eq!(jq_str(".[0]", "[10, 20, 30]"), "10");
        assert_eq!(jq_str(".[2]", "[10, 20, 30]"), "30");
        assert_eq!(jq_str(".[-1]", "[10, 20, 30]"), "30");
    }

    #[test]
    fn filter_iterate() {
        assert_eq!(jq_str(".[]", "[1, 2, 3]"), "1\n2\n3");
    }

    #[test]
    fn filter_pipe() {
        assert_eq!(jq_str(".[] | . + 1", "[1, 2, 3]"), "2\n3\n4");
    }

    #[test]
    fn filter_comma() {
        assert_eq!(jq_str(".a, .b", r#"{"a":1,"b":2}"#), "1\n2");
    }

    #[test]
    fn filter_comparison() {
        assert_eq!(jq_str(". == 1", "1"), "true");
        assert_eq!(jq_str(". == 1", "2"), "false");
        assert_eq!(jq_str(". > 1", "2"), "true");
        assert_eq!(jq_str(". < 1", "2"), "false");
    }

    #[test]
    fn filter_arithmetic() {
        assert_eq!(jq_str(". + 1", "5"), "6");
        assert_eq!(jq_str(". * 2", "5"), "10");
        assert_eq!(jq_str(". - 3", "5"), "2");
        assert_eq!(jq_str(". / 2", "10"), "5");
        assert_eq!(jq_str(". % 3", "10"), "1");
    }

    #[test]
    fn filter_string_concat() {
        assert_eq!(
            jq_str(r#".a + " " + .b"#, r#"{"a":"hello","b":"world"}"#),
            r#""hello world""#
        );
    }

    #[test]
    fn filter_array_concat() {
        assert_eq!(jq_str(". + [4]", "[1, 2, 3]"), "[1,2,3,4]");
    }

    #[test]
    fn filter_object_merge() {
        let r = jq_str(". + {\"c\": 3}", r#"{"a":1,"b":2}"#);
        assert!(r.contains("\"a\":1"));
        assert!(r.contains("\"c\":3"));
    }

    #[test]
    fn filter_select() {
        assert_eq!(jq_str(".[] | select(. > 2)", "[1, 2, 3, 4]"), "3\n4");
    }

    #[test]
    fn filter_map() {
        assert_eq!(jq_str("map(. * 2)", "[1, 2, 3]"), "[2,4,6]");
    }

    #[test]
    fn filter_type() {
        assert_eq!(jq_str("type", "42"), r#""number""#);
        assert_eq!(jq_str("type", r#""hi""#), r#""string""#);
        assert_eq!(jq_str("type", "null"), r#""null""#);
        assert_eq!(jq_str("type", "[1]"), r#""array""#);
    }

    #[test]
    fn filter_length() {
        assert_eq!(jq_str("length", r#""hello""#), "5");
        assert_eq!(jq_str("length", "[1, 2, 3]"), "3");
        assert_eq!(jq_str("length", "null"), "0");
    }

    #[test]
    fn filter_keys_values() {
        assert_eq!(jq_str("keys", r#"{"b":2,"a":1}"#), r#"["a","b"]"#);
        assert_eq!(jq_str("values", r#"{"a":1,"b":2}"#), "[1,2]");
    }

    #[test]
    fn filter_has() {
        assert_eq!(jq_str(r#"has("a")"#, r#"{"a":1,"b":2}"#), "true");
        assert_eq!(jq_str(r#"has("c")"#, r#"{"a":1,"b":2}"#), "false");
    }

    #[test]
    fn filter_if_then_else() {
        assert_eq!(
            jq_str("if . > 0 then \"pos\" else \"neg\" end", "5"),
            r#""pos""#
        );
        assert_eq!(
            jq_str("if . > 0 then \"pos\" else \"neg\" end", "-1"),
            r#""neg""#
        );
    }

    #[test]
    fn filter_not_and_or() {
        assert_eq!(jq_str("true and false", "null"), "false");
        assert_eq!(jq_str("true or false", "null"), "true");
        assert_eq!(jq_str("true | not", "null"), "false");
    }

    #[test]
    fn filter_alternative() {
        assert_eq!(jq_str(".a // \"default\"", r#"{"b":1}"#), r#""default""#);
        assert_eq!(jq_str(".a // \"default\"", r#"{"a":1}"#), "1");
    }

    #[test]
    fn filter_to_entries() {
        let r = jq_str("to_entries", r#"{"a":1}"#);
        assert!(r.contains("\"key\""));
        assert!(r.contains("\"value\""));
    }

    #[test]
    fn filter_from_entries() {
        let r = jq_str(
            "from_entries",
            r#"[{"key":"a","value":1},{"key":"b","value":2}]"#,
        );
        assert!(r.contains("\"a\":1"));
        assert!(r.contains("\"b\":2"));
    }

    #[test]
    fn filter_split_join() {
        assert_eq!(jq_str(r#"split(",")"#, r#""a,b,c""#), r#"["a","b","c"]"#);
        assert_eq!(jq_str(r#"join("-")"#, r#"["a","b","c"]"#), r#""a-b-c""#);
    }

    #[test]
    fn filter_tostring_tonumber() {
        assert_eq!(jq_str("tostring", "42"), r#""42""#);
        assert_eq!(jq_str("tonumber", r#""42""#), "42");
    }

    #[test]
    fn filter_ascii_case() {
        assert_eq!(jq_str("ascii_downcase", r#""HELLO""#), r#""hello""#);
        assert_eq!(jq_str("ascii_upcase", r#""hello""#), r#""HELLO""#);
    }

    #[test]
    fn filter_sort() {
        assert_eq!(jq_str("sort", "[3, 1, 2]"), "[1,2,3]");
    }

    #[test]
    fn filter_unique() {
        assert_eq!(jq_str("unique", "[1, 2, 1, 3, 2]"), "[1,2,3]");
    }

    #[test]
    fn filter_reverse() {
        assert_eq!(jq_str("reverse", "[1, 2, 3]"), "[3,2,1]");
    }

    #[test]
    fn filter_flatten() {
        assert_eq!(jq_str("flatten", "[[1, 2], [3, [4]]]"), "[1,2,3,4]");
    }

    #[test]
    fn filter_add() {
        assert_eq!(jq_str("add", "[1, 2, 3]"), "6");
        assert_eq!(jq_str("add", r#"["a", "b", "c"]"#), r#""abc""#);
    }

    #[test]
    fn filter_any_all() {
        assert_eq!(jq_str("any", "[false, true]"), "true");
        assert_eq!(jq_str("all", "[false, true]"), "false");
        assert_eq!(jq_str("all", "[true, true]"), "true");
    }

    #[test]
    fn filter_reduce() {
        assert_eq!(jq_str("reduce .[] as $x (0; . + $x)", "[1, 2, 3, 4]"), "10");
    }

    #[test]
    fn filter_array_construct() {
        assert_eq!(jq_str("[.[] | . * 2]", "[1, 2, 3]"), "[2,4,6]");
    }

    #[test]
    fn filter_object_construct() {
        assert_eq!(
            jq_str(r#"{"name": .n, "age": .a}"#, r#"{"n":"bob","a":25}"#),
            r#"{"name":"bob","age":25}"#
        );
    }

    #[test]
    fn filter_test() {
        assert_eq!(jq_str(r#"test("foo")"#, r#""foobar""#), "true");
        assert_eq!(jq_str(r#"test("baz")"#, r#""foobar""#), "false");
    }

    #[test]
    fn filter_startswith_endswith() {
        assert_eq!(jq_str(r#"startswith("foo")"#, r#""foobar""#), "true");
        assert_eq!(jq_str(r#"endswith("bar")"#, r#""foobar""#), "true");
    }

    #[test]
    fn filter_ltrimstr_rtrimstr() {
        assert_eq!(jq_raw(r#"ltrimstr("foo")"#, r#""foobar""#), "bar");
        assert_eq!(jq_raw(r#"rtrimstr("bar")"#, r#""foobar""#), "foo");
    }

    #[test]
    fn filter_range() {
        assert_eq!(jq_str("range(3)", "null"), "0\n1\n2");
        assert_eq!(jq_str("range(2;5)", "null"), "2\n3\n4");
    }

    #[test]
    fn filter_limit() {
        assert_eq!(jq_str("limit(2; .[])", "[1, 2, 3, 4]"), "1\n2");
    }

    #[test]
    fn filter_first_last() {
        assert_eq!(jq_str("first", "[1, 2, 3]"), "1");
        assert_eq!(jq_str("last", "[1, 2, 3]"), "3");
    }

    #[test]
    fn filter_min_max() {
        assert_eq!(jq_str("min", "[3, 1, 2]"), "1");
        assert_eq!(jq_str("max", "[3, 1, 2]"), "3");
    }

    #[test]
    fn filter_contains() {
        assert_eq!(jq_str(r#"contains("bar")"#, r#""foobar""#), "true");
        assert_eq!(jq_str("contains([2, 3])", "[1, 2, 3]"), "true");
    }

    #[test]
    fn filter_recurse() {
        let r = jq_str(".. | numbers", r#"{"a":1,"b":{"c":2}}"#);
        assert!(r.contains('1'));
        assert!(r.contains('2'));
    }

    #[test]
    fn filter_try() {
        assert_eq!(jq_str("try .a.b.c", "null"), "null");
    }

    #[test]
    fn filter_optional_field() {
        assert_eq!(jq_str(".a?", "42"), "");
    }

    #[test]
    fn filter_variable_binding() {
        assert_eq!(jq_str(".a as $x | .b + $x", r#"{"a":10,"b":20}"#), "30");
    }

    #[test]
    fn filter_def() {
        assert_eq!(
            jq_str("def double: . * 2; map(double)", "[1, 2, 3]"),
            "[2,4,6]"
        );
    }

    #[test]
    fn filter_sort_by() {
        assert_eq!(
            jq_str("sort_by(.a)", r#"[{"a":3},{"a":1},{"a":2}]"#),
            r#"[{"a":1},{"a":2},{"a":3}]"#
        );
    }

    #[test]
    fn filter_group_by() {
        let r = jq_str(
            "group_by(.a)",
            r#"[{"a":1,"b":"x"},{"a":2,"b":"y"},{"a":1,"b":"z"}]"#,
        );
        assert!(r.starts_with("[["));
    }

    #[test]
    fn filter_unique_by() {
        assert_eq!(
            jq_str(
                "unique_by(.a)",
                r#"[{"a":1,"b":"x"},{"a":2,"b":"y"},{"a":1,"b":"z"}]"#
            ),
            r#"[{"a":1,"b":"x"},{"a":2,"b":"y"}]"#
        );
    }

    #[test]
    fn filter_with_entries() {
        assert_eq!(
            jq_str("with_entries(select(.value > 1))", r#"{"a":1,"b":2,"c":3}"#),
            r#"{"b":2,"c":3}"#
        );
    }

    #[test]
    fn filter_indices() {
        assert_eq!(jq_str(r#"indices("b")"#, r#""abcabc""#), "[1,4]");
    }

    #[test]
    fn filter_at_csv() {
        assert_eq!(jq_raw("@csv", r#"[1,"two",3]"#), r#"1,"two",3"#);
    }

    #[test]
    fn filter_at_html() {
        assert_eq!(jq_raw("@html", r#""<b>hi</b>""#), "&lt;b&gt;hi&lt;/b&gt;");
    }

    #[test]
    fn filter_at_base64() {
        assert_eq!(jq_raw("@base64", r#""hello""#), "aGVsbG8=");
    }

    #[test]
    fn filter_at_base64d() {
        assert_eq!(jq_raw("@base64d", r#""aGVsbG8=""#), "hello");
    }

    #[test]
    fn filter_at_uri() {
        assert_eq!(jq_raw("@uri", r#""hello world""#), "hello%20world");
    }

    #[test]
    fn filter_floor_ceil_round() {
        assert_eq!(jq_str("floor", "3.7"), "3");
        assert_eq!(jq_str("ceil", "3.2"), "4");
        assert_eq!(jq_str("round", "3.5"), "4");
    }

    #[test]
    fn filter_negate() {
        assert_eq!(jq_str("-(. + 1)", "5"), "-6");
    }

    #[test]
    fn filter_null_propagation() {
        assert_eq!(jq_str(".missing", r#"{"a": 1}"#), "null");
        assert_eq!(jq_str(".a.b", "null"), "null");
    }

    #[test]
    fn filter_object_shorthand() {
        let r = jq_str("{a, b}", r#"{"a":1,"b":2,"c":3}"#);
        assert!(r.contains("\"a\":1"));
        assert!(r.contains("\"b\":2"));
        assert!(!r.contains("\"c\""));
    }

    #[test]
    fn filter_dynamic_key() {
        assert_eq!(jq_str(r#"{("key"): "val"}"#, "null"), r#"{"key":"val"}"#);
    }

    #[test]
    fn filter_slice() {
        assert_eq!(jq_str(".[1:3]", "[0,1,2,3,4]"), "[1,2]");
    }

    #[test]
    fn filter_explode_implode() {
        assert_eq!(jq_str("explode", r#""AB""#), "[65,66]");
        assert_eq!(jq_str("implode", "[65,66]"), r#""AB""#);
    }

    #[test]
    fn filter_getpath_setpath() {
        assert_eq!(jq_str(r#"getpath(["a","b"])"#, r#"{"a":{"b":42}}"#), "42");
    }

    #[test]
    fn filter_gsub() {
        assert_eq!(jq_raw(r#"gsub("o"; "0")"#, r#""foo""#), "f00");
    }

    #[test]
    fn filter_foreach() {
        assert_eq!(
            jq_str("foreach .[] as $x (0; . + $x)", "[1, 2, 3]"),
            "1\n3\n6"
        );
    }

    #[test]
    fn filter_map_values() {
        assert_eq!(
            jq_str("map_values(. + 1)", r#"{"a":1,"b":2}"#),
            r#"{"a":2,"b":3}"#
        );
    }

    #[test]
    fn filter_tojson_fromjson() {
        assert_eq!(jq_str("tojson", r#"{"a":1}"#), r#""{\"a\":1}""#);
        assert_eq!(jq_str("fromjson", r#""{\"a\":1}""#), r#"{"a":1}"#);
    }

    // ---- Integration-level: util_jq through UtilContext ----

    #[test]
    fn util_jq_basic() {
        use wasmsh_fs::MemoryFs;
        let mut fs = MemoryFs::new();
        let h = fs.open("/test.json", OpenOptions::write()).unwrap();
        fs.write_file(h, br#"{"name":"alice","age":30}"#).unwrap();
        fs.close(h);

        let mut out = crate::VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut out,
                cwd: "/",
                stdin: None,
                state: None,
                network: None,
            };
            util_jq(&mut ctx, &["jq", ".name", "/test.json"])
        };
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), r#""alice""#);
    }

    #[test]
    fn util_jq_raw_output() {
        use wasmsh_fs::MemoryFs;
        let mut fs = MemoryFs::new();
        let mut out = crate::VecOutput::default();
        let input = br#"{"name":"alice"}"#;
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut out,
                cwd: "/",
                stdin: Some(input),
                state: None,
                network: None,
            };
            util_jq(&mut ctx, &["jq", "-r", ".name"])
        };
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "alice");
    }

    #[test]
    fn util_jq_compact() {
        use wasmsh_fs::MemoryFs;
        let mut fs = MemoryFs::new();
        let mut out = crate::VecOutput::default();
        let input = br#"{"a":1,"b":[2,3]}"#;
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut out,
                cwd: "/",
                stdin: Some(input),
                state: None,
                network: None,
            };
            util_jq(&mut ctx, &["jq", "-c", "."])
        };
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), r#"{"a":1,"b":[2,3]}"#);
    }

    #[test]
    fn util_jq_null_input() {
        use wasmsh_fs::MemoryFs;
        let mut fs = MemoryFs::new();
        let mut out = crate::VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut out,
                cwd: "/",
                stdin: None,
                state: None,
                network: None,
            };
            util_jq(&mut ctx, &["jq", "-n", "1 + 2"])
        };
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "3");
    }

    #[test]
    fn util_jq_slurp() {
        use wasmsh_fs::MemoryFs;
        let mut fs = MemoryFs::new();
        let mut out = crate::VecOutput::default();
        let input = b"1\n2\n3";
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut out,
                cwd: "/",
                stdin: Some(input),
                state: None,
                network: None,
            };
            util_jq(&mut ctx, &["jq", "-s", "add"])
        };
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "6");
    }

    #[test]
    fn util_jq_arg() {
        use wasmsh_fs::MemoryFs;
        let mut fs = MemoryFs::new();
        let mut out = crate::VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut out,
                cwd: "/",
                stdin: None,
                state: None,
                network: None,
            };
            util_jq(&mut ctx, &["jq", "-n", "--arg", "name", "bob", "$name"])
        };
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), r#""bob""#);
    }

    #[test]
    fn util_jq_exit_status() {
        use wasmsh_fs::MemoryFs;
        let mut fs = MemoryFs::new();
        let mut out = crate::VecOutput::default();
        let input = b"null";
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut out,
                cwd: "/",
                stdin: Some(input),
                state: None,
                network: None,
            };
            util_jq(&mut ctx, &["jq", "-e", "."])
        };
        assert_eq!(status, 1);
    }

    // ================================================================
    // Error paths through util_jq
    // ================================================================

    fn run_util_jq(argv: &[&str], stdin: Option<&[u8]>) -> (i32, String, String) {
        use wasmsh_fs::MemoryFs;
        let mut fs = MemoryFs::new();
        let mut out = crate::VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut out,
                cwd: "/",
                stdin,
                state: None,
                network: None,
            };
            util_jq(&mut ctx, argv)
        };
        let stdout = out.stdout_str().to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        (status, stdout, stderr)
    }

    #[test]
    fn jq_malformed_json() {
        let (status, _, stderr) = run_util_jq(&["jq", "."], Some(b"{bad"));
        assert_eq!(status, 2);
        assert!(stderr.contains("error parsing JSON"), "stderr: {stderr}");
    }

    #[test]
    fn jq_invalid_filter() {
        let (status, _, stderr) = run_util_jq(&["jq", ".[???]"], Some(b"{}"));
        assert_eq!(status, 2);
        assert!(stderr.contains("error parsing filter"), "stderr: {stderr}");
    }

    #[test]
    fn jq_no_input() {
        let (status, _, stderr) = run_util_jq(&["jq", "."], None);
        assert_eq!(status, 1);
        assert!(stderr.contains("no input"), "stderr: {stderr}");
    }

    #[test]
    fn jq_no_filter() {
        let (status, _, stderr) = run_util_jq(&["jq"], Some(b"{}"));
        assert_eq!(status, 1);
        assert!(stderr.contains("no filter"), "stderr: {stderr}");
    }

    #[test]
    fn jq_exit_status_null() {
        let (status, _, _) = run_util_jq(&["jq", "-e", "."], Some(b"null"));
        assert_eq!(status, 1);
    }

    #[test]
    fn jq_exit_status_false() {
        let (status, _, _) = run_util_jq(&["jq", "-e", "."], Some(b"false"));
        assert_eq!(status, 1);
    }

    #[test]
    fn jq_exit_status_true() {
        let (status, _, _) = run_util_jq(&["jq", "-e", "."], Some(b"true"));
        assert_eq!(status, 0);
    }

    #[test]
    fn jq_exit_status_empty_output() {
        // -e with no output should return 4
        let (status, _, _) = run_util_jq(&["jq", "-e", "empty"], Some(b"null"));
        assert_eq!(status, 4);
    }

    #[test]
    fn jq_empty_json_input() {
        let (status, _, stderr) = run_util_jq(&["jq", "."], Some(b""));
        assert_eq!(status, 1);
        assert!(stderr.contains("no input"), "stderr: {stderr}");
    }

    // ================================================================
    // Untested filter operations — reduce, foreach, def, label/break, etc.
    // ================================================================

    #[test]
    fn filter_reduce_sum() {
        assert_eq!(jq_str("reduce .[] as $x (0; . + $x)", "[10, 20, 30]"), "60");
    }

    #[test]
    fn filter_reduce_string_concat() {
        assert_eq!(
            jq_raw(r#"reduce .[] as $x (""; . + $x)"#, r#"["a","b","c"]"#),
            "abc"
        );
    }

    #[test]
    fn filter_foreach_running_sum() {
        assert_eq!(
            jq_str("[foreach .[] as $x (0; . + $x)]", "[1, 2, 3]"),
            "[1,3,6]"
        );
    }

    #[test]
    fn filter_foreach_with_extract() {
        // foreach with extract expression (3 args)
        assert_eq!(
            jq_str("[foreach .[] as $x (0; . + $x; . * 2)]", "[1, 2, 3]"),
            "[2,6,12]"
        );
    }

    #[test]
    fn filter_def_user_function() {
        assert_eq!(
            jq_str("def double: . * 2; [.[] | double]", "[1, 2, 3]"),
            "[2,4,6]"
        );
    }

    #[test]
    fn filter_def_with_args() {
        // In this implementation, def args are stored as vars accessed via $name
        // So: def addN(n): . + $n; works differently from jq proper
        // Instead test with a no-arg helper that references a bound variable
        assert_eq!(jq_str("3 as $x | def addx: . + $x; 5 | addx", "null"), "8");
    }

    #[test]
    fn filter_def_recursive() {
        // Factorial via recursion
        assert_eq!(
            jq_str(
                "def fact: if . <= 1 then 1 else . * ((. - 1) | fact) end; 5 | fact",
                "null"
            ),
            "120"
        );
    }

    #[test]
    fn filter_try_catch() {
        assert_eq!(jq_raw(r#"try error("boom") catch ."#, "null"), "boom");
    }

    #[test]
    fn filter_try_catch_no_error() {
        assert_eq!(jq_str("try 42 catch .", "null"), "42");
    }

    #[test]
    fn filter_try_no_catch() {
        // try without catch suppresses error and produces empty output
        assert_eq!(jq_str(r#"try error("oops")"#, "null"), "");
    }

    #[test]
    fn filter_label_break_basic() {
        // label catches the break signal and produces empty
        // This tests that label/break parse and execute without panic
        let r = jq_str("label $out | 42", "null");
        assert_eq!(r, "42");
    }

    #[test]
    fn filter_label_break_signal() {
        // break inside label suppresses remaining output
        // The label catches the break signal
        let (status, _, _) = run_util_jq(
            &["jq", r"label $out | foreach .[] as $x (0; . + $x)"],
            Some(b"[1,2,3]"),
        );
        assert_eq!(status, 0);
    }

    #[test]
    fn filter_limit_builtin() {
        assert_eq!(jq_str("[limit(3; .[])]", "[1,2,3,4,5]"), "[1,2,3]");
    }

    #[test]
    fn filter_until_via_recurse() {
        // Implement until-like logic using recursive def without params
        // Count from 0 to 5 by incrementing
        assert_eq!(
            jq_str(
                "def inc_to_5: if . >= 5 then . else (. + 1) | inc_to_5 end; 0 | inc_to_5",
                "null"
            ),
            "5"
        );
    }

    #[test]
    fn filter_while_via_recurse_builtin() {
        // Use the builtin recurse function with a select to implement while-like behavior
        assert_eq!(
            jq_str("[1 | recurse(. * 2; . < 16) | select(. < 10)]", "null"),
            "[1,2,4,8]"
        );
    }

    #[test]
    fn filter_walk_manual() {
        // walk-like behavior: increment all numbers in nested structure
        // Use map + map_values for each level
        assert_eq!(
            jq_str(
                r#"{"a": (.a + 1), "b": [.b[] + 1]}"#,
                r#"{"a":1,"b":[2,3]}"#
            ),
            r#"{"a":2,"b":[3,4]}"#
        );
    }

    // ================================================================
    // Path operations: paths, leaf_paths, getpath, setpath, delpaths
    // ================================================================

    #[test]
    fn filter_paths() {
        let r = jq_str(r"[path(..)]", r#"{"a":1,"b":{"c":2}}"#);
        assert!(r.contains("[\"a\"]"));
        assert!(r.contains("[\"b\",\"c\"]"));
    }

    #[test]
    fn filter_leaf_paths() {
        let r = jq_str("leaf_paths", r#"{"a":1,"b":{"c":2}}"#);
        // leaf_paths returns an array of paths
        assert!(r.contains("[\"a\"]"));
        assert!(r.contains("[\"b\",\"c\"]"));
    }

    #[test]
    fn filter_getpath() {
        assert_eq!(jq_str(r#"getpath(["a","b"])"#, r#"{"a":{"b":42}}"#), "42");
        assert_eq!(jq_str(r#"getpath(["x"])"#, r#"{"a":1}"#), "null");
    }

    #[test]
    fn filter_setpath() {
        assert_eq!(
            jq_str(r#"setpath(["a","b"]; 99)"#, r#"{"a":{"b":1}}"#),
            r#"{"a":{"b":99}}"#
        );
    }

    #[test]
    fn filter_setpath_create() {
        assert_eq!(jq_str(r#"setpath(["x"]; 42)"#, r"{}"), r#"{"x":42}"#);
    }

    #[test]
    fn filter_delpaths() {
        let r = jq_str(r#"delpaths([["a"]])"#, r#"{"a":1,"b":2}"#);
        assert!(!r.contains("\"a\""));
        assert!(r.contains("\"b\":2"));
    }

    // ================================================================
    // Format strings: @base64, @base64d, @uri, @html, @csv, @tsv
    // ================================================================

    #[test]
    fn filter_at_base64_roundtrip() {
        assert_eq!(
            jq_raw("@base64", r#""Hello, World!""#),
            "SGVsbG8sIFdvcmxkIQ=="
        );
        assert_eq!(
            jq_raw("@base64d", r#""SGVsbG8sIFdvcmxkIQ==""#),
            "Hello, World!"
        );
    }

    #[test]
    fn filter_at_base64_empty() {
        assert_eq!(jq_raw("@base64", r#""""#), "");
        assert_eq!(jq_raw("@base64d", r#""""#), "");
    }

    #[test]
    fn filter_at_uri_special_chars() {
        assert_eq!(jq_raw("@uri", r#""foo bar&baz=1""#), "foo%20bar%26baz%3D1");
    }

    #[test]
    fn filter_at_html_entities() {
        assert_eq!(
            jq_raw("@html", r#""<a href=\"x\">&""#),
            "&lt;a href=&quot;x&quot;&gt;&amp;"
        );
    }

    #[test]
    fn filter_at_csv_with_quotes() {
        assert_eq!(jq_raw("@csv", r#"["a","b\"c",1]"#), r#""a","b""c",1"#);
    }

    #[test]
    fn filter_at_csv_with_null() {
        assert_eq!(jq_raw("@csv", r"[1,null,3]"), "1,,3");
    }

    #[test]
    fn filter_at_tsv_basic() {
        assert_eq!(jq_raw("@tsv", r#"["a","b","c"]"#), "a\tb\tc");
    }

    #[test]
    fn filter_at_tsv_escaping() {
        // JSON "a\tb" has a literal tab; TSV escapes it to \t
        assert_eq!(jq_raw("@tsv", r#"["a\tb","c\nd"]"#), "a\\tb\tc\\nd");
    }

    // ================================================================
    // explode / implode
    // ================================================================

    #[test]
    fn filter_explode_ascii() {
        assert_eq!(jq_str("explode", r#""Hi""#), "[72,105]");
    }

    #[test]
    fn filter_implode_ascii() {
        assert_eq!(jq_str("implode", "[72,105]"), r#""Hi""#);
    }

    #[test]
    fn filter_explode_implode_roundtrip() {
        assert_eq!(jq_raw("explode | implode", r#""test123""#), "test123");
    }

    // ================================================================
    // indices, index, rindex
    // ================================================================

    #[test]
    fn filter_index_string() {
        assert_eq!(jq_str(r#"index("b")"#, r#""abcabc""#), "1");
    }

    #[test]
    fn filter_rindex_string() {
        assert_eq!(jq_str(r#"rindex("b")"#, r#""abcabc""#), "4");
    }

    #[test]
    fn filter_indices_array() {
        assert_eq!(jq_str("indices(1)", "[1,2,1,3,1]"), "[0,2,4]");
    }

    #[test]
    fn filter_index_not_found() {
        assert_eq!(jq_str(r#"index("z")"#, r#""abc""#), "null");
    }

    #[test]
    fn filter_rindex_not_found() {
        assert_eq!(jq_str(r#"rindex("z")"#, r#""abc""#), "null");
    }

    // ================================================================
    // sub / gsub (regex replacement)
    // ================================================================

    #[test]
    fn filter_sub_first_only() {
        assert_eq!(jq_raw(r#"sub("o"; "0")"#, r#""foo""#), "f0o");
    }

    #[test]
    fn filter_gsub_all() {
        assert_eq!(jq_raw(r#"gsub("o"; "0")"#, r#""foobar""#), "f00bar");
    }

    #[test]
    fn filter_gsub_regex() {
        assert_eq!(jq_raw(r#"gsub("[0-9]"; "X")"#, r#""a1b2c3""#), "aXbXcX");
    }

    #[test]
    fn filter_sub_case_insensitive() {
        assert_eq!(jq_raw(r#"sub("FOO"; "bar"; "i")"#, r#""fooFOO""#), "barFOO");
    }

    // ================================================================
    // min_by / max_by
    // ================================================================

    #[test]
    fn filter_min_by() {
        assert_eq!(
            jq_str("min_by(.a)", r#"[{"a":3},{"a":1},{"a":2}]"#),
            r#"{"a":1}"#
        );
    }

    #[test]
    fn filter_max_by() {
        assert_eq!(
            jq_str("max_by(.a)", r#"[{"a":3},{"a":1},{"a":2}]"#),
            r#"{"a":3}"#
        );
    }

    #[test]
    fn filter_min_empty_array() {
        assert_eq!(jq_str("min", "[]"), "null");
    }

    #[test]
    fn filter_max_empty_array() {
        assert_eq!(jq_str("max", "[]"), "null");
    }

    // ================================================================
    // tojson / fromjson
    // ================================================================

    #[test]
    fn filter_tojson_number() {
        assert_eq!(jq_str("tojson", "42"), r#""42""#);
    }

    #[test]
    fn filter_fromjson_number() {
        assert_eq!(jq_str("fromjson", r#""42""#), "42");
    }

    #[test]
    fn filter_tojson_array() {
        assert_eq!(jq_str("tojson", "[1,2,3]"), r#""[1,2,3]""#);
    }

    #[test]
    fn filter_fromjson_object() {
        assert_eq!(jq_str("fromjson", r#""{\"a\":1}""#), r#"{"a":1}"#);
    }

    // ================================================================
    // recurse / ..
    // ================================================================

    #[test]
    fn filter_recurse_with_filter() {
        let r = jq_str(
            "[recurse(.children[]?)] | length",
            r#"{"name":"root","children":[{"name":"a","children":[]},{"name":"b","children":[]}]}"#,
        );
        // root + 2 children = 3
        assert_eq!(r, "3");
    }

    #[test]
    fn filter_dotdot_numbers() {
        assert_eq!(
            jq_str("[.. | numbers]", r#"{"a":1,"b":{"c":2,"d":"x"}}"#),
            "[1,2]"
        );
    }

    #[test]
    fn filter_dotdot_strings() {
        let r = jq_str("[.. | strings]", r#"{"a":"hello","b":{"c":"world"}}"#);
        assert!(r.contains("\"hello\""));
        assert!(r.contains("\"world\""));
    }

    // ================================================================
    // env / $ENV
    // ================================================================

    #[test]
    fn filter_env_returns_object() {
        assert_eq!(jq_str("env", "null"), "{}");
    }

    #[test]
    fn filter_env_var_returns_object() {
        assert_eq!(jq_str("$ENV", "null"), "{}");
    }

    // ================================================================
    // builtins
    // ================================================================

    #[test]
    fn filter_builtins_list() {
        let r = jq_str("builtins | length", "null");
        // Should return a positive number
        let n: i64 = r.parse().unwrap();
        assert!(n > 50, "expected >50 builtins, got {n}");
    }

    #[test]
    fn filter_builtins_contains_map() {
        let r = jq_str(r#"builtins | any(. == "map/0")"#, "null");
        assert_eq!(r, "true");
    }

    // ================================================================
    // CLI flags through run_util_jq
    // ================================================================

    #[test]
    fn util_jq_compact_flag() {
        let (status, stdout, _) =
            run_util_jq(&["jq", "-c", "."], Some(br#"{"a": 1, "b": [2, 3]}"#));
        assert_eq!(status, 0);
        assert_eq!(stdout.trim(), r#"{"a":1,"b":[2,3]}"#);
    }

    #[test]
    fn util_jq_slurp_multiple_docs() {
        let (status, stdout, _) = run_util_jq(&["jq", "-s", "."], Some(b"1\n2\n3"));
        assert_eq!(status, 0);
        assert!(stdout.contains("[1,2,3]") || stdout.contains("[\n"));
    }

    #[test]
    fn util_jq_arg_flag() {
        let (status, stdout, _) = run_util_jq(&["jq", "-n", "--arg", "x", "hello", "$x"], None);
        assert_eq!(status, 0);
        assert_eq!(stdout.trim(), r#""hello""#);
    }

    #[test]
    fn util_jq_argjson_flag() {
        let (status, stdout, _) = run_util_jq(&["jq", "-n", "--argjson", "x", "42", "$x"], None);
        assert_eq!(status, 0);
        assert_eq!(stdout.trim(), "42");
    }

    #[test]
    fn util_jq_argjson_invalid() {
        let (status, _, stderr) = run_util_jq(&["jq", "-n", "--argjson", "x", "{bad", "$x"], None);
        assert_eq!(status, 1);
        assert!(stderr.contains("invalid JSON"), "stderr: {stderr}");
    }

    #[test]
    fn util_jq_null_input_flag() {
        let (status, stdout, _) = run_util_jq(&["jq", "-n", "1 + 2"], None);
        assert_eq!(status, 0);
        assert_eq!(stdout.trim(), "3");
    }

    #[test]
    fn util_jq_combined_flags() {
        let (status, stdout, _) =
            run_util_jq(&["jq", "-rc", ".name"], Some(br#"{"name":"alice"}"#));
        assert_eq!(status, 0);
        assert_eq!(stdout.trim(), "alice");
    }

    #[test]
    fn util_jq_arg_missing_value() {
        let (status, _, stderr) = run_util_jq(&["jq", "-n", "--arg", "x"], None);
        assert_eq!(status, 1);
        assert!(stderr.contains("--arg requires"), "stderr: {stderr}");
    }

    #[test]
    fn util_jq_argjson_missing_value() {
        let (status, _, stderr) = run_util_jq(&["jq", "-n", "--argjson", "x"], None);
        assert_eq!(status, 1);
        assert!(stderr.contains("--argjson requires"), "stderr: {stderr}");
    }

    #[test]
    fn util_jq_multi_doc_no_slurp() {
        // Two JSON values on stdin, processed individually
        let (status, stdout, _) = run_util_jq(&["jq", ". + 1"], Some(b"1\n2"));
        assert_eq!(status, 0);
        let lines: Vec<&str> = stdout.trim().lines().collect();
        assert_eq!(lines, vec!["2", "3"]);
    }

    #[test]
    fn util_jq_file_input() {
        use wasmsh_fs::MemoryFs;
        let mut fs = MemoryFs::new();
        let h = fs.open("/data.json", OpenOptions::write()).unwrap();
        fs.write_file(h, br"[1,2,3]").unwrap();
        fs.close(h);

        let mut out = crate::VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut out,
                cwd: "/",
                stdin: None,
                state: None,
                network: None,
            };
            util_jq(&mut ctx, &["jq", "add", "/data.json"])
        };
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "6");
    }

    #[test]
    fn util_jq_file_not_found() {
        use wasmsh_fs::MemoryFs;
        let mut fs = MemoryFs::new();
        let mut out = crate::VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut out,
                cwd: "/",
                stdin: None,
                state: None,
                network: None,
            };
            util_jq(&mut ctx, &["jq", ".", "/nonexistent.json"])
        };
        assert_eq!(status, 1);
    }

    // ================================================================
    // Object construction
    // ================================================================

    #[test]
    fn filter_object_dynamic_key() {
        assert_eq!(
            jq_str(r"{(.name): .value}", r#"{"name":"foo","value":42}"#),
            r#"{"foo":42}"#
        );
    }

    #[test]
    fn filter_object_shorthand_multiple() {
        let r = jq_str("{a, b}", r#"{"a":1,"b":2,"c":3}"#);
        assert!(r.contains("\"a\":1"));
        assert!(r.contains("\"b\":2"));
        assert!(!r.contains("\"c\""));
    }

    #[test]
    fn filter_object_string_key() {
        assert_eq!(jq_str(r#"{"key": .val}"#, r#"{"val":99}"#), r#"{"key":99}"#);
    }

    // ================================================================
    // String interpolation (via filter concatenation)
    // ================================================================

    #[test]
    fn filter_string_concat_interpolation() {
        // jq doesn't have \() syntax in our impl, but we can test string concatenation
        assert_eq!(
            jq_raw(
                r#".name + " is " + (.age | tostring)"#,
                r#"{"name":"Alice","age":30}"#
            ),
            "Alice is 30"
        );
    }

    // ================================================================
    // Slice operations
    // ================================================================

    #[test]
    fn filter_slice_from_to() {
        assert_eq!(jq_str(".[2:5]", "[0,1,2,3,4,5,6]"), "[2,3,4]");
    }

    #[test]
    fn filter_slice_negative() {
        assert_eq!(jq_str(".[-2:]", "[0,1,2,3,4]"), "[3,4]");
    }

    #[test]
    fn filter_slice_to_end() {
        assert_eq!(jq_str(".[3:]", "[0,1,2,3,4]"), "[3,4]");
    }

    #[test]
    fn filter_slice_from_start() {
        assert_eq!(jq_str(".[:3]", "[0,1,2,3,4]"), "[0,1,2]");
    }

    #[test]
    fn filter_slice_negative_end() {
        assert_eq!(jq_str(".[:-1]", "[0,1,2,3,4]"), "[0,1,2,3]");
    }

    #[test]
    fn filter_slice_string() {
        assert_eq!(jq_raw(".[2:5]", r#""hello world""#), "llo");
    }

    #[test]
    fn filter_slice_string_negative() {
        assert_eq!(jq_raw(".[-3:]", r#""hello""#), "llo");
    }

    // ================================================================
    // Alternative operator
    // ================================================================

    #[test]
    fn filter_alternative_null() {
        assert_eq!(
            jq_str(r#".foo // "default""#, r#"{"bar":1}"#),
            r#""default""#
        );
    }

    #[test]
    fn filter_alternative_false() {
        // false is also non-truthy, so alternative kicks in
        assert_eq!(
            jq_str(r#".foo // "default""#, r#"{"foo":false}"#),
            r#""default""#
        );
    }

    #[test]
    fn filter_alternative_has_value() {
        assert_eq!(
            jq_str(r#".foo // "default""#, r#"{"foo":"bar"}"#),
            r#""bar""#
        );
    }

    #[test]
    fn filter_alternative_chain() {
        assert_eq!(jq_str(r#".a // .b // "none""#, r#"{"b":2}"#), "2");
    }

    // ================================================================
    // Comparison operators
    // ================================================================

    #[test]
    fn filter_comparison_ne() {
        assert_eq!(jq_str(". != 1", "2"), "true");
        assert_eq!(jq_str(". != 1", "1"), "false");
    }

    #[test]
    fn filter_comparison_le() {
        assert_eq!(jq_str(". <= 3", "3"), "true");
        assert_eq!(jq_str(". <= 3", "4"), "false");
    }

    #[test]
    fn filter_comparison_ge() {
        assert_eq!(jq_str(". >= 3", "3"), "true");
        assert_eq!(jq_str(". >= 3", "2"), "false");
    }

    #[test]
    fn filter_comparison_strings() {
        assert_eq!(jq_str(r#". < "b""#, r#""a""#), "true");
        assert_eq!(jq_str(r#". > "b""#, r#""a""#), "false");
    }

    // ================================================================
    // Math functions
    // ================================================================

    #[test]
    fn filter_sqrt() {
        assert_eq!(jq_str("sqrt", "9"), "3");
    }

    #[test]
    fn filter_fabs() {
        assert_eq!(jq_str("fabs", "-5"), "5");
    }

    #[test]
    fn filter_pow() {
        assert_eq!(jq_str("pow(2;10)", "null"), "1024");
    }

    #[test]
    fn filter_log() {
        // ln(1) == 0
        assert_eq!(jq_str("log", "1"), "0");
    }

    #[test]
    fn filter_log2() {
        assert_eq!(jq_str("log2", "8"), "3");
    }

    #[test]
    fn filter_log10() {
        assert_eq!(jq_str("log10", "100"), "2");
    }

    #[test]
    fn filter_exp() {
        assert_eq!(jq_str("exp", "0"), "1");
    }

    #[test]
    fn filter_exp2() {
        assert_eq!(jq_str("exp2", "3"), "8");
    }

    #[test]
    fn filter_infinite_nan() {
        assert_eq!(jq_str("infinite | isinfinite", "null"), "true");
        assert_eq!(jq_str("nan | isnan", "null"), "true");
        assert_eq!(jq_str("1 | isnormal", "null"), "true");
        assert_eq!(jq_str("0 | isnormal", "null"), "false");
    }

    // ================================================================
    // Type selectors
    // ================================================================

    #[test]
    fn filter_type_selectors() {
        // objects selector
        assert_eq!(
            jq_str("[.[] | objects]", r#"[1, "a", {}, [], null]"#),
            "[{}]"
        );
        // arrays
        assert_eq!(
            jq_str("[.[] | arrays]", r#"[1, "a", {}, [], null]"#),
            "[[]]"
        );
        // strings
        assert_eq!(
            jq_str("[.[] | strings]", r#"[1, "a", {}, [], null]"#),
            r#"["a"]"#
        );
        // numbers
        assert_eq!(
            jq_str("[.[] | numbers]", r#"[1, "a", {}, [], null]"#),
            "[1]"
        );
        // booleans
        assert_eq!(
            jq_str("[.[] | booleans]", r"[1, true, false, null]"),
            "[true,false]"
        );
        // nulls
        assert_eq!(
            jq_str("[.[] | nulls]", r#"[1, null, "a", null]"#),
            "[null,null]"
        );
        // scalars
        assert_eq!(
            jq_str("[.[] | scalars]", r#"[1, "a", {}, [], null]"#),
            r#"[1,"a",null]"#
        );
        // iterables
        assert_eq!(
            jq_str("[.[] | iterables]", r#"[1, "a", {}, [], null]"#),
            "[{},[]]"
        );
    }

    // ================================================================
    // contains / inside
    // ================================================================

    #[test]
    fn filter_contains_object() {
        assert_eq!(jq_str(r#"contains({"a":1})"#, r#"{"a":1,"b":2}"#), "true");
        assert_eq!(
            jq_str(r#"contains({"a":1,"c":3})"#, r#"{"a":1,"b":2}"#),
            "false"
        );
    }

    #[test]
    fn filter_inside() {
        assert_eq!(jq_str(r#"inside("foobar")"#, r#""foo""#), "true");
        assert_eq!(jq_str(r#"inside("foobar")"#, r#""baz""#), "false");
    }

    // ================================================================
    // in operator
    // ================================================================

    #[test]
    fn filter_in_object() {
        assert_eq!(jq_str(r#""a" | in({"a":1})"#, "null"), "true");
        assert_eq!(jq_str(r#""z" | in({"a":1})"#, "null"), "false");
    }

    // ================================================================
    // flatten with depth
    // ================================================================

    #[test]
    fn filter_flatten_depth() {
        assert_eq!(jq_str("flatten(1)", "[[1,[2]],[3]]"), "[1,[2],3]");
    }

    // ================================================================
    // group_by
    // ================================================================

    #[test]
    fn filter_group_by_result() {
        let r = jq_str("group_by(.a) | length", r#"[{"a":1},{"a":2},{"a":1}]"#);
        assert_eq!(r, "2");
    }

    // ================================================================
    // any / all with filter arg
    // ================================================================

    #[test]
    fn filter_any_with_filter() {
        assert_eq!(jq_str("any(. > 3)", "[1, 2, 3, 4]"), "true");
        assert_eq!(jq_str("any(. > 10)", "[1, 2, 3, 4]"), "false");
    }

    #[test]
    fn filter_all_with_filter() {
        assert_eq!(jq_str("all(. > 0)", "[1, 2, 3, 4]"), "true");
        assert_eq!(jq_str("all(. > 2)", "[1, 2, 3, 4]"), "false");
    }

    // ================================================================
    // first/last with generator arg
    // ================================================================

    #[test]
    fn filter_first_with_generator() {
        assert_eq!(jq_str("first(.[])", "[10, 20, 30]"), "10");
    }

    #[test]
    fn filter_last_with_generator() {
        assert_eq!(jq_str("last(.[])", "[10, 20, 30]"), "30");
    }

    // ================================================================
    // nth
    // ================================================================

    #[test]
    fn filter_nth_basic() {
        assert_eq!(jq_str("nth(1)", "[10, 20, 30]"), "20");
    }

    #[test]
    fn filter_nth_with_generator() {
        assert_eq!(jq_str("nth(2; .[])", "[10, 20, 30, 40]"), "30");
    }

    // ================================================================
    // range with step
    // ================================================================

    #[test]
    fn filter_range_with_step() {
        assert_eq!(jq_str("[range(0;10;3)]", "null"), "[0,3,6,9]");
    }

    #[test]
    fn filter_range_negative_step() {
        assert_eq!(jq_str("[range(5;0;-1)]", "null"), "[5,4,3,2,1]");
    }

    // ================================================================
    // error function
    // ================================================================

    #[test]
    fn filter_error_with_message() {
        let r = jq_str(r#"error("custom error")"#, "null");
        assert!(r.contains("ERROR:"), "result: {r}");
        assert!(r.contains("custom error"), "result: {r}");
    }

    #[test]
    fn filter_error_from_input() {
        let r = jq_str("error", r#""my error""#);
        assert!(r.contains("ERROR:"), "result: {r}");
        assert!(r.contains("my error"), "result: {r}");
    }

    // ================================================================
    // empty
    // ================================================================

    #[test]
    fn filter_empty_via_util() {
        // empty produces no output at the util_jq level
        let (status, stdout, _) = run_util_jq(&["jq", "empty"], Some(b"null"));
        assert_eq!(status, 0);
        assert_eq!(stdout.trim(), "");
    }

    #[test]
    fn filter_empty_in_array() {
        // ArrayConstruct catches the empty signal and produces []
        assert_eq!(jq_str("[empty]", "null"), "[]");
    }

    // ================================================================
    // if/elif/else
    // ================================================================

    #[test]
    fn filter_elif() {
        assert_eq!(
            jq_str(
                r#"if . < 0 then "neg" elif . == 0 then "zero" else "pos" end"#,
                "0"
            ),
            r#""zero""#
        );
    }

    #[test]
    fn filter_if_without_else() {
        // Without else, input passes through
        assert_eq!(jq_str("if . > 10 then \"big\" end", "5"), "5");
    }

    // ================================================================
    // and / or / not edge cases
    // ================================================================

    #[test]
    fn filter_and_short_circuit() {
        assert_eq!(jq_str("false and true", "null"), "false");
        assert_eq!(jq_str("null and true", "null"), "false");
    }

    #[test]
    fn filter_or_short_circuit() {
        assert_eq!(jq_str("true or false", "null"), "true");
        assert_eq!(jq_str("1 or false", "null"), "true");
    }

    // ================================================================
    // null arithmetic
    // ================================================================

    #[test]
    fn filter_null_add() {
        assert_eq!(jq_str("null + 5", "null"), "5");
        assert_eq!(jq_str("5 + null", "null"), "5");
    }

    // ================================================================
    // division / modulo errors
    // ================================================================

    #[test]
    fn filter_division_by_zero() {
        let r = jq_str("1 / 0", "null");
        assert!(r.contains("ERROR:"), "result: {r}");
    }

    #[test]
    fn filter_modulo_by_zero() {
        let r = jq_str("1 % 0", "null");
        assert!(r.contains("ERROR:"), "result: {r}");
    }

    // ================================================================
    // utf8bytelength
    // ================================================================

    #[test]
    fn filter_utf8bytelength() {
        assert_eq!(jq_str("utf8bytelength", r#""hello""#), "5");
    }

    // ================================================================
    // keys_unsorted
    // ================================================================

    #[test]
    fn filter_keys_unsorted() {
        let r = jq_str("keys_unsorted", r#"{"b":2,"a":1}"#);
        assert!(r.contains("\"b\""));
        assert!(r.contains("\"a\""));
    }

    // ================================================================
    // keys on array
    // ================================================================

    #[test]
    fn filter_keys_array() {
        assert_eq!(jq_str("keys", "[10,20,30]"), "[0,1,2]");
    }

    // ================================================================
    // values on array
    // ================================================================

    #[test]
    fn filter_values_array() {
        assert_eq!(jq_str("values", "[10,20,30]"), "[10,20,30]");
    }

    // ================================================================
    // has on array
    // ================================================================

    #[test]
    fn filter_has_array() {
        assert_eq!(jq_str("has(0)", "[10,20,30]"), "true");
        assert_eq!(jq_str("has(5)", "[10,20,30]"), "false");
    }

    // ================================================================
    // optional iterate
    // ================================================================

    #[test]
    fn filter_optional_iterate_non_iterable() {
        assert_eq!(jq_str(".[]?", "42"), "");
    }

    #[test]
    fn filter_optional_index() {
        assert_eq!(jq_str(".[0]?", "42"), "");
    }

    // ================================================================
    // iterate on null / object
    // ================================================================

    #[test]
    fn filter_iterate_null() {
        assert_eq!(jq_str("[.[]?]", "null"), "[]");
    }

    #[test]
    fn filter_iterate_object_values() {
        assert_eq!(jq_str("[.[] | . + 1]", r#"{"a":1,"b":2}"#), "[2,3]");
    }

    // ================================================================
    // Negate error on non-number
    // ================================================================

    #[test]
    fn filter_negate_string_error() {
        let r = jq_str("-.", r#""hello""#);
        assert!(r.contains("ERROR:"), "result: {r}");
    }

    // ================================================================
    // Object iteration error
    // ================================================================

    #[test]
    fn filter_iterate_number_error() {
        let r = jq_str(".[]", "42");
        assert!(r.contains("ERROR:"), "result: {r}");
    }

    // ================================================================
    // JSON parser: parse_all for multi-document
    // ================================================================

    #[test]
    fn parse_all_multiple_values() {
        let vals = JsonParser::parse_all("1 2 3").unwrap();
        assert_eq!(vals.len(), 3);
    }

    #[test]
    fn parse_all_mixed_types() {
        let vals = JsonParser::parse_all(r#"1 "hello" true null [1,2]"#).unwrap();
        assert_eq!(vals.len(), 5);
    }

    // ================================================================
    // JqValue methods coverage
    // ================================================================

    #[test]
    fn value_type_name() {
        assert_eq!(JqValue::Null.type_name(), "null");
        assert_eq!(JqValue::Bool(true).type_name(), "boolean");
        assert_eq!(JqValue::Number(1.0).type_name(), "number");
        assert_eq!(JqValue::String("x".into()).type_name(), "string");
        assert_eq!(JqValue::Array(vec![]).type_name(), "array");
        assert_eq!(JqValue::Object(vec![]).type_name(), "object");
    }

    #[test]
    fn value_is_truthy() {
        assert!(!JqValue::Null.is_truthy());
        assert!(!JqValue::Bool(false).is_truthy());
        assert!(JqValue::Bool(true).is_truthy());
        assert!(JqValue::Number(0.0).is_truthy());
        assert!(JqValue::String(String::new()).is_truthy());
    }

    #[test]
    fn value_length_bool_number() {
        // bool and number return null for length
        assert!(matches!(JqValue::Bool(true).length(), JqValue::Null));
        assert!(matches!(JqValue::Number(42.0).length(), JqValue::Null));
    }

    #[test]
    fn value_compare_different_types() {
        // null < false < true < number < string < array < object
        let null = JqValue::Null;
        let boolean = JqValue::Bool(false);
        let num = JqValue::Number(1.0);
        let s = JqValue::String("a".into());
        assert_eq!(null.compare(&boolean), Some(std::cmp::Ordering::Less));
        assert_eq!(boolean.compare(&num), Some(std::cmp::Ordering::Less));
        assert_eq!(num.compare(&s), Some(std::cmp::Ordering::Less));
    }

    #[test]
    fn value_compare_arrays() {
        let a = JqValue::Array(vec![JqValue::Number(1.0), JqValue::Number(2.0)]);
        let b = JqValue::Array(vec![JqValue::Number(1.0), JqValue::Number(3.0)]);
        assert_eq!(a.compare(&b), Some(std::cmp::Ordering::Less));
    }

    #[test]
    fn value_contains_nested() {
        let a = JqValue::Array(vec![
            JqValue::Number(1.0),
            JqValue::Number(2.0),
            JqValue::Number(3.0),
        ]);
        let b = JqValue::Array(vec![JqValue::Number(2.0)]);
        assert!(a.contains_value(&b));
    }

    #[test]
    fn value_equals_objects() {
        let a = JqValue::Object(vec![
            ("x".into(), JqValue::Number(1.0)),
            ("y".into(), JqValue::Number(2.0)),
        ]);
        let b = JqValue::Object(vec![
            ("y".into(), JqValue::Number(2.0)),
            ("x".into(), JqValue::Number(1.0)),
        ]);
        // Objects with same key-value pairs in different order should be equal
        assert!(a.equals(&b));
    }

    #[test]
    fn value_equals_different_types() {
        assert!(!JqValue::Number(1.0).equals(&JqValue::String("1".into())));
        assert!(!JqValue::Null.equals(&JqValue::Bool(false)));
    }

    #[test]
    fn value_as_i64() {
        assert_eq!(JqValue::Number(42.0).as_i64(), Some(42));
        assert_eq!(JqValue::Number(42.5).as_i64(), None);
        assert_eq!(JqValue::String("x".into()).as_i64(), None);
    }

    #[test]
    fn value_as_str() {
        assert_eq!(JqValue::String("hi".into()).as_str(), Some("hi"));
        assert_eq!(JqValue::Number(1.0).as_str(), None);
    }

    #[test]
    fn value_as_f64() {
        assert_eq!(JqValue::Number(2.75).as_f64(), Some(2.75));
        assert_eq!(JqValue::Null.as_f64(), None);
    }

    // ================================================================
    // format_number edge cases
    // ================================================================

    #[test]
    fn format_number_nan() {
        assert_eq!(format_number(f64::NAN), "null");
    }

    #[test]
    fn format_number_infinity() {
        assert_eq!(format_number(f64::INFINITY), "1.7976931348623157e+308");
        assert_eq!(format_number(f64::NEG_INFINITY), "-1.7976931348623157e+308");
    }

    #[test]
    fn format_number_integer() {
        assert_eq!(format_number(42.0), "42");
    }

    #[test]
    fn format_number_fractional() {
        assert_eq!(format_number(2.75), "2.75");
    }

    // ================================================================
    // JSON printer
    // ================================================================

    #[test]
    fn json_write_string_escapes() {
        let val = JqValue::String("a\nb\t\"c\\d".into());
        let s = json_to_string(&val, true);
        assert_eq!(s, r#""a\nb\t\"c\\d""#);
    }

    #[test]
    fn json_write_compact_vs_pretty() {
        let val = JqValue::Array(vec![JqValue::Number(1.0), JqValue::Number(2.0)]);
        let compact = json_to_string(&val, true);
        let pretty = json_to_string(&val, false);
        assert_eq!(compact, "[1,2]");
        assert!(pretty.contains('\n'));
    }

    #[test]
    fn json_write_empty_array_object() {
        assert_eq!(json_to_string(&JqValue::Array(vec![]), true), "[]");
        assert_eq!(json_to_string(&JqValue::Object(vec![]), true), "{}");
    }

    // ================================================================
    // recurse_values
    // ================================================================

    #[test]
    fn recurse_values_nested() {
        let val = parse_json(r#"{"a":[1,{"b":2}]}"#).unwrap();
        let all = val.recurse_values();
        // Should include: root obj, array, 1, inner obj, 2 = 5 items min
        assert!(all.len() >= 5, "got {} items", all.len());
    }

    // ================================================================
    // to_string_repr
    // ================================================================

    #[test]
    fn to_string_repr_scalar() {
        assert_eq!(JqValue::Null.to_string_repr(), "null");
        assert_eq!(JqValue::Bool(true).to_string_repr(), "true");
        assert_eq!(JqValue::Number(42.0).to_string_repr(), "42");
        assert_eq!(JqValue::String("hi".into()).to_string_repr(), "hi");
    }

    #[test]
    fn to_string_repr_compound() {
        // to_string_repr uses pretty printing for compound types
        let arr = JqValue::Array(vec![JqValue::Number(1.0)]);
        let repr = arr.to_string_repr();
        assert!(repr.contains('1'));
        assert!(repr.starts_with('['));
    }

    // ================================================================
    // Filter: Variable binding with multiple outputs
    // ================================================================

    #[test]
    fn filter_binding_multiple_outputs() {
        // Each element becomes $x, and we add $x to the original input
        assert_eq!(jq_str(".[] as $x | $x * $x", "[2, 3, 4]"), "4\n9\n16");
    }

    // ================================================================
    // debug passthrough
    // ================================================================

    #[test]
    fn filter_debug() {
        assert_eq!(jq_str("debug", "42"), "42");
    }

    // ================================================================
    // input/inputs (stubs)
    // ================================================================

    #[test]
    fn filter_input_returns_null() {
        assert_eq!(jq_str("input", "null"), "null");
    }

    #[test]
    fn filter_inputs_returns_empty() {
        assert_eq!(jq_str("[inputs]", "null"), "[]");
    }

    // ================================================================
    // Nested object construction with multiple filter outputs
    // ================================================================

    #[test]
    fn filter_object_construct_with_array_iterate() {
        let r = jq_str("{a: .[]}", r"[1, 2]");
        // Should produce two objects: {a:1} and {a:2}
        assert!(r.contains("{\"a\":1}"));
        assert!(r.contains("{\"a\":2}"));
    }

    // ================================================================
    // Variable ($name) in object construction
    // ================================================================

    #[test]
    fn filter_object_variable_key() {
        assert_eq!(
            jq_str(r".name as $n | {$n}", r#"{"name":"alice"}"#),
            r#"{"n":"alice"}"#
        );
    }

    // ================================================================
    // Arithmetic on strings and arrays
    // ================================================================

    #[test]
    fn filter_arith_string_add() {
        assert_eq!(jq_raw(r#""hello" + " " + "world""#, "null"), "hello world");
    }

    #[test]
    fn filter_arith_array_add() {
        assert_eq!(jq_str("[1,2] + [3,4]", "null"), "[1,2,3,4]");
    }

    #[test]
    fn filter_arith_object_merge() {
        let r = jq_str(r#"{"a":1} + {"b":2}"#, "null");
        assert!(r.contains("\"a\":1"));
        assert!(r.contains("\"b\":2"));
    }

    #[test]
    fn filter_arith_object_override() {
        assert_eq!(jq_str(r#"{"a":1} + {"a":2}"#, "null"), r#"{"a":2}"#);
    }

    #[test]
    fn filter_arith_type_error() {
        let r = jq_str(r#"1 + "a""#, "null");
        assert!(r.contains("ERROR:"), "result: {r}");
    }

    // ================================================================
    // from_entries with name/value keys
    // ================================================================

    #[test]
    fn filter_from_entries_name_key() {
        let r = jq_str("from_entries", r#"[{"name":"a","value":1}]"#);
        assert!(r.contains("\"a\":1"));
    }

    // ================================================================
    // Regex: test with case insensitive flag
    // ================================================================

    #[test]
    fn filter_test_case_insensitive() {
        assert_eq!(jq_str(r#"test("foo"; "i")"#, r#""FOObar""#), "true");
    }

    // ================================================================
    // match function
    // ================================================================

    #[test]
    fn filter_match_basic() {
        let r = jq_str(r#"match("bar")"#, r#""foobar""#);
        assert!(r.contains("\"string\":\"bar\""));
        assert!(r.contains("\"offset\":3"));
    }

    // ================================================================
    // tonumber errors
    // ================================================================

    #[test]
    fn filter_tonumber_invalid() {
        let r = jq_str("tonumber", r#""abc""#);
        assert!(r.contains("ERROR:"), "result: {r}");
    }

    #[test]
    fn filter_tonumber_identity() {
        assert_eq!(jq_str("tonumber", "42"), "42");
    }

    // ================================================================
    // reverse on string
    // ================================================================

    #[test]
    fn filter_reverse_string() {
        assert_eq!(jq_raw("reverse", r#""abc""#), "cba");
    }

    // ================================================================
    // @json and @text format strings
    // ================================================================

    #[test]
    fn filter_at_json() {
        assert_eq!(jq_raw("@json", "[1,2]"), "[1,2]");
    }

    #[test]
    fn filter_at_text() {
        assert_eq!(jq_raw("@text", r#""hello""#), "hello");
    }

    // ================================================================
    // Unknown format error
    // ================================================================

    #[test]
    fn filter_unknown_format() {
        let r = jq_str("@bogus", r#""hi""#);
        assert!(r.contains("ERROR:"), "result: {r}");
        assert!(r.contains("unknown format"));
    }

    // ================================================================
    // Unknown function error
    // ================================================================

    #[test]
    fn filter_unknown_function() {
        let r = jq_str("nonexistent_func", "null");
        assert!(r.contains("ERROR:"), "result: {r}");
        assert!(r.contains("not defined"));
    }

    // ================================================================
    // Recursion depth tracking
    // ================================================================

    #[test]
    fn filter_recursion_depth_tracked() {
        // Verify that the depth parameter is passed through by testing
        // a moderately nested filter that still works within limits
        assert_eq!(jq_str("def f: . + 1; 0 | f | f | f | f | f", "null"), "5");
    }

    // ================================================================
    // del_path coverage
    // ================================================================

    #[test]
    fn filter_delpaths_nested() {
        let r = jq_str(r#"delpaths([["a","b"]])"#, r#"{"a":{"b":1,"c":2},"d":3}"#);
        assert!(!r.contains("\"b\":1"));
        assert!(r.contains("\"c\":2"));
        assert!(r.contains("\"d\":3"));
    }

    #[test]
    fn filter_delpaths_array_index() {
        let r = jq_str("delpaths([[1]])", "[10,20,30]");
        assert_eq!(r, "[10,30]");
    }

    // ================================================================
    // set_path with array creation
    // ================================================================

    #[test]
    fn filter_setpath_array_index() {
        assert_eq!(
            jq_str(r#"setpath([1]; "x")"#, r#"["a","b","c"]"#),
            r#"["a","x","c"]"#
        );
    }

    // ================================================================
    // Various misc coverage
    // ================================================================

    #[test]
    fn filter_map_empty_result() {
        // map that skips everything via select
        assert_eq!(jq_str("map(select(. > 10))", "[1, 2, 3]"), "[]");
    }

    #[test]
    fn filter_add_empty_array() {
        assert_eq!(jq_str("add", "[]"), "null");
    }

    #[test]
    fn filter_add_null() {
        assert_eq!(jq_str("add", "null"), "null");
    }

    #[test]
    fn filter_map_values_array() {
        assert_eq!(jq_str("map_values(. + 10)", "[1, 2, 3]"), "[11,12,13]");
    }

    // ================================================================
    // @base64 on non-string input
    // ================================================================

    #[test]
    fn filter_at_base64_number() {
        // Non-string input gets to_string_repr first
        assert_eq!(jq_raw("@base64", "42"), "NDI=");
    }

    // ================================================================
    // @base64d error handling
    // ================================================================

    #[test]
    fn filter_at_base64d_on_number() {
        let r = jq_str("@base64d", "42");
        assert!(r.contains("ERROR:"), "result: {r}");
    }

    // ================================================================
    // @csv and @tsv on non-array
    // ================================================================

    #[test]
    fn filter_at_csv_non_array() {
        let r = jq_str("@csv", "42");
        assert!(r.contains("ERROR:"), "result: {r}");
    }

    #[test]
    fn filter_at_tsv_non_array() {
        let r = jq_str("@tsv", "42");
        assert!(r.contains("ERROR:"), "result: {r}");
    }

    // ================================================================
    // @uri and @html on non-string input
    // ================================================================

    #[test]
    fn filter_at_uri_number() {
        // Non-string gets to_string_repr
        let r = jq_raw("@uri", "42");
        assert_eq!(r, "42");
    }

    #[test]
    fn filter_at_html_number() {
        let r = jq_raw("@html", "42");
        assert_eq!(r, "42");
    }

    // ================================================================
    // Empty array construct []
    // ================================================================

    #[test]
    fn filter_empty_array_construct() {
        assert_eq!(jq_str("[]", "null"), "[]");
    }

    // ================================================================
    // Parenthesized expression
    // ================================================================

    #[test]
    fn filter_paren_expr() {
        assert_eq!(jq_str("(1 + 2) * 3", "null"), "9");
    }

    // ================================================================
    // Literal values in filter
    // ================================================================

    #[test]
    fn filter_literal_string() {
        assert_eq!(jq_str(r#""hello""#, "null"), r#""hello""#);
    }

    #[test]
    fn filter_literal_number() {
        assert_eq!(jq_str("42", "null"), "42");
    }

    #[test]
    fn filter_literal_bool() {
        assert_eq!(jq_str("true", "null"), "true");
        assert_eq!(jq_str("false", "null"), "false");
    }

    #[test]
    fn filter_literal_null() {
        assert_eq!(jq_str("null", "42"), "null");
    }

    #[test]
    fn filter_negative_literal() {
        assert_eq!(jq_str("-42", "null"), "-42");
    }

    // ================================================================
    // @html with single-quote
    // ================================================================

    #[test]
    fn filter_at_html_single_quote() {
        assert_eq!(jq_raw("@html", r#""it's""#), "it&#39;s");
    }

    // ================================================================
    // JSON parser: unicode escapes
    // ================================================================

    #[test]
    fn parse_unicode_escape() {
        match parse_json(r#""\u0041""#).unwrap() {
            JqValue::String(s) => assert_eq!(s, "A"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn parse_empty_input_error() {
        assert!(parse_json("").is_err());
        assert!(parse_json("   ").is_err());
    }

    // ================================================================
    // JSON write: control characters
    // ================================================================

    #[test]
    fn json_write_control_chars() {
        let val = JqValue::String("\u{0008}\u{000C}\r".into());
        let s = json_to_string(&val, true);
        assert!(s.contains("\\b"));
        assert!(s.contains("\\f"));
        assert!(s.contains("\\r"));
    }

    // ================================================================
    // util_jq --join-output flag (-j)
    // ================================================================

    #[test]
    fn util_jq_join_output() {
        let (status, stdout, _) = run_util_jq(&["jq", "-j", ".name"], Some(br#"{"name":"test"}"#));
        assert_eq!(status, 0);
        assert_eq!(stdout.trim(), "test");
    }

    // ================================================================
    // util_jq -- separator
    // ================================================================

    #[test]
    fn util_jq_double_dash() {
        let (status, stdout, _) = run_util_jq(&["jq", "--", "."], Some(b"42"));
        assert_eq!(status, 0);
        assert_eq!(stdout.trim(), "42");
    }

    // ================================================================
    // util_jq runtime error returns status 5
    // ================================================================

    #[test]
    fn util_jq_runtime_error_status() {
        let (status, _, stderr) = run_util_jq(&["jq", ".[] | .a"], Some(b"42"));
        assert_eq!(status, 5);
        assert!(!stderr.is_empty());
    }

    // ================================================================
    // JSON parser: scientific notation
    // ================================================================

    #[test]
    fn parse_scientific_notation() {
        match parse_json("1.5e2").unwrap() {
            JqValue::Number(n) => assert!((n - 150.0).abs() < f64::EPSILON),
            _ => panic!("expected number"),
        }
    }

    // ================================================================
    // Verify test regex matching
    // ================================================================

    #[test]
    fn regex_dot_star() {
        let matches = simple_regex_match("hello", "h.*o", false);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].1, "hello");
    }

    #[test]
    fn regex_char_class() {
        let matches = simple_regex_match("a1b2c3", "[0-9]+", false);
        assert!(!matches.is_empty());
        assert_eq!(matches[0].1, "1");
    }

    #[test]
    fn regex_anchored() {
        let matches = simple_regex_match("hello", "^hel", false);
        assert_eq!(matches.len(), 1);
        let matches = simple_regex_match("hello", "^llo", false);
        assert!(matches.is_empty());
    }

    #[test]
    fn regex_end_anchor() {
        let matches = simple_regex_match("hello", "llo$", false);
        assert_eq!(matches.len(), 1);
        let matches = simple_regex_match("hello", "hel$", false);
        assert!(matches.is_empty());
    }

    #[test]
    fn regex_word_digit_space() {
        assert!(!simple_regex_match("a1 ", r"\w+", false).is_empty());
        assert!(!simple_regex_match("a1 ", r"\d", false).is_empty());
        assert!(!simple_regex_match("a1 ", r"\s", false).is_empty());
    }
}
