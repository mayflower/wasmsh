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

// ---------------------------------------------------------------------------
// JSON parser
// ---------------------------------------------------------------------------

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
            match self.advance() {
                Some(b'"') => return Ok(s),
                Some(b'\\') => match self.advance() {
                    Some(b'"') => s.push('"'),
                    Some(b'\\') => s.push('\\'),
                    Some(b'/') => s.push('/'),
                    Some(b'n') => s.push('\n'),
                    Some(b't') => s.push('\t'),
                    Some(b'r') => s.push('\r'),
                    Some(b'b') => s.push('\u{0008}'),
                    Some(b'f') => s.push('\u{000C}'),
                    Some(b'u') => {
                        let hex = self.take_hex(4)?;
                        let cp =
                            u32::from_str_radix(&hex, 16).map_err(|_| "invalid unicode escape")?;
                        // Handle surrogate pairs
                        if (0xD800..=0xDBFF).contains(&cp) {
                            // High surrogate, expect \uXXXX low surrogate
                            if self.advance() != Some(b'\\') || self.advance() != Some(b'u') {
                                return Err("expected low surrogate".into());
                            }
                            let hex2 = self.take_hex(4)?;
                            let cp2 = u32::from_str_radix(&hex2, 16)
                                .map_err(|_| "invalid unicode escape")?;
                            if !(0xDC00..=0xDFFF).contains(&cp2) {
                                return Err("invalid low surrogate".into());
                            }
                            let full = 0x10000 + ((cp - 0xD800) << 10) + (cp2 - 0xDC00);
                            if let Some(c) = char::from_u32(full) {
                                s.push(c);
                            } else {
                                s.push('\u{FFFD}');
                            }
                        } else if let Some(c) = char::from_u32(cp) {
                            s.push(c);
                        } else {
                            s.push('\u{FFFD}');
                        }
                    }
                    Some(c) => {
                        s.push('\\');
                        s.push(c as char);
                    }
                    None => return Err("unterminated string escape".into()),
                },
                Some(c) => s.push(c as char),
                None => return Err("unterminated string".into()),
            }
        }
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
        if self.pos < self.input.len() && self.input[self.pos] == b'-' {
            self.pos += 1;
        }
        while self.pos < self.input.len() && self.input[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if self.pos < self.input.len() && self.input[self.pos] == b'.' {
            self.pos += 1;
            while self.pos < self.input.len() && self.input[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        if self.pos < self.input.len()
            && (self.input[self.pos] == b'e' || self.input[self.pos] == b'E')
        {
            self.pos += 1;
            if self.pos < self.input.len()
                && (self.input[self.pos] == b'+' || self.input[self.pos] == b'-')
            {
                self.pos += 1;
            }
            while self.pos < self.input.len() && self.input[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        let s = std::str::from_utf8(&self.input[start..self.pos]).map_err(|_| "invalid number")?;
        let n: f64 = s.parse().map_err(|_| format!("invalid number: {s}"))?;
        Ok(JqValue::Number(n))
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
        JqValue::Array(arr) => {
            if arr.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            if !compact {
                out.push('\n');
            }
            for (i, v) in arr.iter().enumerate() {
                if !compact {
                    write_indent(out, indent + 1);
                }
                json_write(out, v, compact, indent + 1);
                if i + 1 < arr.len() {
                    out.push(',');
                }
                if !compact {
                    out.push('\n');
                }
            }
            if !compact {
                write_indent(out, indent);
            }
            out.push(']');
        }
        JqValue::Object(pairs) => {
            if pairs.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            if !compact {
                out.push('\n');
            }
            for (i, (k, v)) in pairs.iter().enumerate() {
                if !compact {
                    write_indent(out, indent + 1);
                }
                json_write_string(out, k);
                out.push(':');
                if !compact {
                    out.push(' ');
                }
                json_write(out, v, compact, indent + 1);
                if i + 1 < pairs.len() {
                    out.push(',');
                }
                if !compact {
                    out.push('\n');
                }
            }
            if !compact {
                write_indent(out, indent);
            }
            out.push('}');
        }
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
        match ch {
            b'.' => {
                self.pos += 1;
                if self.pos < self.input.len() && self.input[self.pos] == b'.' {
                    self.pos += 1;
                    Ok(Token::DotDot)
                } else {
                    Ok(Token::Dot)
                }
            }
            b'[' => {
                self.pos += 1;
                Ok(Token::LBracket)
            }
            b']' => {
                self.pos += 1;
                Ok(Token::RBracket)
            }
            b'(' => {
                self.pos += 1;
                Ok(Token::LParen)
            }
            b')' => {
                self.pos += 1;
                Ok(Token::RParen)
            }
            b'{' => {
                self.pos += 1;
                Ok(Token::LBrace)
            }
            b'}' => {
                self.pos += 1;
                Ok(Token::RBrace)
            }
            b'|' => {
                self.pos += 1;
                Ok(Token::Pipe)
            }
            b',' => {
                self.pos += 1;
                Ok(Token::Comma)
            }
            b':' => {
                self.pos += 1;
                Ok(Token::Colon)
            }
            b';' => {
                self.pos += 1;
                Ok(Token::Semi)
            }
            b'?' => {
                self.pos += 1;
                Ok(Token::Question)
            }
            b'+' => {
                self.pos += 1;
                Ok(Token::Plus)
            }
            b'-' => {
                self.pos += 1;
                Ok(Token::Minus)
            }
            b'*' => {
                self.pos += 1;
                Ok(Token::Star)
            }
            b'/' => {
                self.pos += 1;
                if self.pos < self.input.len() && self.input[self.pos] == b'/' {
                    self.pos += 1;
                    Ok(Token::Alternative)
                } else {
                    Ok(Token::Slash)
                }
            }
            b'%' => {
                self.pos += 1;
                Ok(Token::Percent)
            }
            b'=' => {
                self.pos += 1;
                if self.pos < self.input.len() && self.input[self.pos] == b'=' {
                    self.pos += 1;
                    Ok(Token::Eq)
                } else {
                    Err("unexpected '='".into())
                }
            }
            b'!' => {
                self.pos += 1;
                if self.pos < self.input.len() && self.input[self.pos] == b'=' {
                    self.pos += 1;
                    Ok(Token::Ne)
                } else {
                    Err("unexpected '!'".into())
                }
            }
            b'<' => {
                self.pos += 1;
                if self.pos < self.input.len() && self.input[self.pos] == b'=' {
                    self.pos += 1;
                    Ok(Token::Le)
                } else {
                    Ok(Token::Lt)
                }
            }
            b'>' => {
                self.pos += 1;
                if self.pos < self.input.len() && self.input[self.pos] == b'=' {
                    self.pos += 1;
                    Ok(Token::Ge)
                } else {
                    Ok(Token::Gt)
                }
            }
            b'$' => {
                self.pos += 1;
                let start = self.pos;
                while self.pos < self.input.len()
                    && (self.input[self.pos].is_ascii_alphanumeric()
                        || self.input[self.pos] == b'_')
                {
                    self.pos += 1;
                }
                let name = std::str::from_utf8(&self.input[start..self.pos]).unwrap_or("");
                Ok(Token::Variable(name.to_string()))
            }
            b'@' => {
                self.pos += 1;
                let start = self.pos;
                while self.pos < self.input.len()
                    && (self.input[self.pos].is_ascii_alphanumeric()
                        || self.input[self.pos] == b'_')
                {
                    self.pos += 1;
                }
                let name = std::str::from_utf8(&self.input[start..self.pos]).unwrap_or("");
                Ok(Token::AtFormat(name.to_string()))
            }
            b'"' => self.tokenize_string(),
            c if c.is_ascii_digit() => Ok(self.tokenize_number()),
            c if c.is_ascii_alphabetic() || c == b'_' => Ok(self.tokenize_ident()),
            _ => {
                self.pos += 1;
                Err(format!("unexpected character: '{}'", ch as char))
            }
        }
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
                b'\\' => {
                    self.pos += 1;
                    if self.pos >= self.input.len() {
                        return Err("unterminated string escape".into());
                    }
                    match self.input[self.pos] {
                        b'"' => {
                            s.push('"');
                            self.pos += 1;
                        }
                        b'\\' => {
                            s.push('\\');
                            self.pos += 1;
                        }
                        b'/' => {
                            s.push('/');
                            self.pos += 1;
                        }
                        b'n' => {
                            s.push('\n');
                            self.pos += 1;
                        }
                        b't' => {
                            s.push('\t');
                            self.pos += 1;
                        }
                        b'r' => {
                            s.push('\r');
                            self.pos += 1;
                        }
                        b'b' => {
                            s.push('\u{0008}');
                            self.pos += 1;
                        }
                        b'f' => {
                            s.push('\u{000C}');
                            self.pos += 1;
                        }
                        b'u' => {
                            self.pos += 1;
                            let mut hex = String::new();
                            for _ in 0..4 {
                                if self.pos < self.input.len() {
                                    hex.push(self.input[self.pos] as char);
                                    self.pos += 1;
                                }
                            }
                            if let Ok(cp) = u32::from_str_radix(&hex, 16) {
                                if let Some(c) = char::from_u32(cp) {
                                    s.push(c);
                                }
                            }
                        }
                        c => {
                            s.push('\\');
                            s.push(c as char);
                            self.pos += 1;
                        }
                    }
                }
                c => {
                    s.push(c as char);
                    self.pos += 1;
                }
            }
        }
    }

    fn tokenize_number(&mut self) -> Token {
        let start = self.pos;
        while self.pos < self.input.len() && self.input[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if self.pos < self.input.len() && self.input[self.pos] == b'.' {
            self.pos += 1;
            while self.pos < self.input.len() && self.input[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        if self.pos < self.input.len()
            && (self.input[self.pos] == b'e' || self.input[self.pos] == b'E')
        {
            self.pos += 1;
            if self.pos < self.input.len()
                && (self.input[self.pos] == b'+' || self.input[self.pos] == b'-')
            {
                self.pos += 1;
            }
            while self.pos < self.input.len() && self.input[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        let s = std::str::from_utf8(&self.input[start..self.pos]).unwrap_or("0");
        let n: f64 = s.parse().unwrap_or(0.0);
        Token::NumLit(n)
    }

    fn tokenize_ident(&mut self) -> Token {
        let start = self.pos;
        while self.pos < self.input.len()
            && (self.input[self.pos].is_ascii_alphanumeric() || self.input[self.pos] == b'_')
        {
            self.pos += 1;
        }
        let s = std::str::from_utf8(&self.input[start..self.pos]).unwrap_or("");
        match s {
            "and" => Token::And,
            "or" => Token::Or,
            "not" => Token::Not,
            "if" => Token::If,
            "then" => Token::Then,
            "elif" => Token::Elif,
            "else" => Token::Else,
            "end" => Token::End,
            "as" => Token::As,
            "def" => Token::Def,
            "reduce" => Token::Reduce,
            "foreach" => Token::Foreach,
            "try" => Token::Try,
            "catch" => Token::Catch,
            "label" => Token::Label,
            "true" => Token::True,
            "false" => Token::False,
            "null" => Token::Null,
            "empty" => Token::Empty,
            _ => Token::Ident(s.to_string()),
        }
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
                Token::Dot => {
                    let saved = self.pos;
                    self.advance();
                    if let Token::Ident(name) = self.peek() {
                        let name = name.clone();
                        self.advance();
                        if *self.peek() == Token::Question {
                            self.advance();
                            expr = JqFilter::Pipe(
                                Box::new(expr),
                                Box::new(JqFilter::OptionalField(name)),
                            );
                        } else {
                            expr = JqFilter::Pipe(Box::new(expr), Box::new(JqFilter::Field(name)));
                        }
                    } else {
                        self.pos = saved;
                        break;
                    }
                }
                Token::LBracket => {
                    expr = self.parse_bracket_suffix(expr)?;
                }
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
                    self.advance();
                    let var = match self.advance() {
                        Token::Variable(name) => name,
                        t => return Err(format!("expected variable after 'as', got {t:?}")),
                    };
                    self.expect(&Token::Pipe)?;
                    let body = self.parse_pipe()?;
                    expr = JqFilter::Binding {
                        expr: Box::new(expr),
                        var,
                        body: Box::new(body),
                    };
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_bracket_suffix(&mut self, base: JqFilter) -> Result<JqFilter, String> {
        self.expect(&Token::LBracket)?;
        // .[]
        if *self.peek() == Token::RBracket {
            self.advance();
            let optional = if *self.peek() == Token::Question {
                self.advance();
                true
            } else {
                false
            };
            return Ok(JqFilter::Pipe(
                Box::new(base),
                Box::new(if optional {
                    JqFilter::OptionalIterate
                } else {
                    JqFilter::Iterate
                }),
            ));
        }
        // .[:M]
        if *self.peek() == Token::Colon {
            self.advance();
            let to = self.parse_pipe()?;
            self.expect(&Token::RBracket)?;
            let optional = if *self.peek() == Token::Question {
                self.advance();
                true
            } else {
                false
            };
            let f = JqFilter::Slice(None, Some(Box::new(to)));
            let result = JqFilter::Pipe(Box::new(base), Box::new(f));
            return Ok(if optional {
                JqFilter::TryCatch {
                    try_: Box::new(result),
                    catch: None,
                }
            } else {
                result
            });
        }
        let idx = self.parse_pipe()?;
        if *self.peek() == Token::Colon {
            // .[expr:expr?]
            self.advance();
            let to = if *self.peek() == Token::RBracket {
                None
            } else {
                Some(Box::new(self.parse_pipe()?))
            };
            self.expect(&Token::RBracket)?;
            let optional = if *self.peek() == Token::Question {
                self.advance();
                true
            } else {
                false
            };
            let f = JqFilter::Slice(Some(Box::new(idx)), to);
            let result = JqFilter::Pipe(Box::new(base), Box::new(f));
            return Ok(if optional {
                JqFilter::TryCatch {
                    try_: Box::new(result),
                    catch: None,
                }
            } else {
                result
            });
        }
        self.expect(&Token::RBracket)?;
        let optional = if *self.peek() == Token::Question {
            self.advance();
            true
        } else {
            false
        };
        if optional {
            Ok(JqFilter::Pipe(
                Box::new(base),
                Box::new(JqFilter::OptionalIndex(Box::new(idx))),
            ))
        } else {
            Ok(JqFilter::Pipe(
                Box::new(base),
                Box::new(JqFilter::Index(Box::new(idx))),
            ))
        }
    }

    #[allow(clippy::too_many_lines)]
    fn parse_primary(&mut self) -> Result<JqFilter, String> {
        match self.peek().clone() {
            Token::Dot => {
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
            Token::DotDot => {
                self.advance();
                Ok(JqFilter::Recurse)
            }
            Token::LBracket => {
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
            Token::LBrace => {
                self.advance();
                self.parse_object_construct()
            }
            Token::LParen => {
                self.advance();
                let inner = self.parse_pipe()?;
                self.expect(&Token::RParen)?;
                Ok(inner)
            }
            Token::StrLit(s) => {
                self.advance();
                Ok(JqFilter::Literal(JqValue::String(s)))
            }
            Token::NumLit(n) => {
                self.advance();
                Ok(JqFilter::Literal(JqValue::Number(n)))
            }
            Token::True => {
                self.advance();
                Ok(JqFilter::Literal(JqValue::Bool(true)))
            }
            Token::False => {
                self.advance();
                Ok(JqFilter::Literal(JqValue::Bool(false)))
            }
            Token::Null => {
                self.advance();
                Ok(JqFilter::Literal(JqValue::Null))
            }
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
            Token::Try => {
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
            Token::Label => {
                self.advance();
                let var = match self.advance() {
                    Token::Variable(name) => name,
                    t => return Err(format!("expected variable after 'label', got {t:?}")),
                };
                self.expect(&Token::Pipe)?;
                let body = self.parse_pipe()?;
                Ok(JqFilter::Label(var, Box::new(body)))
            }
            Token::Minus => {
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
            Token::AtFormat(name) => {
                self.advance();
                Ok(JqFilter::Format(name))
            }
            Token::Ident(name) => {
                self.advance();
                if *self.peek() == Token::LParen {
                    self.advance();
                    let mut args = Vec::new();
                    if *self.peek() != Token::RParen {
                        args.push(self.parse_pipe()?);
                        while *self.peek() == Token::Semi {
                            self.advance();
                            args.push(self.parse_pipe()?);
                        }
                    }
                    self.expect(&Token::RParen)?;
                    Ok(JqFilter::FuncCall(name, args))
                } else {
                    Ok(JqFilter::FuncCall(name, vec![]))
                }
            }
            t => Err(format!("unexpected token: {t:?}")),
        }
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
                Token::Dot => {
                    let saved = self.pos;
                    self.advance();
                    if let Token::Ident(name) = self.peek() {
                        let name = name.clone();
                        self.advance();
                        if *self.peek() == Token::Question {
                            self.advance();
                            expr = JqFilter::Pipe(
                                Box::new(expr),
                                Box::new(JqFilter::OptionalField(name)),
                            );
                        } else {
                            expr = JqFilter::Pipe(Box::new(expr), Box::new(JqFilter::Field(name)));
                        }
                    } else {
                        self.pos = saved;
                        break;
                    }
                }
                Token::LBracket => {
                    expr = self.parse_bracket_suffix(expr)?;
                }
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
                if *self.peek() == Token::Colon {
                    self.advance();
                    let val = self.parse_alternative()?;
                    Ok((JqObjKey::Ident(name), Some(val)))
                } else {
                    // Shorthand: {name} means {name: .name}
                    Ok((JqObjKey::Ident(name), None))
                }
            }
            Token::StrLit(s) => {
                self.advance();
                if *self.peek() == Token::Colon {
                    self.advance();
                    let val = self.parse_alternative()?;
                    Ok((JqObjKey::Ident(s), Some(val)))
                } else {
                    Ok((JqObjKey::Ident(s), None))
                }
            }
            Token::Variable(name) => {
                self.advance();
                if *self.peek() == Token::Colon {
                    self.advance();
                    let val = self.parse_alternative()?;
                    Ok((JqObjKey::Ident(name.clone()), Some(val)))
                } else {
                    Ok((
                        JqObjKey::Ident(name.clone()),
                        Some(JqFilter::Variable(name)),
                    ))
                }
            }
            Token::LParen => {
                self.advance();
                let key_expr = self.parse_pipe()?;
                self.expect(&Token::RParen)?;
                self.expect(&Token::Colon)?;
                let val = self.parse_alternative()?;
                Ok((JqObjKey::Dynamic(key_expr), Some(val)))
            }
            Token::AtFormat(name) => {
                self.advance();
                if *self.peek() == Token::Colon {
                    self.advance();
                    let val = self.parse_alternative()?;
                    Ok((JqObjKey::Ident(format!("@{name}")), Some(val)))
                } else {
                    Ok((JqObjKey::Ident(format!("@{name}")), None))
                }
            }
            t => Err(format!("expected object key, got {t:?}")),
        }
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

#[allow(clippy::too_many_lines)]
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
        JqFilter::Identity => Ok(vec![input.clone()]),

        JqFilter::Field(name) => Ok(vec![field_access(input, name)]),

        JqFilter::OptionalField(name) => match input {
            JqValue::Object(_) | JqValue::Null => Ok(vec![field_access(input, name)]),
            _ => Ok(vec![]),
        },

        JqFilter::Index(idx_filter) => {
            let indices = apply_filter(idx_filter, input, env, depth + 1)?;
            let mut out = Vec::new();
            for idx in &indices {
                out.push(index_access(input, idx)?);
            }
            Ok(out)
        }

        JqFilter::OptionalIndex(idx_filter) => {
            let indices = apply_filter(idx_filter, input, env, depth + 1)?;
            let mut out = Vec::new();
            for idx in &indices {
                if let Ok(v) = index_access(input, idx) {
                    out.push(v);
                }
            }
            Ok(out)
        }

        JqFilter::Slice(from, to) => {
            let from_val = match from {
                Some(f) => {
                    let vals = apply_filter(f, input, env, depth + 1)?;
                    vals.first().and_then(JqValue::as_i64).unwrap_or(0)
                }
                None => 0,
            };
            let arr = match input {
                JqValue::Array(a) => a,
                JqValue::String(s) => {
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
                    return Ok(vec![JqValue::String(sliced)]);
                }
                _ => return Err(format!("cannot slice {}", input.type_name())),
            };
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

        JqFilter::Iterate => match input {
            JqValue::Array(arr) => Ok(arr.clone()),
            JqValue::Object(pairs) => Ok(pairs.iter().map(|(_, v)| v.clone()).collect()),
            JqValue::Null => Ok(vec![]),
            _ => Err(format!("cannot iterate over {}", input.type_name())),
        },

        JqFilter::OptionalIterate => match input {
            JqValue::Array(arr) => Ok(arr.clone()),
            JqValue::Object(pairs) => Ok(pairs.iter().map(|(_, v)| v.clone()).collect()),
            _ => Ok(vec![]),
        },

        JqFilter::Pipe(left, right) => {
            let left_vals = apply_filter(left, input, env, depth + 1)?;
            let mut out = Vec::new();
            for v in &left_vals {
                let right_vals = apply_filter(right, v, env, depth + 1)?;
                out.extend(right_vals);
            }
            Ok(out)
        }

        JqFilter::Comma(left, right) => {
            let mut out = apply_filter(left, input, env, depth + 1)?;
            out.extend(apply_filter(right, input, env, depth + 1)?);
            Ok(out)
        }

        JqFilter::Literal(val) => Ok(vec![val.clone()]),

        JqFilter::Comparison(left, op, right) => {
            let lvals = apply_filter(left, input, env, depth + 1)?;
            let rvals = apply_filter(right, input, env, depth + 1)?;
            let mut out = Vec::new();
            for lv in &lvals {
                for rv in &rvals {
                    let result = match op {
                        CompOp::Eq => lv.equals(rv),
                        CompOp::Ne => !lv.equals(rv),
                        CompOp::Lt => lv.compare(rv) == Some(std::cmp::Ordering::Less),
                        CompOp::Le => {
                            matches!(
                                lv.compare(rv),
                                Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                            )
                        }
                        CompOp::Gt => lv.compare(rv) == Some(std::cmp::Ordering::Greater),
                        CompOp::Ge => {
                            matches!(
                                lv.compare(rv),
                                Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                            )
                        }
                    };
                    out.push(JqValue::Bool(result));
                }
            }
            Ok(out)
        }

        JqFilter::Arithmetic(left, op, right) => {
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

        JqFilter::Not => Ok(vec![JqValue::Bool(!input.is_truthy())]),

        JqFilter::And(left, right) => {
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

        JqFilter::Or(left, right) => {
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

        JqFilter::If {
            cond,
            then_,
            elifs,
            else_,
        } => {
            let cond_vals = apply_filter(cond, input, env, depth + 1)?;
            let mut out = Vec::new();
            for cv in &cond_vals {
                if cv.is_truthy() {
                    out.extend(apply_filter(then_, input, env, depth + 1)?);
                } else {
                    let mut handled = false;
                    for (econd, ebody) in elifs {
                        let evals = apply_filter(econd, input, env, depth + 1)?;
                        if evals.first().is_some_and(JqValue::is_truthy) {
                            out.extend(apply_filter(ebody, input, env, depth + 1)?);
                            handled = true;
                            break;
                        }
                    }
                    if !handled {
                        if let Some(else_) = else_ {
                            out.extend(apply_filter(else_, input, env, depth + 1)?);
                        } else {
                            out.push(input.clone());
                        }
                    }
                }
            }
            Ok(out)
        }

        JqFilter::TryCatch { try_, catch } => match apply_filter(try_, input, env, depth + 1) {
            Ok(vals) => Ok(vals),
            Err(e) => {
                if let Some(catch_f) = catch {
                    let err_val = JqValue::String(e);
                    apply_filter(catch_f, &err_val, env, depth + 1)
                } else {
                    Ok(vec![])
                }
            }
        },

        JqFilter::Alternative(left, right) => {
            let lvals = apply_filter(left, input, env, depth + 1)?;
            let mut out = Vec::new();
            let mut had_truthy = false;
            for lv in &lvals {
                if lv.is_truthy() {
                    out.push(lv.clone());
                    had_truthy = true;
                }
            }
            if had_truthy {
                Ok(out)
            } else {
                apply_filter(right, input, env, depth + 1)
            }
        }

        JqFilter::Reduce {
            iter,
            var,
            init,
            update,
        } => {
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

        JqFilter::Foreach {
            iter,
            var,
            init,
            update,
            extract,
        } => {
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

        JqFilter::Binding { expr, var, body } => {
            let vals = apply_filter(expr, input, env, depth + 1)?;
            let mut out = Vec::new();
            for v in &vals {
                let new_env = env.with_var(var, v.clone());
                out.extend(apply_filter(body, input, &new_env, depth + 1)?);
            }
            Ok(out)
        }

        JqFilter::FuncDef {
            name,
            args,
            body,
            rest,
        } => {
            let new_env = env.with_func(name, args.clone(), (**body).clone());
            apply_filter(rest, input, &new_env, depth + 1)
        }

        JqFilter::FuncCall(name, args) => dispatch_func(name, args, input, env, depth),

        JqFilter::ArrayConstruct(inner) => match apply_filter(inner, input, env, depth + 1) {
            Ok(vals) => Ok(vec![JqValue::Array(vals)]),
            Err(e) if e == EMPTY_SIGNAL => Ok(vec![JqValue::Array(vec![])]),
            Err(e) => Err(e),
        },

        JqFilter::ObjectConstruct(pairs) => build_object(pairs, input, env, depth),

        JqFilter::Variable(name) => {
            if name == "ENV" {
                Ok(vec![JqValue::Object(vec![])])
            } else if let Some(val) = env.vars.get(name) {
                Ok(vec![val.clone()])
            } else {
                Err(format!("${name} is not defined"))
            }
        }

        JqFilter::Recurse => Ok(input.recurse_values()),

        JqFilter::Label(_name, body) => match apply_filter(body, input, env, depth + 1) {
            Ok(vals) => Ok(vals),
            Err(e) if e == BREAK_SIGNAL => Ok(vec![]),
            Err(e) => Err(e),
        },

        JqFilter::Negate(inner) => {
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

        JqFilter::Format(name) => apply_format(name, input),
    }
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
    match (left, op, right) {
        (JqValue::Number(a), ArithOp::Add, JqValue::Number(b)) => Ok(JqValue::Number(a + b)),
        (JqValue::Number(a), ArithOp::Sub, JqValue::Number(b)) => Ok(JqValue::Number(a - b)),
        (JqValue::Number(a), ArithOp::Mul, JqValue::Number(b)) => Ok(JqValue::Number(a * b)),
        (JqValue::Number(a), ArithOp::Div, JqValue::Number(b)) => {
            if *b == 0.0 {
                Err("division by zero".into())
            } else {
                Ok(JqValue::Number(a / b))
            }
        }
        (JqValue::Number(a), ArithOp::Mod, JqValue::Number(b)) => {
            if *b == 0.0 {
                Err("modulo by zero".into())
            } else {
                Ok(JqValue::Number(a % b))
            }
        }
        (JqValue::String(a), ArithOp::Add, JqValue::String(b)) => {
            Ok(JqValue::String(format!("{a}{b}")))
        }
        (JqValue::Array(a), ArithOp::Add, JqValue::Array(b)) => {
            let mut result = a.clone();
            result.extend(b.iter().cloned());
            Ok(JqValue::Array(result))
        }
        (JqValue::Object(a), ArithOp::Add, JqValue::Object(b)) => {
            let mut result = a.clone();
            for (k, v) in b {
                if let Some(existing) = result.iter_mut().find(|(ek, _)| ek == k) {
                    existing.1 = v.clone();
                } else {
                    result.push((k.clone(), v.clone()));
                }
            }
            Ok(JqValue::Object(result))
        }
        (JqValue::Null, ArithOp::Add, other) | (other, ArithOp::Add, JqValue::Null) => {
            Ok(other.clone())
        }
        _ => Err(format!(
            "cannot {} {} and {}",
            match op {
                ArithOp::Add => "add",
                ArithOp::Sub => "subtract",
                ArithOp::Mul => "multiply",
                ArithOp::Div => "divide",
                ArithOp::Mod => "modulo",
            },
            left.type_name(),
            right.type_name()
        )),
    }
}

fn build_object(
    pairs: &[(JqObjKey, Option<JqFilter>)],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    let mut results: Vec<Vec<(String, JqValue)>> = vec![vec![]];

    for (key, val_filter) in pairs {
        let mut new_results = Vec::new();
        match key {
            JqObjKey::Ident(name) => {
                let val = if let Some(vf) = val_filter {
                    apply_filter(vf, input, env, depth + 1)?
                } else {
                    vec![field_access(input, name)]
                };
                for existing in &results {
                    for v in &val {
                        let mut obj_pairs = existing.clone();
                        obj_pairs.push((name.clone(), v.clone()));
                        new_results.push(obj_pairs);
                    }
                }
            }
            JqObjKey::Dynamic(key_filter) => {
                let keys = apply_filter(key_filter, input, env, depth + 1)?;
                let val = if let Some(vf) = val_filter {
                    apply_filter(vf, input, env, depth + 1)?
                } else {
                    vec![input.clone()]
                };
                for existing in &results {
                    for k in &keys {
                        let key_str = match k {
                            JqValue::String(s) => s.clone(),
                            other => other.to_string_repr(),
                        };
                        for v in &val {
                            let mut obj_pairs = existing.clone();
                            obj_pairs.push((key_str.clone(), v.clone()));
                            new_results.push(obj_pairs);
                        }
                    }
                }
            }
        }
        results = new_results;
    }

    Ok(results.into_iter().map(JqValue::Object).collect())
}

// ---------------------------------------------------------------------------
// Built-in functions
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
fn dispatch_func(
    name: &str,
    args: &[JqFilter],
    input: &JqValue,
    env: &JqEnv,
    depth: usize,
) -> Result<Vec<JqValue>, String> {
    // Check user-defined functions first
    if let Some((param_names, body)) = env.funcs.get(name) {
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
        return apply_filter(&body.clone(), input, &new_env, depth + 1);
    }

    match name {
        "empty" => Err(EMPTY_SIGNAL.into()),

        "error" => {
            if let Some(arg) = args.first() {
                let vals = apply_filter(arg, input, env, depth + 1)?;
                let msg = vals.first().map_or("error".into(), JqValue::to_string_repr);
                Err(msg)
            } else {
                let msg = match input {
                    JqValue::String(s) => s.clone(),
                    _ => input.to_string_repr(),
                };
                Err(msg)
            }
        }

        "type" => Ok(vec![JqValue::String(input.type_name().to_string())]),

        "length" => Ok(vec![input.length()]),

        "utf8bytelength" => match input {
            JqValue::String(s) => Ok(vec![JqValue::Number(s.len() as f64)]),
            _ => Ok(vec![input.length()]),
        },

        "keys" | "keys_unsorted" => match input {
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
        },

        "values" => match input {
            JqValue::Object(pairs) => Ok(vec![JqValue::Array(
                pairs.iter().map(|(_, v)| v.clone()).collect(),
            )]),
            JqValue::Array(arr) => Ok(vec![JqValue::Array(arr.clone())]),
            _ => Err(format!("{} has no values", input.type_name())),
        },

        "has" => {
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

        "in" => {
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

        "contains" => {
            if args.len() != 1 {
                return Err("contains requires 1 argument".into());
            }
            let other_vals = apply_filter(&args[0], input, env, depth + 1)?;
            let other = other_vals.into_iter().next().unwrap_or(JqValue::Null);
            Ok(vec![JqValue::Bool(input.contains_value(&other))])
        }

        "inside" => {
            if args.len() != 1 {
                return Err("inside requires 1 argument".into());
            }
            let other_vals = apply_filter(&args[0], input, env, depth + 1)?;
            let other = other_vals.into_iter().next().unwrap_or(JqValue::Null);
            Ok(vec![JqValue::Bool(other.contains_value(input))])
        }

        "select" => {
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

        "map" => {
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

        "map_values" => {
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

        "add" => match input {
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
        },

        "any" | "all" => {
            let is_any = name == "any";
            match input {
                JqValue::Array(arr) => {
                    if args.is_empty() {
                        let result = if is_any {
                            arr.iter().any(JqValue::is_truthy)
                        } else {
                            arr.iter().all(JqValue::is_truthy)
                        };
                        Ok(vec![JqValue::Bool(result)])
                    } else {
                        let result = if is_any {
                            arr.iter().any(|v| {
                                apply_filter(&args[0], v, env, depth + 1)
                                    .ok()
                                    .and_then(|vals| vals.into_iter().next())
                                    .is_some_and(|v| v.is_truthy())
                            })
                        } else {
                            arr.iter().all(|v| {
                                apply_filter(&args[0], v, env, depth + 1)
                                    .ok()
                                    .and_then(|vals| vals.into_iter().next())
                                    .is_some_and(|v| v.is_truthy())
                            })
                        };
                        Ok(vec![JqValue::Bool(result)])
                    }
                }
                _ => Err(format!("cannot {name} over {}", input.type_name())),
            }
        }

        "flatten" => {
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

        "sort" => match input {
            JqValue::Array(arr) => {
                let mut sorted = arr.clone();
                sorted.sort_by(|a, b| a.compare(b).unwrap_or(std::cmp::Ordering::Equal));
                Ok(vec![JqValue::Array(sorted)])
            }
            _ => Err(format!("cannot sort {}", input.type_name())),
        },

        "sort_by" => {
            if args.len() != 1 {
                return Err("sort_by requires 1 argument".into());
            }
            match input {
                JqValue::Array(arr) => {
                    let mut items: Vec<(JqValue, JqValue)> = Vec::new();
                    for item in arr {
                        let kv = apply_filter(&args[0], item, env, depth + 1)?;
                        let k = kv.into_iter().next().unwrap_or(JqValue::Null);
                        items.push((k, item.clone()));
                    }
                    items.sort_by(|(a, _), (b, _)| {
                        a.compare(b).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    Ok(vec![JqValue::Array(
                        items.into_iter().map(|(_, v)| v).collect(),
                    )])
                }
                _ => Err(format!("cannot sort_by {}", input.type_name())),
            }
        }

        "group_by" => {
            if args.len() != 1 {
                return Err("group_by requires 1 argument".into());
            }
            match input {
                JqValue::Array(arr) => {
                    let mut items: Vec<(JqValue, JqValue)> = Vec::new();
                    for item in arr {
                        let kv = apply_filter(&args[0], item, env, depth + 1)?;
                        let k = kv.into_iter().next().unwrap_or(JqValue::Null);
                        items.push((k, item.clone()));
                    }
                    items.sort_by(|(a, _), (b, _)| {
                        a.compare(b).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    let mut groups: Vec<JqValue> = Vec::new();
                    let mut current_key: Option<JqValue> = None;
                    let mut current_group: Vec<JqValue> = Vec::new();
                    for (k, v) in items {
                        if current_key.as_ref().is_none_or(|ck| !ck.equals(&k)) {
                            if !current_group.is_empty() {
                                groups.push(JqValue::Array(std::mem::take(&mut current_group)));
                            }
                            current_key = Some(k);
                        }
                        current_group.push(v);
                    }
                    if !current_group.is_empty() {
                        groups.push(JqValue::Array(current_group));
                    }
                    Ok(vec![JqValue::Array(groups)])
                }
                _ => Err(format!("cannot group_by {}", input.type_name())),
            }
        }

        "unique" => match input {
            JqValue::Array(arr) => {
                let mut sorted = arr.clone();
                sorted.sort_by(|a, b| a.compare(b).unwrap_or(std::cmp::Ordering::Equal));
                let mut out = Vec::new();
                for item in &sorted {
                    if out.last().is_none_or(|last: &JqValue| !last.equals(item)) {
                        out.push(item.clone());
                    }
                }
                Ok(vec![JqValue::Array(out)])
            }
            _ => Err(format!("cannot unique {}", input.type_name())),
        },

        "unique_by" => {
            if args.len() != 1 {
                return Err("unique_by requires 1 argument".into());
            }
            match input {
                JqValue::Array(arr) => {
                    let mut seen: Vec<JqValue> = Vec::new();
                    let mut out = Vec::new();
                    for item in arr {
                        let kv = apply_filter(&args[0], item, env, depth + 1)?;
                        let k = kv.into_iter().next().unwrap_or(JqValue::Null);
                        if !seen.iter().any(|s| s.equals(&k)) {
                            seen.push(k);
                            out.push(item.clone());
                        }
                    }
                    Ok(vec![JqValue::Array(out)])
                }
                _ => Err(format!("cannot unique_by {}", input.type_name())),
            }
        }

        "reverse" => match input {
            JqValue::Array(arr) => {
                let mut rev = arr.clone();
                rev.reverse();
                Ok(vec![JqValue::Array(rev)])
            }
            JqValue::String(s) => Ok(vec![JqValue::String(s.chars().rev().collect())]),
            _ => Err(format!("cannot reverse {}", input.type_name())),
        },

        "first" => {
            if args.is_empty() {
                match input {
                    JqValue::Array(arr) => Ok(vec![arr.first().cloned().unwrap_or(JqValue::Null)]),
                    _ => Ok(vec![input.clone()]),
                }
            } else {
                let vals = apply_filter(&args[0], input, env, depth + 1)?;
                Ok(vec![vals.into_iter().next().unwrap_or(JqValue::Null)])
            }
        }

        "last" => {
            if args.is_empty() {
                match input {
                    JqValue::Array(arr) => Ok(vec![arr.last().cloned().unwrap_or(JqValue::Null)]),
                    _ => Ok(vec![input.clone()]),
                }
            } else {
                let vals = apply_filter(&args[0], input, env, depth + 1)?;
                Ok(vec![vals.into_iter().last().unwrap_or(JqValue::Null)])
            }
        }

        "nth" => {
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

        "range" => {
            if args.is_empty() {
                return Err("range requires at least 1 argument".into());
            }
            let first_vals = apply_filter(&args[0], input, env, depth + 1)?;
            let first_num = first_vals.first().and_then(JqValue::as_f64).unwrap_or(0.0);
            let (start, end, step) = if args.len() >= 2 {
                let second_vals = apply_filter(&args[1], input, env, depth + 1)?;
                let second_num = second_vals.first().and_then(JqValue::as_f64).unwrap_or(0.0);
                let step_val = if args.len() >= 3 {
                    let sv = apply_filter(&args[2], input, env, depth + 1)?;
                    sv.first().and_then(JqValue::as_f64).unwrap_or(1.0)
                } else {
                    1.0
                };
                (first_num, second_num, step_val)
            } else {
                (0.0, first_num, 1.0)
            };
            let mut out = Vec::new();
            if step > 0.0 {
                let mut i = start;
                while i < end {
                    out.push(JqValue::Number(i));
                    i += step;
                }
            } else if step < 0.0 {
                let mut i = start;
                while i > end {
                    out.push(JqValue::Number(i));
                    i += step;
                }
            }
            Ok(out)
        }

        "limit" => {
            if args.len() != 2 {
                return Err("limit requires 2 arguments".into());
            }
            let n_vals = apply_filter(&args[0], input, env, depth + 1)?;
            let n = n_vals.first().and_then(JqValue::as_i64).unwrap_or(0) as usize;
            let vals = apply_filter(&args[1], input, env, depth + 1)?;
            Ok(vals.into_iter().take(n).collect())
        }

        "to_entries" => match input {
            JqValue::Object(pairs) => {
                let entries: Vec<JqValue> = pairs
                    .iter()
                    .map(|(k, v)| {
                        JqValue::Object(vec![
                            ("key".into(), JqValue::String(k.clone())),
                            ("value".into(), v.clone()),
                        ])
                    })
                    .collect();
                Ok(vec![JqValue::Array(entries)])
            }
            _ => Err(format!("{} has no entries", input.type_name())),
        },

        "from_entries" => match input {
            JqValue::Array(arr) => {
                let mut pairs = Vec::new();
                for item in arr {
                    let key = match item {
                        JqValue::Object(p) => {
                            let k = p
                                .iter()
                                .find(|(k, _)| k == "key" || k == "name")
                                .map_or(JqValue::Null, |(_, v)| v.clone());
                            match k {
                                JqValue::String(s) => s,
                                JqValue::Number(n) => format_number(n),
                                _ => k.to_string_repr(),
                            }
                        }
                        _ => continue,
                    };
                    let val = match item {
                        JqValue::Object(p) => p
                            .iter()
                            .find(|(k, _)| k == "value")
                            .map_or(JqValue::Null, |(_, v)| v.clone()),
                        _ => JqValue::Null,
                    };
                    pairs.push((key, val));
                }
                Ok(vec![JqValue::Object(pairs)])
            }
            _ => Err(format!("cannot from_entries on {}", input.type_name())),
        },

        "with_entries" => {
            if args.len() != 1 {
                return Err("with_entries requires 1 argument".into());
            }
            match input {
                JqValue::Object(pairs) => {
                    let entries: Vec<JqValue> = pairs
                        .iter()
                        .map(|(k, v)| {
                            JqValue::Object(vec![
                                ("key".into(), JqValue::String(k.clone())),
                                ("value".into(), v.clone()),
                            ])
                        })
                        .collect();
                    let mut mapped = Vec::new();
                    for entry in &entries {
                        match apply_filter(&args[0], entry, env, depth + 1) {
                            Ok(vals) => mapped.extend(vals),
                            Err(e) if e == EMPTY_SIGNAL => {}
                            Err(e) => return Err(e),
                        }
                    }
                    let mut result_pairs = Vec::new();
                    for item in &mapped {
                        if let JqValue::Object(p) = item {
                            let k = p
                                .iter()
                                .find(|(k, _)| k == "key" || k == "name")
                                .map_or(JqValue::Null, |(_, v)| v.clone());
                            let key = match k {
                                JqValue::String(s) => s,
                                other => other.to_string_repr(),
                            };
                            let val = p
                                .iter()
                                .find(|(k, _)| k == "value")
                                .map_or(JqValue::Null, |(_, v)| v.clone());
                            result_pairs.push((key, val));
                        }
                    }
                    Ok(vec![JqValue::Object(result_pairs)])
                }
                _ => Err(format!("cannot with_entries on {}", input.type_name())),
            }
        }

        "indices" | "index" | "rindex" => {
            if args.len() != 1 {
                return Err(format!("{name} requires 1 argument"));
            }
            let needle_vals = apply_filter(&args[0], input, env, depth + 1)?;
            let needle = needle_vals.into_iter().next().unwrap_or(JqValue::Null);
            match (input, &needle) {
                (JqValue::String(s), JqValue::String(sub)) => {
                    let found: Vec<usize> = s.match_indices(sub.as_str()).map(|(i, _)| i).collect();
                    if name == "index" {
                        Ok(vec![found
                            .first()
                            .map_or(JqValue::Null, |i| JqValue::Number(*i as f64))])
                    } else if name == "rindex" {
                        Ok(vec![found
                            .last()
                            .map_or(JqValue::Null, |i| JqValue::Number(*i as f64))])
                    } else {
                        Ok(vec![JqValue::Array(
                            found
                                .into_iter()
                                .map(|i| JqValue::Number(i as f64))
                                .collect(),
                        )])
                    }
                }
                (JqValue::Array(arr), _) => {
                    let found: Vec<usize> = arr
                        .iter()
                        .enumerate()
                        .filter(|(_, v)| v.equals(&needle))
                        .map(|(i, _)| i)
                        .collect();
                    if name == "index" {
                        Ok(vec![found
                            .first()
                            .map_or(JqValue::Null, |i| JqValue::Number(*i as f64))])
                    } else if name == "rindex" {
                        Ok(vec![found
                            .last()
                            .map_or(JqValue::Null, |i| JqValue::Number(*i as f64))])
                    } else {
                        Ok(vec![JqValue::Array(
                            found
                                .into_iter()
                                .map(|i| JqValue::Number(i as f64))
                                .collect(),
                        )])
                    }
                }
                _ => Ok(vec![JqValue::Null]),
            }
        }

        "test" | "match" | "capture" => {
            if args.is_empty() {
                return Err(format!("{name} requires at least 1 argument"));
            }
            let pat_vals = apply_filter(&args[0], input, env, depth + 1)?;
            let pat = pat_vals
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let flags = if args.len() > 1 {
                let flag_vals = apply_filter(&args[1], input, env, depth + 1)?;
                flag_vals
                    .first()
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            } else {
                String::new()
            };
            let case_insensitive = flags.contains('i');
            match input {
                JqValue::String(s) => {
                    let matched = simple_regex_match(s, &pat, case_insensitive);
                    if name == "test" {
                        Ok(vec![JqValue::Bool(!matched.is_empty())])
                    } else if name == "match" {
                        if matched.is_empty() {
                            Err(format!("null (no match for pattern \"{pat}\")"))
                        } else {
                            let m = &matched[0];
                            Ok(vec![JqValue::Object(vec![
                                ("offset".into(), JqValue::Number(m.0 as f64)),
                                ("length".into(), JqValue::Number(m.1.len() as f64)),
                                ("string".into(), JqValue::String(m.1.clone())),
                                ("captures".into(), JqValue::Array(vec![])),
                            ])])
                        }
                    } else {
                        Ok(vec![JqValue::Object(vec![])])
                    }
                }
                _ => Err(format!("{name} requires string input")),
            }
        }

        "split" => {
            if args.len() != 1 {
                return Err("split requires 1 argument".into());
            }
            let sep_vals = apply_filter(&args[0], input, env, depth + 1)?;
            let sep = sep_vals
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match input {
                JqValue::String(s) => {
                    let parts: Vec<JqValue> = s
                        .split(&sep)
                        .map(|p| JqValue::String(p.to_string()))
                        .collect();
                    Ok(vec![JqValue::Array(parts)])
                }
                _ => Err(format!("cannot split {}", input.type_name())),
            }
        }

        "join" => {
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

        "ltrimstr" => {
            if args.len() != 1 {
                return Err("ltrimstr requires 1 argument".into());
            }
            let prefix_vals = apply_filter(&args[0], input, env, depth + 1)?;
            let prefix = prefix_vals
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match input {
                JqValue::String(s) => {
                    let result = s.strip_prefix(prefix.as_str()).unwrap_or(s);
                    Ok(vec![JqValue::String(result.to_string())])
                }
                _ => Ok(vec![input.clone()]),
            }
        }

        "rtrimstr" => {
            if args.len() != 1 {
                return Err("rtrimstr requires 1 argument".into());
            }
            let suffix_vals = apply_filter(&args[0], input, env, depth + 1)?;
            let suffix = suffix_vals
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match input {
                JqValue::String(s) => {
                    let result = s.strip_suffix(suffix.as_str()).unwrap_or(s);
                    Ok(vec![JqValue::String(result.to_string())])
                }
                _ => Ok(vec![input.clone()]),
            }
        }

        "startswith" => {
            if args.len() != 1 {
                return Err("startswith requires 1 argument".into());
            }
            let prefix_vals = apply_filter(&args[0], input, env, depth + 1)?;
            let prefix = prefix_vals
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match input {
                JqValue::String(s) => Ok(vec![JqValue::Bool(s.starts_with(prefix.as_str()))]),
                _ => Err("startswith requires string input".into()),
            }
        }

        "endswith" => {
            if args.len() != 1 {
                return Err("endswith requires 1 argument".into());
            }
            let suffix_vals = apply_filter(&args[0], input, env, depth + 1)?;
            let suffix = suffix_vals
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match input {
                JqValue::String(s) => Ok(vec![JqValue::Bool(s.ends_with(suffix.as_str()))]),
                _ => Err("endswith requires string input".into()),
            }
        }

        "ascii_downcase" => match input {
            JqValue::String(s) => Ok(vec![JqValue::String(s.to_lowercase())]),
            _ => Err(format!("cannot downcase {}", input.type_name())),
        },

        "ascii_upcase" => match input {
            JqValue::String(s) => Ok(vec![JqValue::String(s.to_uppercase())]),
            _ => Err(format!("cannot upcase {}", input.type_name())),
        },

        "tostring" => {
            let s = match input {
                JqValue::String(s) => s.clone(),
                _ => json_to_string(input, true),
            };
            Ok(vec![JqValue::String(s)])
        }

        "tonumber" => match input {
            JqValue::Number(_) => Ok(vec![input.clone()]),
            JqValue::String(s) => {
                let n: f64 = s
                    .trim()
                    .parse()
                    .map_err(|_| format!("cannot convert \"{s}\" to number"))?;
                Ok(vec![JqValue::Number(n)])
            }
            _ => Err(format!("cannot convert {} to number", input.type_name())),
        },

        "tojson" => Ok(vec![JqValue::String(json_to_string(input, true))]),

        "fromjson" => match input {
            JqValue::String(s) => {
                let val = parse_json(s)?;
                Ok(vec![val])
            }
            _ => Err(format!("cannot fromjson on {}", input.type_name())),
        },

        "explode" => match input {
            JqValue::String(s) => {
                let codepoints: Vec<JqValue> = s
                    .chars()
                    .map(|c| JqValue::Number(c as u32 as f64))
                    .collect();
                Ok(vec![JqValue::Array(codepoints)])
            }
            _ => Err(format!("cannot explode {}", input.type_name())),
        },

        "implode" => match input {
            JqValue::Array(arr) => {
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
            _ => Err(format!("cannot implode {}", input.type_name())),
        },

        "min" | "min_by" => match input {
            JqValue::Array(arr) => {
                if arr.is_empty() {
                    return Ok(vec![JqValue::Null]);
                }
                if name == "min_by" && !args.is_empty() {
                    let mut best = arr[0].clone();
                    let mut best_key = apply_filter(&args[0], &best, env, depth + 1)?
                        .into_iter()
                        .next()
                        .unwrap_or(JqValue::Null);
                    for item in &arr[1..] {
                        let key = apply_filter(&args[0], item, env, depth + 1)?
                            .into_iter()
                            .next()
                            .unwrap_or(JqValue::Null);
                        if key.compare(&best_key) == Some(std::cmp::Ordering::Less) {
                            best = item.clone();
                            best_key = key;
                        }
                    }
                    Ok(vec![best])
                } else {
                    let mut best = arr[0].clone();
                    for item in &arr[1..] {
                        if item.compare(&best) == Some(std::cmp::Ordering::Less) {
                            best = item.clone();
                        }
                    }
                    Ok(vec![best])
                }
            }
            _ => Err(format!("cannot {name} on {}", input.type_name())),
        },

        "max" | "max_by" => match input {
            JqValue::Array(arr) => {
                if arr.is_empty() {
                    return Ok(vec![JqValue::Null]);
                }
                if name == "max_by" && !args.is_empty() {
                    let mut best = arr[0].clone();
                    let mut best_key = apply_filter(&args[0], &best, env, depth + 1)?
                        .into_iter()
                        .next()
                        .unwrap_or(JqValue::Null);
                    for item in &arr[1..] {
                        let key = apply_filter(&args[0], item, env, depth + 1)?
                            .into_iter()
                            .next()
                            .unwrap_or(JqValue::Null);
                        if key.compare(&best_key) == Some(std::cmp::Ordering::Greater) {
                            best = item.clone();
                            best_key = key;
                        }
                    }
                    Ok(vec![best])
                } else {
                    let mut best = arr[0].clone();
                    for item in &arr[1..] {
                        if item.compare(&best) == Some(std::cmp::Ordering::Greater) {
                            best = item.clone();
                        }
                    }
                    Ok(vec![best])
                }
            }
            _ => Err(format!("cannot {name} on {}", input.type_name())),
        },

        "recurse" | "recurse_down" => {
            if args.is_empty() {
                Ok(input.recurse_values())
            } else {
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
        }

        "env" => Ok(vec![JqValue::Object(vec![])]),

        "path" => {
            if args.len() != 1 {
                return Err("path requires 1 argument".into());
            }
            let paths = compute_paths(&args[0], input, env, depth)?;
            Ok(paths
                .into_iter()
                .map(|p| {
                    JqValue::Array(
                        p.into_iter()
                            .map(|seg| match seg {
                                PathSeg::Key(k) => JqValue::String(k),
                                PathSeg::Index(i) => JqValue::Number(i as f64),
                            })
                            .collect(),
                    )
                })
                .collect())
        }

        "getpath" => {
            if args.len() != 1 {
                return Err("getpath requires 1 argument".into());
            }
            let path_vals = apply_filter(&args[0], input, env, depth + 1)?;
            let path_arr = path_vals.into_iter().next().unwrap_or(JqValue::Null);
            if let JqValue::Array(segments) = path_arr {
                let mut current = input.clone();
                for seg in &segments {
                    match seg {
                        JqValue::String(k) => current = field_access(&current, k),
                        JqValue::Number(_) => {
                            current = index_access(&current, seg).unwrap_or(JqValue::Null);
                        }
                        _ => return Ok(vec![JqValue::Null]),
                    }
                }
                Ok(vec![current])
            } else {
                Ok(vec![JqValue::Null])
            }
        }

        "setpath" => {
            if args.len() != 2 {
                return Err("setpath requires 2 arguments".into());
            }
            let path_vals = apply_filter(&args[0], input, env, depth + 1)?;
            let path_arr = path_vals.into_iter().next().unwrap_or(JqValue::Null);
            let val_vals = apply_filter(&args[1], input, env, depth + 1)?;
            let val = val_vals.into_iter().next().unwrap_or(JqValue::Null);
            if let JqValue::Array(segments) = path_arr {
                let result = set_path(input, &segments, &val);
                Ok(vec![result])
            } else {
                Ok(vec![input.clone()])
            }
        }

        "delpaths" => {
            if args.len() != 1 {
                return Err("delpaths requires 1 argument".into());
            }
            let paths_vals = apply_filter(&args[0], input, env, depth + 1)?;
            let paths_arr = paths_vals.into_iter().next().unwrap_or(JqValue::Null);
            if let JqValue::Array(paths) = paths_arr {
                let mut result = input.clone();
                let mut path_list: Vec<Vec<JqValue>> = Vec::new();
                for p in &paths {
                    if let JqValue::Array(segs) = p {
                        path_list.push(segs.clone());
                    }
                }
                path_list.sort_by_key(|b| std::cmp::Reverse(b.len()));
                for path in &path_list {
                    result = del_path(&result, path);
                }
                Ok(vec![result])
            } else {
                Ok(vec![input.clone()])
            }
        }

        "leaf_paths" => {
            let paths = gather_leaf_paths(input, &[]);
            Ok(vec![JqValue::Array(
                paths
                    .into_iter()
                    .map(|p| {
                        JqValue::Array(
                            p.into_iter()
                                .map(|seg| match seg {
                                    PathSeg::Key(k) => JqValue::String(k),
                                    PathSeg::Index(i) => JqValue::Number(i as f64),
                                })
                                .collect(),
                        )
                    })
                    .collect(),
            )])
        }

        "input" => Ok(vec![JqValue::Null]),
        "inputs" => Ok(vec![]),
        "debug" => Ok(vec![input.clone()]),

        "not" => Ok(vec![JqValue::Bool(!input.is_truthy())]),

        "objects" | "iterables" | "booleans" | "numbers" | "strings" | "nulls" | "arrays"
        | "scalars" => {
            let type_matches = match name {
                "objects" => matches!(input, JqValue::Object(_)),
                "arrays" => matches!(input, JqValue::Array(_)),
                "iterables" => {
                    matches!(input, JqValue::Array(_) | JqValue::Object(_))
                }
                "booleans" => matches!(input, JqValue::Bool(_)),
                "numbers" => matches!(input, JqValue::Number(_)),
                "strings" => matches!(input, JqValue::String(_)),
                "nulls" => matches!(input, JqValue::Null),
                "scalars" => !matches!(input, JqValue::Array(_) | JqValue::Object(_)),
                _ => false,
            };
            if type_matches {
                Ok(vec![input.clone()])
            } else {
                Ok(vec![])
            }
        }

        "infinite" => Ok(vec![JqValue::Number(f64::INFINITY)]),
        "nan" => Ok(vec![JqValue::Number(f64::NAN)]),
        "isinfinite" => match input {
            JqValue::Number(n) => Ok(vec![JqValue::Bool(n.is_infinite())]),
            _ => Ok(vec![JqValue::Bool(false)]),
        },
        "isnan" => match input {
            JqValue::Number(n) => Ok(vec![JqValue::Bool(n.is_nan())]),
            _ => Ok(vec![JqValue::Bool(false)]),
        },
        "isnormal" => match input {
            JqValue::Number(n) => Ok(vec![JqValue::Bool(n.is_normal())]),
            _ => Ok(vec![JqValue::Bool(false)]),
        },

        "floor" => match input {
            JqValue::Number(n) => Ok(vec![JqValue::Number(n.floor())]),
            _ => Err(format!("cannot floor {}", input.type_name())),
        },
        "ceil" => match input {
            JqValue::Number(n) => Ok(vec![JqValue::Number(n.ceil())]),
            _ => Err(format!("cannot ceil {}", input.type_name())),
        },
        "round" => match input {
            JqValue::Number(n) => Ok(vec![JqValue::Number(n.round())]),
            _ => Err(format!("cannot round {}", input.type_name())),
        },
        "fabs" => match input {
            JqValue::Number(n) => Ok(vec![JqValue::Number(n.abs())]),
            _ => Err(format!("cannot fabs {}", input.type_name())),
        },
        "sqrt" => match input {
            JqValue::Number(n) => Ok(vec![JqValue::Number(n.sqrt())]),
            _ => Err(format!("cannot sqrt {}", input.type_name())),
        },
        "pow" => {
            if args.len() != 2 {
                return Err("pow requires 2 arguments".into());
            }
            let base_vals = apply_filter(&args[0], input, env, depth + 1)?;
            let exp_vals = apply_filter(&args[1], input, env, depth + 1)?;
            let base = base_vals.first().and_then(JqValue::as_f64).unwrap_or(0.0);
            let exp = exp_vals.first().and_then(JqValue::as_f64).unwrap_or(0.0);
            Ok(vec![JqValue::Number(base.powf(exp))])
        }
        "log" | "log2" | "log10" => match input {
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
        },
        "exp" | "exp2" | "exp10" => match input {
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
        },

        "gsub" | "sub" => {
            if args.len() < 2 {
                return Err(format!("{name} requires at least 2 arguments"));
            }
            let pat_vals = apply_filter(&args[0], input, env, depth + 1)?;
            let pat = pat_vals
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let repl_vals = apply_filter(&args[1], input, env, depth + 1)?;
            let repl = repl_vals
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let flags = if args.len() > 2 {
                let f = apply_filter(&args[2], input, env, depth + 1)?;
                f.first().and_then(|v| v.as_str()).unwrap_or("").to_string()
            } else {
                String::new()
            };
            let ci = flags.contains('i');
            match input {
                JqValue::String(s) => {
                    let result = if name == "gsub" {
                        simple_regex_replace(s, &pat, &repl, ci, true)
                    } else {
                        simple_regex_replace(s, &pat, &repl, ci, false)
                    };
                    Ok(vec![JqValue::String(result)])
                }
                _ => Err(format!("{name} requires string input")),
            }
        }

        "builtins" => {
            let names = vec![
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
            Ok(vec![JqValue::Array(
                names
                    .into_iter()
                    .map(|n| JqValue::String(format!("{n}/0")))
                    .collect(),
            )])
        }

        _ => Err(format!("{name}/0 is not defined")),
    }
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
    let text = if case_insensitive {
        text.to_lowercase()
    } else {
        text.to_string()
    };
    let pattern = if case_insensitive {
        pattern.to_lowercase()
    } else {
        pattern.to_string()
    };

    let anchored_start = pattern.starts_with('^');
    let anchored_end = pattern.ends_with('$') && !pattern.ends_with("\\$");
    let pat = if anchored_start {
        &pattern[1..]
    } else {
        &pattern
    };
    let pat = if anchored_end && !pat.is_empty() {
        &pat[..pat.len() - 1]
    } else {
        pat
    };

    let mut results = Vec::new();
    if anchored_start {
        if let Some(len) = regex_match_at(&text, pat, 0) {
            let matched = &text[..len];
            if !anchored_end || len == text.len() {
                results.push((0, matched.to_string()));
            }
        }
    } else {
        for start in 0..=text.len() {
            if start > text.len() {
                break;
            }
            if let Some(len) = regex_match_at(&text[start..], pat, 0) {
                let matched = &text[start..start + len];
                if !anchored_end || start + len == text.len() {
                    results.push((start, matched.to_string()));
                    break;
                }
            }
        }
    }
    results
}

fn regex_match_at(text: &str, pattern: &str, pos: usize) -> Option<usize> {
    let pat_bytes = pattern.as_bytes();

    if pos >= pat_bytes.len() {
        return Some(0);
    }

    let (element, element_len) = parse_regex_element(pat_bytes, pos);
    let has_quantifier = pos + element_len < pat_bytes.len();
    let quantifier = if has_quantifier {
        pat_bytes[pos + element_len]
    } else {
        0
    };

    let is_quantifier = quantifier == b'*' || quantifier == b'+' || quantifier == b'?';
    let rest_pos = if is_quantifier {
        pos + element_len + 1
    } else {
        pos + element_len
    };

    let text_bytes = text.as_bytes();

    if is_quantifier {
        let min = usize::from(quantifier == b'+');
        let max = if quantifier == b'?' { 1 } else { usize::MAX };

        let mut count = 0;
        let mut text_pos = 0;
        while count < max && text_pos < text_bytes.len() {
            if matches_element(&element, text_bytes, text_pos) {
                let cl = char_len_at(text, text_pos);
                text_pos += cl;
                count += 1;
            } else {
                break;
            }
        }

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
    } else {
        if text_bytes.is_empty() && !matches!(element, RegexElement::Empty) {
            return None;
        }
        if matches_element(&element, text_bytes, 0) {
            let cl = if let RegexElement::Empty = element {
                0
            } else {
                if text_bytes.is_empty() {
                    return None;
                }
                char_len_at(text, 0)
            };
            if let Some(rest_len) = regex_match_at(&text[cl..], pattern, rest_pos) {
                return Some(cl + rest_len);
            }
        }
        None
    }
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
        b'\\' => {
            if pos + 1 < pat.len() {
                match pat[pos + 1] {
                    b'd' => (RegexElement::Digit, 2),
                    b'w' => (RegexElement::Word, 2),
                    b's' => (RegexElement::Space, 2),
                    b'D' => (RegexElement::NotDigit, 2),
                    b'W' => (RegexElement::NotWord, 2),
                    b'S' => (RegexElement::NotSpace, 2),
                    c => (RegexElement::Literal(c), 2),
                }
            } else {
                (RegexElement::Literal(b'\\'), 1)
            }
        }
        b'[' => {
            let mut i = pos + 1;
            let negated = i < pat.len() && pat[i] == b'^';
            if negated {
                i += 1;
            }
            let mut ranges = Vec::new();
            while i < pat.len() && pat[i] != b']' {
                let start = pat[i];
                if i + 2 < pat.len() && pat[i + 1] == b'-' && pat[i + 2] != b']' {
                    ranges.push((start, pat[i + 2]));
                    i += 3;
                } else {
                    ranges.push((start, start));
                    i += 1;
                }
            }
            let len = if i < pat.len() { i + 1 - pos } else { i - pos };
            (RegexElement::CharClass(ranges, negated), len)
        }
        c => (RegexElement::Literal(c), 1),
    }
}

fn matches_element(elem: &RegexElement, text: &[u8], pos: usize) -> bool {
    if pos >= text.len() {
        return matches!(elem, RegexElement::Empty);
    }
    let c = text[pos];
    match elem {
        RegexElement::Literal(expected) => c == *expected,
        RegexElement::Dot => c != b'\n',
        RegexElement::Digit => c.is_ascii_digit(),
        RegexElement::Word => c.is_ascii_alphanumeric() || c == b'_',
        RegexElement::Space => c.is_ascii_whitespace(),
        RegexElement::NotDigit => !c.is_ascii_digit(),
        RegexElement::NotWord => !(c.is_ascii_alphanumeric() || c == b'_'),
        RegexElement::NotSpace => !c.is_ascii_whitespace(),
        RegexElement::CharClass(ranges, negated) => {
            let in_class = ranges.iter().any(|(lo, hi)| c >= *lo && c <= *hi);
            if *negated {
                !in_class
            } else {
                in_class
            }
        }
        RegexElement::Empty => false,
    }
}

fn char_len_at(text: &str, byte_pos: usize) -> usize {
    text[byte_pos..].chars().next().map_or(1, char::len_utf8)
}

fn char_len_back(text: &str, byte_pos: usize) -> usize {
    text[..byte_pos].chars().last().map_or(1, char::len_utf8)
}

fn simple_regex_replace(
    text: &str,
    pattern: &str,
    replacement: &str,
    case_insensitive: bool,
    global: bool,
) -> String {
    let search_text = if case_insensitive {
        text.to_lowercase()
    } else {
        text.to_string()
    };
    let pat = if case_insensitive {
        pattern.to_lowercase()
    } else {
        pattern.to_string()
    };

    let anchored_start = pat.starts_with('^');
    let anchored_end = pat.ends_with('$') && !pat.ends_with("\\$");
    let clean_pat = {
        let p = if anchored_start { &pat[1..] } else { &pat };
        if anchored_end && !p.is_empty() {
            &p[..p.len() - 1]
        } else {
            p
        }
    };

    let mut result = String::new();
    let mut pos = 0;

    loop {
        if pos > text.len() {
            break;
        }

        if anchored_start && pos > 0 {
            result.push_str(&text[pos..]);
            break;
        }

        // Scan forward to find next match
        let mut found = None;
        let scan_start = pos;
        for start in scan_start..=search_text.len() {
            if start > search_text.len() {
                break;
            }
            if let Some(match_len) = regex_match_at(&search_text[start..], clean_pat, 0) {
                if !anchored_end || start + match_len == text.len() {
                    found = Some((start, match_len));
                    break;
                }
            }
        }

        if let Some((match_start, match_len)) = found {
            // Add text before the match
            result.push_str(&text[pos..match_start]);
            result.push_str(replacement);
            pos = match_start + match_len;
            if match_len == 0 {
                if pos < text.len() {
                    let clen = char_len_at(text, pos);
                    result.push_str(&text[pos..pos + clen]);
                    pos += clen;
                } else {
                    break;
                }
            }
            if !global {
                result.push_str(&text[pos..]);
                return result;
            }
        } else {
            result.push_str(&text[pos..]);
            break;
        }
    }
    result
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
        JqFilter::Index(idx) => {
            let vals = apply_filter(idx, input, env, depth + 1)?;
            let mut out = Vec::new();
            for v in &vals {
                match v {
                    JqValue::Number(n) => out.push(vec![PathSeg::Index(*n as usize)]),
                    JqValue::String(s) => out.push(vec![PathSeg::Key(s.clone())]),
                    _ => {}
                }
            }
            Ok(out)
        }
        JqFilter::Iterate => match input {
            JqValue::Array(arr) => Ok((0..arr.len()).map(|i| vec![PathSeg::Index(i)]).collect()),
            JqValue::Object(pairs) => Ok(pairs
                .iter()
                .map(|(k, _)| vec![PathSeg::Key(k.clone())])
                .collect()),
            _ => Ok(vec![]),
        },
        JqFilter::Pipe(left, right) => {
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
        JqFilter::Identity => Ok(vec![vec![]]),
        JqFilter::Recurse => {
            let mut out = Vec::new();
            collect_all_paths(input, &[], &mut out);
            Ok(out)
        }
        _ => Ok(vec![]),
    }
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
        JqValue::Array(arr) => {
            if arr.is_empty() {
                return vec![prefix.to_vec()];
            }
            let mut out = Vec::new();
            for (i, v) in arr.iter().enumerate() {
                let mut p = prefix.to_vec();
                p.push(PathSeg::Index(i));
                out.extend(gather_leaf_paths(v, &p));
            }
            out
        }
        JqValue::Object(pairs) => {
            if pairs.is_empty() {
                return vec![prefix.to_vec()];
            }
            let mut out = Vec::new();
            for (k, v) in pairs {
                let mut p = prefix.to_vec();
                p.push(PathSeg::Key(k.clone()));
                out.extend(gather_leaf_paths(v, &p));
            }
            out
        }
        _ => vec![prefix.to_vec()],
    }
}

fn set_path(val: &JqValue, path: &[JqValue], new_val: &JqValue) -> JqValue {
    if path.is_empty() {
        return new_val.clone();
    }
    let seg = &path[0];
    let rest = &path[1..];
    match seg {
        JqValue::String(key) => {
            let mut pairs = if let JqValue::Object(p) = val {
                p.clone()
            } else {
                Vec::new()
            };
            let existing = pairs.iter().position(|(k, _)| k == key);
            let inner = existing.map_or(JqValue::Null, |i| pairs[i].1.clone());
            let new_inner = set_path(&inner, rest, new_val);
            if let Some(i) = existing {
                pairs[i].1 = new_inner;
            } else {
                pairs.push((key.clone(), new_inner));
            }
            JqValue::Object(pairs)
        }
        JqValue::Number(n) => {
            let idx = *n as usize;
            let mut arr = if let JqValue::Array(a) = val {
                a.clone()
            } else {
                Vec::new()
            };
            while arr.len() <= idx {
                arr.push(JqValue::Null);
            }
            let inner = arr[idx].clone();
            arr[idx] = set_path(&inner, rest, new_val);
            JqValue::Array(arr)
        }
        _ => val.clone(),
    }
}

fn del_path(val: &JqValue, path: &[JqValue]) -> JqValue {
    if path.is_empty() {
        return JqValue::Null;
    }
    if path.len() == 1 {
        match (&path[0], val) {
            (JqValue::String(key), JqValue::Object(pairs)) => {
                let new_pairs: Vec<_> = pairs.iter().filter(|(k, _)| k != key).cloned().collect();
                return JqValue::Object(new_pairs);
            }
            (JqValue::Number(n), JqValue::Array(arr)) => {
                let idx = *n as usize;
                let mut new_arr = arr.clone();
                if idx < new_arr.len() {
                    new_arr.remove(idx);
                }
                return JqValue::Array(new_arr);
            }
            _ => return val.clone(),
        }
    }
    let seg = &path[0];
    let rest = &path[1..];
    match (seg, val) {
        (JqValue::String(key), JqValue::Object(pairs)) => {
            let new_pairs: Vec<_> = pairs
                .iter()
                .map(|(k, v)| {
                    if k == key {
                        (k.clone(), del_path(v, rest))
                    } else {
                        (k.clone(), v.clone())
                    }
                })
                .collect();
            JqValue::Object(new_pairs)
        }
        (JqValue::Number(n), JqValue::Array(arr)) => {
            let idx = *n as usize;
            let new_arr: Vec<_> = arr
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    if i == idx {
                        del_path(v, rest)
                    } else {
                        v.clone()
                    }
                })
                .collect();
            JqValue::Array(new_arr)
        }
        _ => val.clone(),
    }
}

// ---------------------------------------------------------------------------
// Format strings (@csv, @tsv, @html, @json, @base64, @base64d, @uri)
// ---------------------------------------------------------------------------

fn apply_format(name: &str, input: &JqValue) -> Result<Vec<JqValue>, String> {
    match name {
        "csv" => match input {
            JqValue::Array(arr) => {
                let mut out = String::new();
                for (i, v) in arr.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    match v {
                        JqValue::String(s) => {
                            out.push('"');
                            for c in s.chars() {
                                if c == '"' {
                                    out.push_str("\"\"");
                                } else {
                                    out.push(c);
                                }
                            }
                            out.push('"');
                        }
                        JqValue::Null => {}
                        other => out.push_str(&other.to_string_repr()),
                    }
                }
                Ok(vec![JqValue::String(out)])
            }
            _ => Err("@csv requires array input".into()),
        },

        "tsv" => match input {
            JqValue::Array(arr) => {
                let mut out = String::new();
                for (i, v) in arr.iter().enumerate() {
                    if i > 0 {
                        out.push('\t');
                    }
                    match v {
                        JqValue::String(s) => {
                            for c in s.chars() {
                                match c {
                                    '\t' => out.push_str("\\t"),
                                    '\n' => out.push_str("\\n"),
                                    '\r' => out.push_str("\\r"),
                                    '\\' => out.push_str("\\\\"),
                                    _ => out.push(c),
                                }
                            }
                        }
                        JqValue::Null => {}
                        other => out.push_str(&other.to_string_repr()),
                    }
                }
                Ok(vec![JqValue::String(out)])
            }
            _ => Err("@tsv requires array input".into()),
        },

        "html" => {
            let s = match input {
                JqValue::String(s) => s.clone(),
                other => other.to_string_repr(),
            };
            let escaped = s
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('\'', "&#39;")
                .replace('"', "&quot;");
            Ok(vec![JqValue::String(escaped)])
        }

        "json" => Ok(vec![JqValue::String(json_to_string(input, true))]),

        "text" => Ok(vec![JqValue::String(input.to_string_repr())]),

        "base64" => {
            let s = match input {
                JqValue::String(s) => s.clone(),
                other => other.to_string_repr(),
            };
            Ok(vec![JqValue::String(simple_base64_encode(s.as_bytes()))])
        }

        "base64d" => {
            let s = match input {
                JqValue::String(s) => s.clone(),
                _ => return Err("@base64d requires string input".into()),
            };
            match simple_base64_decode(&s) {
                Ok(decoded) => Ok(vec![JqValue::String(
                    String::from_utf8_lossy(&decoded).into_owned(),
                )]),
                Err(e) => Err(format!("@base64d: {e}")),
            }
        }

        "uri" => {
            let s = match input {
                JqValue::String(s) => s.clone(),
                other => other.to_string_repr(),
            };
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
            Ok(vec![JqValue::String(encoded)])
        }

        _ => Err(format!("unknown format: @{name}")),
    }
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
    let clean: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if clean.is_empty() {
        return Ok(Vec::new());
    }
    let padded = if !clean.len().is_multiple_of(4) {
        let mut v = clean;
        while !v.len().is_multiple_of(4) {
            v.push(b'=');
        }
        v
    } else {
        clean
    };
    let mut out = Vec::with_capacity(padded.len() / 4 * 3);
    for chunk in padded.chunks_exact(4) {
        let a = b64_val(chunk[0]).ok_or("invalid base64 character")?;
        let b = b64_val(chunk[1]).ok_or("invalid base64 character")?;
        let c = if chunk[2] == b'=' {
            None
        } else {
            Some(b64_val(chunk[2]).ok_or("invalid base64 character")?)
        };
        let d = if chunk[3] == b'=' {
            None
        } else {
            Some(b64_val(chunk[3]).ok_or("invalid base64 character")?)
        };
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
    }
    Ok(out)
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

#[allow(clippy::too_many_lines)]
pub(crate) fn util_jq(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut raw_output = false;
    let mut exit_status = false;
    let mut compact = false;
    let mut null_input = false;
    let mut slurp = false;
    let mut jq_vars: Vec<(String, JqValue)> = Vec::new();
    let mut filter_str = None;

    // Parse options
    while let Some(&arg) = args.first() {
        if arg == "-r" || arg == "--raw-output" {
            raw_output = true;
            args = &args[1..];
        } else if arg == "-e" || arg == "--exit-status" {
            exit_status = true;
            args = &args[1..];
        } else if arg == "-c" || arg == "--compact-output" {
            compact = true;
            args = &args[1..];
        } else if arg == "-n" || arg == "--null-input" {
            null_input = true;
            args = &args[1..];
        } else if arg == "-s" || arg == "--slurp" {
            slurp = true;
            args = &args[1..];
        } else if arg == "-j" || arg == "--join-output" {
            raw_output = true;
            args = &args[1..];
        } else if arg == "--arg" {
            if args.len() < 3 {
                ctx.output.stderr(b"jq: --arg requires NAME VALUE\n");
                return 1;
            }
            jq_vars.push((args[1].to_string(), JqValue::String(args[2].to_string())));
            args = &args[3..];
        } else if arg == "--argjson" {
            if args.len() < 3 {
                ctx.output.stderr(b"jq: --argjson requires NAME VALUE\n");
                return 1;
            }
            let val = match parse_json(args[2]) {
                Ok(v) => v,
                Err(e) => {
                    let msg = format!("jq: invalid JSON for --argjson: {e}\n");
                    ctx.output.stderr(msg.as_bytes());
                    return 1;
                }
            };
            jq_vars.push((args[1].to_string(), val));
            args = &args[3..];
        } else if arg == "--" {
            args = &args[1..];
            break;
        } else if arg.starts_with('-') && arg.len() > 1 {
            // Combined short flags like -rc
            let flags = &arg[1..];
            let mut unknown = false;
            for c in flags.chars() {
                match c {
                    'e' => exit_status = true,
                    'c' => compact = true,
                    'n' => null_input = true,
                    's' => slurp = true,
                    'r' | 'j' => raw_output = true,
                    _ => {
                        unknown = true;
                        break;
                    }
                }
            }
            if unknown {
                break;
            }
            args = &args[1..];
        } else {
            break;
        }
    }

    // Next non-flag arg is the filter
    if let Some(&f) = args.first() {
        filter_str = Some(f);
        args = &args[1..];
    }

    let Some(filter_str) = filter_str else {
        ctx.output.stderr(b"jq: no filter provided\n");
        return 1;
    };

    // Parse the filter
    let filter = match parse_filter(filter_str) {
        Ok(f) => f,
        Err(e) => {
            let msg = format!("jq: error parsing filter: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            return 2;
        }
    };

    // Set up environment with variables
    let mut env = JqEnv::new();
    for (name, val) in &jq_vars {
        env.vars.insert(name.clone(), val.clone());
    }

    // Collect input JSON values
    let file_args = args;
    let input_texts = if null_input {
        vec![]
    } else if file_args.is_empty() {
        if let Some(data) = ctx.stdin {
            let text = String::from_utf8_lossy(data).to_string();
            vec![text]
        } else {
            ctx.output.stderr(b"jq: no input\n");
            return 1;
        }
    } else {
        let mut texts = Vec::new();
        for path in file_args {
            let full = resolve_path(ctx.cwd, path);
            match read_text(ctx.fs, &full) {
                Ok(text) => texts.push(text),
                Err(e) => {
                    emit_error(ctx.output, "jq", path, &e);
                    return 1;
                }
            }
        }
        texts
    };

    // Parse all JSON inputs
    let mut json_values = Vec::new();
    if null_input {
        json_values.push(JqValue::Null);
    } else {
        for text in &input_texts {
            match JsonParser::parse_all(text) {
                Ok(vals) => json_values.extend(vals),
                Err(e) => {
                    let msg = format!("jq: error parsing JSON: {e}\n");
                    ctx.output.stderr(msg.as_bytes());
                    return 2;
                }
            }
        }
        if json_values.is_empty() {
            ctx.output.stderr(b"jq: no input\n");
            return 1;
        }
    }

    // Apply slurp: combine all inputs into a single array
    let inputs = if slurp {
        vec![JqValue::Array(json_values)]
    } else {
        json_values
    };

    // Execute filter for each input
    let mut last_value = None;
    let mut had_output = false;
    let mut status = 0;

    for input_val in &inputs {
        match run_filter(&filter, input_val, &env) {
            Ok(results) => {
                for val in results {
                    had_output = true;
                    last_value = Some(val.clone());
                    output_value(ctx, &val, raw_output, compact);
                }
            }
            Err(e) if e == EMPTY_SIGNAL => {
                // `empty` produces no output — not an error
            }
            Err(e) => {
                let msg = format!("jq: {e}\n");
                ctx.output.stderr(msg.as_bytes());
                status = 5;
            }
        }
    }

    if exit_status {
        if let Some(last) = &last_value {
            if !last.is_truthy() {
                return 1;
            }
        } else if !had_output {
            return 4;
        }
    }
    status
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
            };
            util_jq(&mut ctx, &["jq", "-e", "."])
        };
        assert_eq!(status, 1);
    }
}
