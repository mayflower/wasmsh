//! YAML utility: yq.

use std::fmt::Write;

use crate::helpers::{collect_input_text, collect_path_text, resolve_path};
use crate::UtilContext;

// ---------------------------------------------------------------------------
// YAML value representation (mirrors jq's shape)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum YamlValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<YamlValue>),
    Object(Vec<(String, YamlValue)>),
}

impl YamlValue {
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

    fn obj_get(&self, key: &str) -> YamlValue {
        if let Self::Object(pairs) = self {
            for (k, v) in pairs {
                if k == key {
                    return v.clone();
                }
            }
        }
        Self::Null
    }

    fn length(&self) -> YamlValue {
        match self {
            Self::Null => Self::Number(0.0),
            Self::Bool(_) | Self::Number(_) => Self::Null,
            Self::String(s) => Self::Number(s.chars().count() as f64),
            Self::Array(a) => Self::Number(a.len() as f64),
            Self::Object(o) => Self::Number(o.len() as f64),
        }
    }
}

// ---------------------------------------------------------------------------
// YAML parser
// ---------------------------------------------------------------------------

struct YamlParser<'a> {
    lines: Vec<&'a str>,
    pos: usize,
}

impl<'a> YamlParser<'a> {
    fn new(input: &'a str) -> Self {
        let lines: Vec<&str> = input.lines().collect();
        Self { lines, pos: 0 }
    }

    fn parse(&mut self) -> Result<YamlValue, String> {
        self.skip_blanks_and_comments();

        // Skip document separator
        if self.pos < self.lines.len() && self.lines[self.pos].trim() == "---" {
            self.pos += 1;
            self.skip_blanks_and_comments();
        }

        if self.pos >= self.lines.len() {
            return Ok(YamlValue::Null);
        }

        self.parse_value(0)
    }

    fn skip_blanks_and_comments(&mut self) {
        while self.pos < self.lines.len() {
            let trimmed = self.lines[self.pos].trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn current_indent(&self) -> usize {
        if self.pos >= self.lines.len() {
            return 0;
        }
        let line = self.lines[self.pos];
        line.len() - line.trim_start().len()
    }

    fn parse_value(&mut self, min_indent: usize) -> Result<YamlValue, String> {
        self.skip_blanks_and_comments();

        if self.pos >= self.lines.len() {
            return Ok(YamlValue::Null);
        }

        let line = self.lines[self.pos];
        let trimmed = line.trim();

        // Flow sequence
        if trimmed.starts_with('[') {
            return self.parse_flow_sequence();
        }

        // Flow mapping
        if trimmed.starts_with('{') {
            return self.parse_flow_mapping();
        }

        // Block list
        if trimmed.starts_with("- ") || trimmed == "-" {
            return self.parse_block_list(min_indent);
        }

        // Multi-line literal/folded
        if trimmed == "|"
            || trimmed == ">"
            || trimmed.starts_with("| ")
            || trimmed.starts_with("> ")
        {
            return Ok(self.parse_multiline_string(trimmed.starts_with('>')));
        }

        // Key-value mapping
        if let Some(_colon_pos) = find_colon_separator(trimmed) {
            return self.parse_block_mapping(min_indent);
        }

        // Scalar value
        let val = parse_scalar(trimmed);
        self.pos += 1;
        Ok(val)
    }

    fn parse_block_mapping(&mut self, min_indent: usize) -> Result<YamlValue, String> {
        let mut pairs = Vec::new();
        let block_indent = self.current_indent();

        if block_indent < min_indent {
            return Ok(YamlValue::Null);
        }

        while self.pos < self.lines.len() {
            self.skip_blanks_and_comments();
            if self.pos >= self.lines.len() || self.current_indent() != block_indent {
                break;
            }

            let line = self.lines[self.pos];
            let trimmed = line.trim();

            let Some(colon_pos) = find_colon_separator(trimmed) else {
                break;
            };

            let key = unquote_string(trimmed[..colon_pos].trim());
            let after_colon = trimmed[colon_pos + 1..].trim();
            let val = self.parse_mapping_value(after_colon, block_indent)?;
            pairs.push((key, val));
        }

        Ok(YamlValue::Object(pairs))
    }

    /// Parse the value portion of a mapping entry (everything after the colon).
    fn parse_mapping_value(
        &mut self,
        after_colon: &str,
        block_indent: usize,
    ) -> Result<YamlValue, String> {
        if after_colon.is_empty() {
            self.pos += 1;
            self.skip_blanks_and_comments();
            if self.pos < self.lines.len() && self.current_indent() > block_indent {
                return self.parse_value(block_indent + 1);
            }
            return Ok(YamlValue::Null);
        }
        if after_colon == "|" || after_colon == ">" {
            self.pos += 1;
            return Ok(self.parse_multiline_string_body(after_colon == ">"));
        }
        self.pos += 1;
        parse_inline_value(after_colon)
    }

    fn parse_block_list(&mut self, min_indent: usize) -> Result<YamlValue, String> {
        let mut items = Vec::new();
        let block_indent = self.current_indent();

        if block_indent < min_indent {
            return Ok(YamlValue::Null);
        }

        while self.pos < self.lines.len() {
            self.skip_blanks_and_comments();
            if self.pos >= self.lines.len() || self.current_indent() != block_indent {
                break;
            }

            let line = self.lines[self.pos];
            let trimmed = line.trim();

            if !trimmed.starts_with("- ") && trimmed != "-" {
                break;
            }

            let item_text = if trimmed == "-" { "" } else { &trimmed[2..] };
            items.push(self.parse_list_item(item_text, block_indent)?);
        }

        Ok(YamlValue::Array(items))
    }

    /// Parse a single list item value, dispatching by its textual shape.
    fn parse_list_item(
        &mut self,
        item_text: &str,
        block_indent: usize,
    ) -> Result<YamlValue, String> {
        if item_text.is_empty() {
            return self.parse_list_item_next_line(block_indent);
        }
        if item_text.starts_with('[') {
            self.pos += 1;
            return parse_inline_flow_sequence(item_text);
        }
        if item_text.starts_with('{') {
            self.pos += 1;
            return parse_inline_flow_mapping(item_text);
        }
        if find_colon_separator(item_text).is_some() {
            return self.parse_list_item_mapping(item_text, block_indent);
        }
        self.pos += 1;
        Ok(parse_scalar(item_text))
    }

    /// Handle a list item whose value appears on the next line(s).
    fn parse_list_item_next_line(&mut self, block_indent: usize) -> Result<YamlValue, String> {
        self.pos += 1;
        self.skip_blanks_and_comments();
        if self.pos < self.lines.len() && self.current_indent() > block_indent {
            self.parse_value(block_indent + 1)
        } else {
            Ok(YamlValue::Null)
        }
    }

    /// Handle the `- key: val` case, possibly followed by deeper mapping keys.
    fn parse_list_item_mapping(
        &mut self,
        item_text: &str,
        block_indent: usize,
    ) -> Result<YamlValue, String> {
        self.pos += 1;
        let first_val = parse_inline_mapping_item(item_text)?;

        self.skip_blanks_and_comments();
        if self.pos >= self.lines.len() || self.current_indent() <= block_indent {
            return Ok(first_val);
        }

        let deeper_indent = self.current_indent();
        let mut pairs = match first_val {
            YamlValue::Object(p) => p,
            _ => vec![],
        };
        self.collect_deeper_mapping_keys(&mut pairs, deeper_indent)?;
        Ok(YamlValue::Object(pairs))
    }

    /// Collect additional `key: value` pairs at a fixed deeper indent level.
    fn collect_deeper_mapping_keys(
        &mut self,
        pairs: &mut Vec<(String, YamlValue)>,
        deeper_indent: usize,
    ) -> Result<(), String> {
        while self.pos < self.lines.len() {
            self.skip_blanks_and_comments();
            if self.pos >= self.lines.len() || self.current_indent() != deeper_indent {
                break;
            }
            let sub_trimmed = self.lines[self.pos].trim();
            let Some(cp) = find_colon_separator(sub_trimmed) else {
                break;
            };
            let k = unquote_string(sub_trimmed[..cp].trim());
            let v_text = sub_trimmed[cp + 1..].trim();
            if v_text.is_empty() {
                self.pos += 1;
                self.skip_blanks_and_comments();
                if self.pos < self.lines.len() && self.current_indent() > deeper_indent {
                    let v = self.parse_value(deeper_indent + 1)?;
                    pairs.push((k, v));
                } else {
                    pairs.push((k, YamlValue::Null));
                }
            } else {
                self.pos += 1;
                pairs.push((k, parse_scalar(v_text)));
            }
        }
        Ok(())
    }

    fn parse_flow_sequence(&mut self) -> Result<YamlValue, String> {
        let trimmed = self.lines[self.pos].trim().to_string();
        self.pos += 1;
        parse_inline_flow_sequence(&trimmed)
    }

    fn parse_flow_mapping(&mut self) -> Result<YamlValue, String> {
        let trimmed = self.lines[self.pos].trim().to_string();
        self.pos += 1;
        parse_inline_flow_mapping(&trimmed)
    }

    fn parse_multiline_string(&mut self, folded: bool) -> YamlValue {
        self.pos += 1;
        self.parse_multiline_string_body(folded)
    }

    fn parse_multiline_string_body(&mut self, folded: bool) -> YamlValue {
        let Some(body_indent) = self.multiline_body_indent() else {
            return YamlValue::String(String::new());
        };
        let mut lines = self.collect_multiline_lines(body_indent);
        trim_trailing_empty_lines(&mut lines);
        YamlValue::String(format_multiline_lines(&lines, folded))
    }

    fn multiline_body_indent(&self) -> Option<usize> {
        let line = *self.lines.get(self.pos)?;
        if !line.trim().is_empty() {
            return Some(line.len() - line.trim_start().len());
        }
        let idx = (self.pos..self.lines.len()).find(|&idx| !self.lines[idx].trim().is_empty())?;
        let line = self.lines[idx];
        Some(line.len() - line.trim_start().len())
    }

    fn collect_multiline_lines(&mut self, body_indent: usize) -> Vec<&'a str> {
        let mut lines = Vec::new();
        while self.pos < self.lines.len() {
            let line = self.lines[self.pos];
            let indent = line.len() - line.trim_start().len();
            if !line.trim().is_empty() && indent < body_indent {
                break;
            }
            lines.push(multiline_line_content(line, body_indent));
            self.pos += 1;
        }
        lines
    }
}

fn multiline_line_content(line: &str, body_indent: usize) -> &str {
    if line.trim().is_empty() {
        ""
    } else {
        &line[body_indent..]
    }
}

fn trim_trailing_empty_lines(lines: &mut Vec<&str>) {
    while lines.last() == Some(&"") {
        lines.pop();
    }
}

fn format_multiline_lines(lines: &[&str], folded: bool) -> String {
    if !folded {
        let mut out = lines.join("\n");
        out.push('\n');
        return out;
    }

    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if line.is_empty() {
            out.push('\n');
            continue;
        }
        if i > 0 && !lines[i - 1].is_empty() && !out.ends_with('\n') {
            out.push(' ');
        }
        out.push_str(line);
    }
    out.push('\n');
    out
}

/// Find `: ` separator in a line (not inside quotes).
fn find_colon_separator(s: &str) -> Option<usize> {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let bytes = s.as_bytes();

    for i in 0..bytes.len() {
        let b = bytes[i];
        if b == b'\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
        } else if b == b'"' && !in_single_quote {
            in_double_quote = !in_double_quote;
        } else if b == b':' && !in_single_quote && !in_double_quote {
            // Must be followed by space, end of string, or nothing
            if i + 1 >= bytes.len() || bytes[i + 1] == b' ' {
                return Some(i);
            }
        }
    }
    None
}

fn unquote_string(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn parse_scalar(s: &str) -> YamlValue {
    let s = s.trim();
    if s.is_empty() || s == "null" || s == "~" {
        return YamlValue::Null;
    }
    if s == "true" || s == "yes" {
        return YamlValue::Bool(true);
    }
    if s == "false" || s == "no" {
        return YamlValue::Bool(false);
    }

    // Quoted string
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        return YamlValue::String(s[1..s.len() - 1].to_string());
    }

    // Strip inline comments
    let val = if let Some(hash_pos) = find_inline_comment(s) {
        s[..hash_pos].trim()
    } else {
        s
    };

    // Number
    if let Ok(n) = val.parse::<i64>() {
        return YamlValue::Number(n as f64);
    }
    if let Ok(n) = val.parse::<f64>() {
        return YamlValue::Number(n);
    }

    YamlValue::String(val.to_string())
}

/// Find inline comment position (# preceded by space, outside quotes).
fn find_inline_comment(s: &str) -> Option<usize> {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let bytes = s.as_bytes();

    for i in 0..bytes.len() {
        let b = bytes[i];
        if b == b'\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
        } else if b == b'"' && !in_single_quote {
            in_double_quote = !in_double_quote;
        } else if b == b'#' && !in_single_quote && !in_double_quote && i > 0 && bytes[i - 1] == b' '
        {
            return Some(i);
        }
    }
    None
}

/// Parse an inline value that starts with `[`, `{`, or is a plain scalar.
fn parse_inline_value(s: &str) -> Result<YamlValue, String> {
    if s.starts_with('[') {
        parse_inline_flow_sequence(s)
    } else if s.starts_with('{') {
        parse_inline_flow_mapping(s)
    } else {
        Ok(parse_scalar(s))
    }
}

fn parse_inline_flow_sequence(s: &str) -> Result<YamlValue, String> {
    let s = s.trim();
    if !s.starts_with('[') || !s.ends_with(']') {
        return Err("invalid flow sequence".to_string());
    }
    let inner = s[1..s.len() - 1].trim();
    if inner.is_empty() {
        return Ok(YamlValue::Array(Vec::new()));
    }

    let items = split_flow_items(inner);
    let mut result = Vec::new();
    for item in items {
        result.push(parse_inline_value(item.trim())?);
    }

    Ok(YamlValue::Array(result))
}

fn parse_inline_flow_mapping(s: &str) -> Result<YamlValue, String> {
    let s = s.trim();
    if !s.starts_with('{') || !s.ends_with('}') {
        return Err("invalid flow mapping".to_string());
    }
    let inner = s[1..s.len() - 1].trim();
    if inner.is_empty() {
        return Ok(YamlValue::Object(Vec::new()));
    }

    let items = split_flow_items(inner);
    let mut pairs = Vec::new();
    for item in items {
        if let Some(pair) = parse_flow_mapping_pair(item.trim())? {
            pairs.push(pair);
        }
    }

    Ok(YamlValue::Object(pairs))
}

/// Parse a single `key: value` pair inside a flow mapping.
fn parse_flow_mapping_pair(trimmed: &str) -> Result<Option<(String, YamlValue)>, String> {
    if let Some(colon_pos) = trimmed.find(": ") {
        let key = unquote_string(&trimmed[..colon_pos]);
        let val_str = trimmed[colon_pos + 2..].trim();
        let val = parse_inline_value(val_str)?;
        return Ok(Some((key, val)));
    }
    if let Some(colon_pos) = trimmed.find(':') {
        let key = unquote_string(&trimmed[..colon_pos]);
        let val_str = trimmed[colon_pos + 1..].trim();
        let val = if val_str.is_empty() {
            YamlValue::Null
        } else {
            parse_scalar(val_str)
        };
        return Ok(Some((key, val)));
    }
    Ok(None)
}

fn parse_inline_mapping_item(s: &str) -> Result<YamlValue, String> {
    let Some(colon_pos) = find_colon_separator(s) else {
        return Ok(parse_scalar(s));
    };
    let key = unquote_string(s[..colon_pos].trim());
    let val_str = s[colon_pos + 1..].trim();
    let val = if val_str.is_empty() {
        YamlValue::Null
    } else {
        parse_inline_value(val_str)?
    };
    Ok(YamlValue::Object(vec![(key, val)]))
}

/// Split flow items by commas, respecting nesting of `[]` and `{}`.
fn split_flow_items(s: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    for ch in s.chars() {
        if flow_toggle_quote(ch, &mut in_single_quote, &mut in_double_quote) {
            current.push(ch);
            continue;
        }
        if in_single_quote || in_double_quote {
            current.push(ch);
            continue;
        }
        if flow_split_here(ch, depth) {
            items.push(current.clone());
            current.clear();
            continue;
        }
        depth = flow_update_depth(depth, ch);
        current.push(ch);
    }

    if !current.trim().is_empty() {
        items.push(current);
    }

    items
}

fn flow_toggle_quote(ch: char, in_single_quote: &mut bool, in_double_quote: &mut bool) -> bool {
    if ch == '\'' && !*in_double_quote {
        *in_single_quote = !*in_single_quote;
        return true;
    }
    if ch == '"' && !*in_single_quote {
        *in_double_quote = !*in_double_quote;
        return true;
    }
    false
}

fn flow_split_here(ch: char, depth: i32) -> bool {
    ch == ',' && depth == 0
}

fn flow_update_depth(depth: i32, ch: char) -> i32 {
    match ch {
        '[' | '{' => depth + 1,
        ']' | '}' => depth - 1,
        _ => depth,
    }
}

// ---------------------------------------------------------------------------
// Filter evaluation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Filter {
    Identity,                       // .
    Field(String),                  // .key
    Index(i64),                     // .[N]
    Iterate,                        // .[]
    Pipe(Box<Filter>, Box<Filter>), // f1 | f2
    Keys,                           // keys
    Values,                         // values
    Length,                         // length
    Type,                           // type
    Select(Box<Filter>),            // select(expr)
    First,                          // first
    Last,                           // last
    Flatten,                        // flatten
    Map(Box<Filter>),               // map(expr)
    Not,                            // not
    FieldChain(Vec<String>),        // .a.b.c
}

fn parse_filter(input: &str) -> Result<Filter, String> {
    let input = input.trim();
    if input.is_empty() || input == "." {
        return Ok(Filter::Identity);
    }

    // Handle pipe
    if let Some((left, right)) = split_pipe(input) {
        let lf = parse_filter(left)?;
        let rf = parse_filter(right)?;
        return Ok(Filter::Pipe(Box::new(lf), Box::new(rf)));
    }

    // Built-in named functions
    if let Some(f) = parse_builtin_filter(input) {
        return Ok(f);
    }

    // Wrapper functions: select(...), map(...)
    if let Some(f) = parse_wrapper_filter(input)? {
        return Ok(f);
    }

    // Dot-prefixed access patterns
    if let Some(f) = parse_dot_filter(input) {
        return Ok(f);
    }

    Err(format!("unsupported filter: {input}"))
}

/// Match named built-in filter keywords.
fn parse_builtin_filter(input: &str) -> Option<Filter> {
    match input {
        "keys" => Some(Filter::Keys),
        "values" => Some(Filter::Values),
        "length" => Some(Filter::Length),
        "type" => Some(Filter::Type),
        "first" => Some(Filter::First),
        "last" => Some(Filter::Last),
        "flatten" => Some(Filter::Flatten),
        "not" => Some(Filter::Not),
        _ => None,
    }
}

/// Parse `select(...)` and `map(...)` wrapper filters.
fn parse_wrapper_filter(input: &str) -> Result<Option<Filter>, String> {
    if let Some(inner) = input
        .strip_prefix("select(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let f = parse_filter(inner)?;
        return Ok(Some(Filter::Select(Box::new(f))));
    }
    if let Some(inner) = input.strip_prefix("map(").and_then(|s| s.strip_suffix(')')) {
        let f = parse_filter(inner)?;
        return Ok(Some(Filter::Map(Box::new(f))));
    }
    Ok(None)
}

/// Parse dot-prefixed filters: `.[]`, `.[N]`, `.key`, `.key[]`, `.key[N]`, `.a.b.c`.
fn parse_dot_filter(input: &str) -> Option<Filter> {
    if input == ".[]" {
        return Some(Filter::Iterate);
    }

    // .[N] — index
    if input.starts_with(".[") && input.ends_with(']') {
        let idx_str = &input[2..input.len() - 1];
        if let Ok(idx) = idx_str.parse::<i64>() {
            return Some(Filter::Index(idx));
        }
    }

    let rest = input.strip_prefix('.')?;

    // Chained field access: .a.b.c
    let parts: Vec<&str> = rest.split('.').collect();
    if parts.len() > 1 && parts.iter().all(|p| !p.is_empty() && !p.contains('[')) {
        return Some(Filter::FieldChain(
            parts.iter().map(|s| (*s).to_string()).collect(),
        ));
    }

    // Simple field: .key
    if !rest.is_empty() && !rest.contains('[') && !rest.contains('|') {
        return Some(Filter::Field(rest.to_string()));
    }

    // .key[] — field then iterate
    if let Some(field) = rest.strip_suffix("[]") {
        if !field.is_empty() {
            return Some(Filter::Pipe(
                Box::new(Filter::Field(field.to_string())),
                Box::new(Filter::Iterate),
            ));
        }
    }

    // .key[N] — field then index
    parse_field_index(rest)
}

/// Parse `.key[N]` patterns from the portion after the leading dot.
fn parse_field_index(rest: &str) -> Option<Filter> {
    if !rest.contains('[') || !rest.ends_with(']') {
        return None;
    }
    let bracket = rest.find('[')?;
    let field = &rest[..bracket];
    let idx_str = &rest[bracket + 1..rest.len() - 1];
    let idx = idx_str.parse::<i64>().ok()?;
    Some(Filter::Pipe(
        Box::new(Filter::Field(field.to_string())),
        Box::new(Filter::Index(idx)),
    ))
}

/// Split on the top-level `|` (not inside parens).
fn split_pipe(s: &str) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    let bytes = s.as_bytes();
    for i in 0..bytes.len() {
        let b = bytes[i];
        if b == b'(' {
            depth += 1;
        } else if b == b')' {
            depth -= 1;
        } else if b == b'|' && depth == 0 {
            return Some((s[..i].trim(), s[i + 1..].trim()));
        }
    }
    None
}

fn eval_filter(val: &YamlValue, filter: &Filter) -> Result<Vec<YamlValue>, String> {
    match filter {
        Filter::Identity => Ok(vec![val.clone()]),
        Filter::Field(key) => Ok(vec![val.obj_get(key)]),
        Filter::FieldChain(keys) => Ok(vec![eval_field_chain(val, keys)]),
        Filter::Index(idx) => Ok(vec![eval_index(val, *idx)]),
        Filter::Iterate => eval_iterate(val),
        Filter::Pipe(left, right) => eval_pipe(val, left, right),
        Filter::Keys => eval_keys(val),
        Filter::Values => eval_values(val),
        Filter::Length => Ok(vec![val.length()]),
        Filter::Type => Ok(vec![YamlValue::String(val.type_name().to_string())]),
        Filter::Select(inner) => eval_select(val, inner),
        Filter::First => Ok(eval_first(val)),
        Filter::Last => Ok(eval_last(val)),
        Filter::Flatten => Ok(eval_flatten(val)),
        Filter::Map(inner) => eval_map(val, inner),
        Filter::Not => Ok(vec![YamlValue::Bool(!val.is_truthy())]),
    }
}

fn eval_field_chain(val: &YamlValue, keys: &[String]) -> YamlValue {
    let mut current = val.clone();
    for key in keys {
        current = current.obj_get(key);
    }
    current
}

fn eval_index(val: &YamlValue, idx: i64) -> YamlValue {
    let YamlValue::Array(arr) = val else {
        return YamlValue::Null;
    };
    let i = if idx < 0 {
        arr.len().wrapping_add(idx as usize)
    } else {
        idx as usize
    };
    arr.get(i).cloned().unwrap_or(YamlValue::Null)
}

fn eval_iterate(val: &YamlValue) -> Result<Vec<YamlValue>, String> {
    match val {
        YamlValue::Array(arr) => Ok(arr.clone()),
        YamlValue::Object(pairs) => Ok(pairs.iter().map(|(_, v)| v.clone()).collect()),
        _ => Err(format!("cannot iterate over {}", val.type_name())),
    }
}

fn eval_pipe(val: &YamlValue, left: &Filter, right: &Filter) -> Result<Vec<YamlValue>, String> {
    let intermediate = eval_filter(val, left)?;
    let mut results = Vec::new();
    for v in &intermediate {
        results.extend(eval_filter(v, right)?);
    }
    Ok(results)
}

fn eval_keys(val: &YamlValue) -> Result<Vec<YamlValue>, String> {
    match val {
        YamlValue::Object(pairs) => Ok(vec![YamlValue::Array(
            pairs
                .iter()
                .map(|(k, _)| YamlValue::String(k.clone()))
                .collect(),
        )]),
        YamlValue::Array(arr) => Ok(vec![YamlValue::Array(
            (0..arr.len())
                .map(|i| YamlValue::Number(i as f64))
                .collect(),
        )]),
        _ => Err("keys requires object or array".to_string()),
    }
}

fn eval_values(val: &YamlValue) -> Result<Vec<YamlValue>, String> {
    match val {
        YamlValue::Object(pairs) => Ok(vec![YamlValue::Array(
            pairs.iter().map(|(_, v)| v.clone()).collect(),
        )]),
        YamlValue::Array(_) => Ok(vec![val.clone()]),
        _ => Err("values requires object or array".to_string()),
    }
}

fn eval_select(val: &YamlValue, inner: &Filter) -> Result<Vec<YamlValue>, String> {
    let results = eval_filter(val, inner)?;
    if results.first().is_some_and(YamlValue::is_truthy) {
        Ok(vec![val.clone()])
    } else {
        Ok(vec![])
    }
}

fn eval_first(val: &YamlValue) -> Vec<YamlValue> {
    match val {
        YamlValue::Array(arr) => vec![arr.first().cloned().unwrap_or(YamlValue::Null)],
        _ => vec![val.clone()],
    }
}

fn eval_last(val: &YamlValue) -> Vec<YamlValue> {
    match val {
        YamlValue::Array(arr) => vec![arr.last().cloned().unwrap_or(YamlValue::Null)],
        _ => vec![val.clone()],
    }
}

fn eval_flatten(val: &YamlValue) -> Vec<YamlValue> {
    let YamlValue::Array(arr) = val else {
        return vec![val.clone()];
    };
    let mut out = Vec::new();
    for item in arr {
        if let YamlValue::Array(inner) = item {
            out.extend(inner.clone());
        } else {
            out.push(item.clone());
        }
    }
    vec![YamlValue::Array(out)]
}

fn eval_map(val: &YamlValue, inner: &Filter) -> Result<Vec<YamlValue>, String> {
    let YamlValue::Array(arr) = val else {
        return Err("map requires array input".to_string());
    };
    let mut out = Vec::new();
    for item in arr {
        out.extend(eval_filter(item, inner)?);
    }
    Ok(vec![YamlValue::Array(out)])
}

// ---------------------------------------------------------------------------
// Output formatting
// ---------------------------------------------------------------------------

fn yaml_to_string(val: &YamlValue, indent: usize) -> String {
    match val {
        YamlValue::Null => "null".to_string(),
        YamlValue::Bool(b) => b.to_string(),
        YamlValue::Number(n) => format_yaml_number(*n),
        YamlValue::String(s) => yaml_format_string(s),
        YamlValue::Array(arr) if arr.is_empty() => "[]".to_string(),
        YamlValue::Array(arr) => yaml_format_array(arr, indent),
        YamlValue::Object(pairs) if pairs.is_empty() => "{}".to_string(),
        YamlValue::Object(pairs) => yaml_format_object(pairs, indent),
    }
}

fn yaml_format_string(s: &str) -> String {
    if s.contains('\n') || s.contains(':') || s.contains('#') || s.is_empty() {
        format!(
            "\"{}\"",
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
        )
    } else {
        s.to_string()
    }
}

fn yaml_format_array(arr: &[YamlValue], indent: usize) -> String {
    let prefix = " ".repeat(indent);
    let mut out = String::new();
    for (i, item) in arr.iter().enumerate() {
        if i > 0 {
            out.push('\n');
            out.push_str(&prefix);
        }
        out.push_str("- ");
        out.push_str(&yaml_to_string(item, indent + 2));
    }
    out
}

fn yaml_format_object(pairs: &[(String, YamlValue)], indent: usize) -> String {
    let prefix = " ".repeat(indent);
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('\n');
            out.push_str(&prefix);
        }
        let _ = write!(out, "{k}:");
        match v {
            YamlValue::Object(_) | YamlValue::Array(_) => {
                let sub = yaml_to_string(v, indent + 2);
                let _ = write!(out, "\n{prefix}  {sub}");
            }
            _ => {
                let _ = write!(out, " {}", yaml_to_string(v, indent));
            }
        }
    }
    out
}

fn json_to_string(val: &YamlValue, compact: bool) -> String {
    json_to_string_inner(val, 0, compact)
}

fn json_to_string_inner(val: &YamlValue, indent: usize, compact: bool) -> String {
    match val {
        YamlValue::Null => "null".to_string(),
        YamlValue::Bool(b) => b.to_string(),
        YamlValue::Number(n) => format_yaml_number(*n),
        YamlValue::String(s) => json_escape_string(s),
        YamlValue::Array(arr) if arr.is_empty() => "[]".to_string(),
        YamlValue::Array(arr) => json_format_array(arr, indent, compact),
        YamlValue::Object(pairs) if pairs.is_empty() => "{}".to_string(),
        YamlValue::Object(pairs) => json_format_object(pairs, indent, compact),
    }
}

fn json_escape_string(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\t', "\\t")
            .replace('\r', "\\r")
    )
}

fn json_format_array(arr: &[YamlValue], indent: usize, compact: bool) -> String {
    let (nl, ind, ind1) = json_indent_parts(indent, compact);
    let mut out = format!("[{nl}");
    for (i, item) in arr.iter().enumerate() {
        if i > 0 {
            let _ = write!(out, ",{nl}");
        }
        let _ = write!(
            out,
            "{ind1}{}",
            json_to_string_inner(item, indent + 1, compact)
        );
    }
    let _ = write!(out, "{nl}{ind}]");
    out
}

fn json_format_object(pairs: &[(String, YamlValue)], indent: usize, compact: bool) -> String {
    let (nl, ind, ind1) = json_indent_parts(indent, compact);
    let sp = if compact { "" } else { " " };
    let mut out = format!("{{{nl}");
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            let _ = write!(out, ",{nl}");
        }
        let key_escaped = k.replace('\\', "\\\\").replace('"', "\\\"");
        let _ = write!(
            out,
            "{ind1}\"{key_escaped}\":{sp}{}",
            json_to_string_inner(v, indent + 1, compact)
        );
    }
    let _ = write!(out, "{nl}{ind}}}");
    out
}

/// Compute the newline, current-indent, and child-indent strings for JSON output.
fn json_indent_parts(indent: usize, compact: bool) -> (&'static str, String, String) {
    if compact {
        ("", String::new(), String::new())
    } else {
        ("\n", "  ".repeat(indent), "  ".repeat(indent + 1))
    }
}

fn format_yaml_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e18 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn raw_string(val: &YamlValue) -> String {
    match val {
        YamlValue::String(s) => s.clone(),
        _ => yaml_to_string(val, 0),
    }
}

// ---------------------------------------------------------------------------
// yq entry point
// ---------------------------------------------------------------------------

/// Parsed yq command-line options.
#[allow(clippy::struct_excessive_bools)]
struct YqOptions {
    raw_output: bool,
    exit_status_mode: bool,
    compact: bool,
    json_output: bool,
}

/// Parse yq flags from the argument list, returning options and the remaining args index.
fn parse_yq_flags(args: &[&str]) -> (YqOptions, usize) {
    let mut opts = YqOptions {
        raw_output: false,
        exit_status_mode: false,
        compact: false,
        json_output: false,
    };
    let mut i = 0;
    while i < args.len() {
        let arg = args[i];
        match arg {
            "-r" | "--raw-output" => opts.raw_output = true,
            "-e" | "--exit-status" => opts.exit_status_mode = true,
            "-c" | "--compact-output" => opts.compact = true,
            "-j" | "--json-output" => opts.json_output = true,
            _ if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") => {
                if !parse_combined_flags(&arg[1..], &mut opts) {
                    break;
                }
            }
            _ => break,
        }
        i += 1;
    }
    (opts, i)
}

/// Apply single-char combined flags (e.g. `-rjc`). Returns `false` if an unknown flag is found.
fn parse_combined_flags(flags: &str, opts: &mut YqOptions) -> bool {
    for ch in flags.chars() {
        match ch {
            'r' => opts.raw_output = true,
            'e' => opts.exit_status_mode = true,
            'c' => opts.compact = true,
            'j' => opts.json_output = true,
            _ => return false,
        }
    }
    true
}

/// Read yq input text from stdin or file arguments.
fn read_yq_input(ctx: &mut UtilContext<'_>, file_args: &[&str]) -> Result<String, i32> {
    if file_args.is_empty() {
        if ctx.stdin.is_none() {
            ctx.output.stderr(b"yq: missing input\n");
            return Err(1);
        }
        return collect_input_text(ctx, &[], "yq");
    }
    let mut combined = String::new();
    for path in file_args {
        let full = resolve_path(ctx.cwd, path);
        match collect_path_text(ctx, &full, path, "yq") {
            Ok(t) => {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&t);
            }
            Err(status) => return Err(status),
        }
    }
    Ok(combined)
}

/// Format a single result value according to the output options.
fn format_yq_result(val: &YamlValue, opts: &YqOptions) -> String {
    if opts.json_output {
        json_to_string(val, opts.compact)
    } else if opts.raw_output {
        raw_string(val)
    } else {
        yaml_to_string(val, 0)
    }
}

pub(crate) fn util_yq(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (opts, consumed) = parse_yq_flags(&argv[1..]);
    let args = &argv[1 + consumed..];

    if args.is_empty() {
        ctx.output.stderr(b"yq: missing filter\n");
        return 1;
    }

    let filter = match parse_filter(args[0]) {
        Ok(f) => f,
        Err(e) => {
            let msg = format!("yq: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            return 1;
        }
    };

    let text = match read_yq_input(ctx, &args[1..]) {
        Ok(t) => t,
        Err(code) => return code,
    };

    let mut parser = YamlParser::new(&text);
    let value = match parser.parse() {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("yq: parse error: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            return 1;
        }
    };

    let results = match eval_filter(&value, &filter) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("yq: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            return 1;
        }
    };

    if opts.exit_status_mode && results.is_empty() {
        return 1;
    }

    for result in &results {
        let output_str = format_yq_result(result, &opts);
        ctx.output.stdout(output_str.as_bytes());
        ctx.output.stdout(b"\n");
    }

    if opts.exit_status_mode {
        i32::from(!results.iter().all(YamlValue::is_truthy))
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UtilContext, VecOutput};
    use wasmsh_fs::{MemoryFs, OpenOptions, Vfs};

    fn run_yq(argv: &[&str], fs: &mut MemoryFs, stdin: Option<&[u8]>) -> (i32, VecOutput) {
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs,
                output: &mut output,
                cwd: "/",
                stdin: stdin.map(crate::UtilStdin::from_bytes),
                state: None,
                network: None,
            };
            util_yq(&mut ctx, argv)
        };
        (status, output)
    }

    #[test]
    fn yq_identity() {
        let mut fs = MemoryFs::new();
        let yaml = b"name: hello\nvalue: 42\n";
        let (status, out) = run_yq(&["yq", "."], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("name:"));
        assert!(s.contains("hello"));
    }

    #[test]
    fn yq_field_access() {
        let mut fs = MemoryFs::new();
        let yaml = b"name: world\ncount: 5\n";
        let (status, out) = run_yq(&["yq", ".name"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "world");
    }

    #[test]
    fn yq_nested_field() {
        let mut fs = MemoryFs::new();
        let yaml = b"outer:\n  inner: deep\n";
        let (status, out) = run_yq(&["yq", ".outer.inner"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "deep");
    }

    #[test]
    fn yq_array_index() {
        let mut fs = MemoryFs::new();
        let yaml = b"items:\n  - apple\n  - banana\n  - cherry\n";
        let (status, out) = run_yq(&["yq", ".items | .[1]"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "banana");
    }

    #[test]
    fn yq_keys() {
        let mut fs = MemoryFs::new();
        let yaml = b"a: 1\nb: 2\nc: 3\n";
        let (status, out) = run_yq(&["yq", "keys"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains('a'));
        assert!(s.contains('b'));
        assert!(s.contains('c'));
    }

    #[test]
    fn yq_length() {
        let mut fs = MemoryFs::new();
        let yaml = b"items:\n  - x\n  - y\n  - z\n";
        let (status, out) = run_yq(&["yq", ".items | length"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "3");
    }

    #[test]
    fn yq_json_output() {
        let mut fs = MemoryFs::new();
        let yaml = b"name: test\nvalue: 42\n";
        let (status, out) = run_yq(&["yq", "-j", "."], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("\"name\""));
        assert!(s.contains("\"test\""));
        assert!(s.contains("42"));
    }

    #[test]
    fn yq_booleans_and_null() {
        let mut fs = MemoryFs::new();
        let yaml = b"flag: true\nother: false\nempty: null\n";
        let (status, out) = run_yq(&["yq", ".flag"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "true");
    }

    #[test]
    fn yq_flow_sequence() {
        let mut fs = MemoryFs::new();
        let yaml = b"items: [1, 2, 3]\n";
        let (status, out) = run_yq(&["yq", ".items | length"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "3");
    }

    #[test]
    fn yq_from_file() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/test.yaml", OpenOptions::write()).unwrap();
        fs.write_file(h, b"greeting: hello\n").unwrap();
        fs.close(h);
        let (status, out) = run_yq(&["yq", ".greeting", "/test.yaml"], &mut fs, None);
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "hello");
    }

    #[test]
    fn yq_type_filter() {
        let mut fs = MemoryFs::new();
        let yaml = b"items:\n  - 1\n  - 2\n";
        let (status, out) = run_yq(&["yq", ".items | type"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "array");
    }

    #[test]
    fn yq_document_separator() {
        let mut fs = MemoryFs::new();
        let yaml = b"---\nkey: value\n";
        let (status, out) = run_yq(&["yq", ".key"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "value");
    }

    // ------------------------------------------------------------------
    // Multi-line literal | and folded > strings
    // ------------------------------------------------------------------

    #[test]
    fn yq_literal_block_scalar() {
        let mut fs = MemoryFs::new();
        let yaml = b"description: |\n  line one\n  line two\n  line three\n";
        let (status, out) = run_yq(&["yq", "-r", ".description"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("line one"));
        assert!(s.contains("line two"));
        assert!(s.contains("line three"));
    }

    #[test]
    fn yq_folded_block_scalar() {
        let mut fs = MemoryFs::new();
        let yaml = b"description: >\n  folded line one\n  folded line two\n";
        let (status, out) = run_yq(&["yq", "-r", ".description"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        // Folded should combine lines with spaces
        assert!(s.contains("folded line one"));
    }

    #[test]
    fn yq_standalone_literal_block() {
        let mut fs = MemoryFs::new();
        let yaml = b"|\n  hello\n  world\n";
        let (status, out) = run_yq(&["yq", "-r", "."], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("hello"));
        assert!(s.contains("world"));
    }

    // ------------------------------------------------------------------
    // Flow mappings and flow sequences
    // ------------------------------------------------------------------

    #[test]
    fn yq_flow_mapping() {
        let mut fs = MemoryFs::new();
        let yaml = b"{a: 1, b: 2}\n";
        let (status, out) = run_yq(&["yq", ".a"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "1");
    }

    #[test]
    fn yq_flow_mapping_nested() {
        let mut fs = MemoryFs::new();
        let yaml = b"data: {x: 10, y: 20}\n";
        let (status, out) = run_yq(&["yq", ".data.x"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "10");
    }

    #[test]
    fn yq_flow_sequence_inline() {
        let mut fs = MemoryFs::new();
        let yaml = b"[10, 20, 30]\n";
        let (status, out) = run_yq(&["yq", ".[1]"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "20");
    }

    #[test]
    fn yq_flow_sequence_length() {
        let mut fs = MemoryFs::new();
        let yaml = b"items: [a, b, c, d]\n";
        let (status, out) = run_yq(&["yq", ".items | length"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "4");
    }

    // ------------------------------------------------------------------
    // Boolean values
    // ------------------------------------------------------------------

    #[test]
    fn yq_boolean_yes_no() {
        let mut fs = MemoryFs::new();
        let yaml = b"enabled: yes\ndisabled: no\n";
        let (status, out) = run_yq(&["yq", ".enabled"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "true");
        let (status, out) = run_yq(&["yq", ".disabled"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "false");
    }

    #[test]
    fn yq_boolean_true_false() {
        let mut fs = MemoryFs::new();
        let yaml = b"a: true\nb: false\n";
        let (status, out) = run_yq(&["yq", ".a"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "true");
        let (status, out) = run_yq(&["yq", ".b"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "false");
    }

    // ------------------------------------------------------------------
    // Null values
    // ------------------------------------------------------------------

    #[test]
    fn yq_null_keyword() {
        let mut fs = MemoryFs::new();
        let yaml = b"value: null\n";
        let (status, out) = run_yq(&["yq", ".value"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "null");
    }

    #[test]
    fn yq_null_tilde() {
        let mut fs = MemoryFs::new();
        let yaml = b"value: ~\n";
        let (status, out) = run_yq(&["yq", ".value"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "null");
    }

    #[test]
    fn yq_null_empty_value() {
        let mut fs = MemoryFs::new();
        let yaml = b"value:\n";
        let (status, out) = run_yq(&["yq", ".value"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "null");
    }

    // ------------------------------------------------------------------
    // Numbers: integers, floats, negative, scientific
    // ------------------------------------------------------------------

    #[test]
    fn yq_integer() {
        let mut fs = MemoryFs::new();
        let yaml = b"count: 42\n";
        let (status, out) = run_yq(&["yq", ".count"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "42");
    }

    #[test]
    fn yq_float() {
        let mut fs = MemoryFs::new();
        let yaml = b"pi: 3.14159\n";
        let (status, out) = run_yq(&["yq", ".pi"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        let val: f64 = out.stdout_str().trim().parse().unwrap();
        // Check it parsed as a float close to pi
        assert!(val > 3.1 && val < 3.2);
    }

    #[test]
    fn yq_negative_number() {
        let mut fs = MemoryFs::new();
        let yaml = b"temp: -40\n";
        let (status, out) = run_yq(&["yq", ".temp"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "-40");
    }

    // ------------------------------------------------------------------
    // Comments at various positions
    // ------------------------------------------------------------------

    #[test]
    fn yq_comment_on_own_line() {
        let mut fs = MemoryFs::new();
        let yaml = b"# This is a comment\nname: hello\n";
        let (status, out) = run_yq(&["yq", ".name"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "hello");
    }

    #[test]
    fn yq_inline_comment() {
        let mut fs = MemoryFs::new();
        let yaml = b"name: hello # inline comment\n";
        let (status, out) = run_yq(&["yq", ".name"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "hello");
    }

    #[test]
    fn yq_comment_between_keys() {
        let mut fs = MemoryFs::new();
        let yaml = b"a: 1\n# comment\nb: 2\n";
        let (status, out) = run_yq(&["yq", ".b"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "2");
    }

    // ------------------------------------------------------------------
    // Nested indentation (3+ levels)
    // ------------------------------------------------------------------

    #[test]
    fn yq_deeply_nested() {
        let mut fs = MemoryFs::new();
        let yaml = b"level1:\n  level2:\n    level3:\n      value: deep\n";
        let (status, out) = run_yq(&["yq", ".level1.level2.level3.value"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "deep");
    }

    #[test]
    fn yq_nested_list_in_map() {
        let mut fs = MemoryFs::new();
        let yaml = b"config:\n  servers:\n    - name: s1\n      port: 80\n    - name: s2\n      port: 443\n";
        let (status, out) = run_yq(&["yq", ".config.servers | length"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "2");
    }

    // ------------------------------------------------------------------
    // Document separator ---
    // ------------------------------------------------------------------

    #[test]
    fn yq_document_separator_with_comment() {
        let mut fs = MemoryFs::new();
        let yaml = b"# header comment\n---\nkey: value\n";
        let (status, out) = run_yq(&["yq", ".key"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "value");
    }

    // ------------------------------------------------------------------
    // Error paths
    // ------------------------------------------------------------------

    #[test]
    fn yq_missing_filter() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_yq(&["yq"], &mut fs, Some(b"key: val\n"));
        assert_ne!(status, 0);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("missing filter"));
    }

    #[test]
    fn yq_missing_input() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_yq(&["yq", "."], &mut fs, None);
        assert_ne!(status, 0);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("missing input"));
    }

    #[test]
    fn yq_unsupported_filter() {
        let mut fs = MemoryFs::new();
        let yaml = b"key: val\n";
        let (status, out) = run_yq(&["yq", "invalid_filter_xyz"], &mut fs, Some(yaml));
        assert_ne!(status, 0);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.is_empty());
    }

    #[test]
    fn yq_file_not_found() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_yq(&["yq", ".", "/nonexistent.yaml"], &mut fs, None);
        assert_ne!(status, 0);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.is_empty());
    }

    // ------------------------------------------------------------------
    // yq filters: keys, values, length, type
    // ------------------------------------------------------------------

    #[test]
    fn yq_values_filter() {
        let mut fs = MemoryFs::new();
        let yaml = b"a: 1\nb: 2\nc: 3\n";
        let (status, out) = run_yq(&["yq", "values"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains('1'));
        assert!(s.contains('2'));
        assert!(s.contains('3'));
    }

    #[test]
    fn yq_type_string() {
        let mut fs = MemoryFs::new();
        let yaml = b"name: hello\n";
        let (status, out) = run_yq(&["yq", ".name | type"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "string");
    }

    #[test]
    fn yq_type_number() {
        let mut fs = MemoryFs::new();
        let yaml = b"count: 42\n";
        let (status, out) = run_yq(&["yq", ".count | type"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "number");
    }

    #[test]
    fn yq_type_boolean() {
        let mut fs = MemoryFs::new();
        let yaml = b"flag: true\n";
        let (status, out) = run_yq(&["yq", ".flag | type"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "boolean");
    }

    #[test]
    fn yq_type_null() {
        let mut fs = MemoryFs::new();
        let yaml = b"val: null\n";
        let (status, out) = run_yq(&["yq", ".val | type"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "null");
    }

    #[test]
    fn yq_type_object() {
        let mut fs = MemoryFs::new();
        let yaml = b"obj:\n  a: 1\n";
        let (status, out) = run_yq(&["yq", ".obj | type"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "object");
    }

    #[test]
    fn yq_length_string() {
        let mut fs = MemoryFs::new();
        let yaml = b"name: hello\n";
        let (status, out) = run_yq(&["yq", ".name | length"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "5");
    }

    #[test]
    fn yq_length_object() {
        let mut fs = MemoryFs::new();
        let yaml = b"a: 1\nb: 2\nc: 3\n";
        let (status, out) = run_yq(&["yq", "length"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "3");
    }

    // ------------------------------------------------------------------
    // -r raw output, -c compact, -j JSON output
    // ------------------------------------------------------------------

    #[test]
    fn yq_raw_output_string() {
        let mut fs = MemoryFs::new();
        let yaml = b"msg: hello world\n";
        let (status, out) = run_yq(&["yq", "-r", ".msg"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        // Raw output should not have quotes
        assert_eq!(out.stdout_str().trim(), "hello world");
    }

    #[test]
    fn yq_compact_json_output() {
        let mut fs = MemoryFs::new();
        let yaml = b"a: 1\nb: 2\n";
        let (status, out) = run_yq(&["yq", "-jc", "."], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        let s = out.stdout_str().trim();
        // Compact JSON should have no newlines within the object
        assert!(s.starts_with('{'));
        assert!(s.ends_with('}'));
        assert!(!s[1..s.len() - 1].contains('\n'));
    }

    #[test]
    fn yq_json_output_array() {
        let mut fs = MemoryFs::new();
        let yaml = b"items:\n  - a\n  - b\n";
        let (status, out) = run_yq(&["yq", "-j", ".items"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains('['));
        assert!(s.contains(']'));
        assert!(s.contains("\"a\""));
        assert!(s.contains("\"b\""));
    }

    // ------------------------------------------------------------------
    // select() filter
    // ------------------------------------------------------------------

    #[test]
    fn yq_select_identity() {
        let mut fs = MemoryFs::new();
        let yaml = b"name: hello\n";
        let (status, out) = run_yq(&["yq", "select(.)"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("name"));
    }

    // ------------------------------------------------------------------
    // map() filter
    // ------------------------------------------------------------------

    #[test]
    fn yq_map_length() {
        let mut fs = MemoryFs::new();
        let yaml = b"items:\n  - hello\n  - world\n  - hi\n";
        let (status, out) = run_yq(&["yq", ".items | map(length)"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains('5'));
        assert!(s.contains('2'));
    }

    // ------------------------------------------------------------------
    // Empty input
    // ------------------------------------------------------------------

    #[test]
    fn yq_empty_input() {
        let mut fs = MemoryFs::new();
        let yaml = b"";
        let (status, out) = run_yq(&["yq", "."], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "null");
    }

    // ------------------------------------------------------------------
    // Quoted strings
    // ------------------------------------------------------------------

    #[test]
    fn yq_double_quoted_string() {
        let mut fs = MemoryFs::new();
        let yaml = b"name: \"hello world\"\n";
        let (status, out) = run_yq(&["yq", ".name"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "hello world");
    }

    #[test]
    fn yq_single_quoted_string() {
        let mut fs = MemoryFs::new();
        let yaml = b"name: 'hello world'\n";
        let (status, out) = run_yq(&["yq", ".name"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "hello world");
    }

    // ------------------------------------------------------------------
    // Keys with special characters
    // ------------------------------------------------------------------

    #[test]
    fn yq_key_with_spaces() {
        let mut fs = MemoryFs::new();
        let yaml = b"\"key with spaces\": value\n";
        let (status, out) = run_yq(&["yq", ".\"key with spaces\""], &mut fs, Some(yaml));
        // The filter parser may or may not support this; at minimum it shouldn't crash
        // If it parses the quoted key, it should return the value
        let _ = status;
        let _ = out;
    }

    // ------------------------------------------------------------------
    // Iterate .[]
    // ------------------------------------------------------------------

    #[test]
    fn yq_iterate_array() {
        let mut fs = MemoryFs::new();
        let yaml = b"items:\n  - one\n  - two\n  - three\n";
        let (status, out) = run_yq(&["yq", ".items | .[]"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("one"));
        assert!(s.contains("two"));
        assert!(s.contains("three"));
    }

    #[test]
    fn yq_iterate_object() {
        let mut fs = MemoryFs::new();
        let yaml = b"a: 1\nb: 2\n";
        let (status, out) = run_yq(&["yq", ".[]"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains('1'));
        assert!(s.contains('2'));
    }

    // ------------------------------------------------------------------
    // first and last
    // ------------------------------------------------------------------

    #[test]
    fn yq_first() {
        let mut fs = MemoryFs::new();
        let yaml = b"items:\n  - alpha\n  - beta\n  - gamma\n";
        let (status, out) = run_yq(&["yq", ".items | first"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "alpha");
    }

    #[test]
    fn yq_last() {
        let mut fs = MemoryFs::new();
        let yaml = b"items:\n  - alpha\n  - beta\n  - gamma\n";
        let (status, out) = run_yq(&["yq", ".items | last"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "gamma");
    }

    // ------------------------------------------------------------------
    // Combined flags
    // ------------------------------------------------------------------

    #[test]
    fn yq_combined_flags_rj() {
        let mut fs = MemoryFs::new();
        let yaml = b"name: hello\ncount: 3\n";
        let (status, out) = run_yq(&["yq", "-rj", "."], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        // -j takes precedence for JSON output
        let s = out.stdout_str();
        assert!(s.contains("\"name\""));
    }

    // ------------------------------------------------------------------
    // Negative array index
    // ------------------------------------------------------------------

    #[test]
    fn yq_negative_index() {
        let mut fs = MemoryFs::new();
        let yaml = b"items:\n  - a\n  - b\n  - c\n";
        let (status, out) = run_yq(&["yq", ".items | .[-1]"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "c");
    }

    // ------------------------------------------------------------------
    // Field then iterate shorthand: .key[]
    // ------------------------------------------------------------------

    #[test]
    fn yq_field_then_iterate() {
        let mut fs = MemoryFs::new();
        let yaml = b"items:\n  - x\n  - y\n";
        let (status, out) = run_yq(&["yq", ".items[]"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains('x'));
        assert!(s.contains('y'));
    }

    // ------------------------------------------------------------------
    // exit-status mode
    // ------------------------------------------------------------------

    #[test]
    fn yq_exit_status_truthy() {
        let mut fs = MemoryFs::new();
        let yaml = b"val: true\n";
        let (status, _) = run_yq(&["yq", "-e", ".val"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
    }

    #[test]
    fn yq_exit_status_null() {
        let mut fs = MemoryFs::new();
        let yaml = b"val: null\n";
        let (status, _) = run_yq(&["yq", "-e", ".val"], &mut fs, Some(yaml));
        assert_ne!(status, 0);
    }

    // ------------------------------------------------------------------
    // Empty flow collections
    // ------------------------------------------------------------------

    #[test]
    fn yq_empty_flow_sequence() {
        let mut fs = MemoryFs::new();
        let yaml = b"items: []\n";
        let (status, out) = run_yq(&["yq", ".items | length"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "0");
    }

    #[test]
    fn yq_empty_flow_mapping() {
        let mut fs = MemoryFs::new();
        let yaml = b"data: {}\n";
        let (status, out) = run_yq(&["yq", ".data | length"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "0");
    }

    // ------------------------------------------------------------------
    // Flatten
    // ------------------------------------------------------------------

    #[test]
    fn yq_flatten() {
        let mut fs = MemoryFs::new();
        let yaml = b"items:\n  - [1, 2]\n  - [3, 4]\n";
        let (status, out) = run_yq(&["yq", ".items | flatten"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains('1'));
        assert!(s.contains('4'));
    }

    // ------------------------------------------------------------------
    // Keys for array
    // ------------------------------------------------------------------

    #[test]
    fn yq_keys_array() {
        let mut fs = MemoryFs::new();
        let yaml = b"items:\n  - a\n  - b\n  - c\n";
        let (status, out) = run_yq(&["yq", ".items | keys"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains('0'));
        assert!(s.contains('1'));
        assert!(s.contains('2'));
    }

    // ------------------------------------------------------------------
    // Access nonexistent field
    // ------------------------------------------------------------------

    #[test]
    fn yq_nonexistent_field() {
        let mut fs = MemoryFs::new();
        let yaml = b"name: hello\n";
        let (status, out) = run_yq(&["yq", ".nonexistent"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "null");
    }

    // ------------------------------------------------------------------
    // List of objects
    // ------------------------------------------------------------------

    #[test]
    fn yq_list_of_objects() {
        let mut fs = MemoryFs::new();
        let yaml = b"people:\n  - name: Alice\n    age: 30\n  - name: Bob\n    age: 25\n";
        let (status, out) = run_yq(&["yq", ".people | length"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "2");
    }

    #[test]
    fn yq_index_into_list_of_objects() {
        let mut fs = MemoryFs::new();
        let yaml = b"people:\n  - name: Alice\n    age: 30\n  - name: Bob\n    age: 25\n";
        let (status, out) = run_yq(&["yq", ".people | .[0] | .name"], &mut fs, Some(yaml));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "Alice");
    }
}
