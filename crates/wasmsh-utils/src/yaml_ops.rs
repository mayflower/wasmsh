//! YAML utility: yq.

use std::fmt::Write;

use crate::helpers::{emit_error, read_text, resolve_path};
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
            if self.pos >= self.lines.len() {
                break;
            }

            let indent = self.current_indent();
            if indent != block_indent {
                break;
            }

            let line = self.lines[self.pos];
            let trimmed = line.trim();

            let Some(colon_pos) = find_colon_separator(trimmed) else {
                break;
            };

            let key = trimmed[..colon_pos].trim().to_string();
            let key = unquote_string(&key);
            let after_colon = trimmed[colon_pos + 1..].trim();

            if after_colon.is_empty() {
                // Value is on next line(s)
                self.pos += 1;
                self.skip_blanks_and_comments();
                if self.pos < self.lines.len() && self.current_indent() > block_indent {
                    let val = self.parse_value(block_indent + 1)?;
                    pairs.push((key, val));
                } else {
                    pairs.push((key, YamlValue::Null));
                }
            } else if after_colon == "|" || after_colon == ">" {
                self.pos += 1;
                let val = self.parse_multiline_string_body(after_colon == ">");
                pairs.push((key, val));
            } else {
                // Inline value
                let val = if after_colon.starts_with('[') {
                    self.pos += 1;
                    parse_inline_flow_sequence(after_colon)?
                } else if after_colon.starts_with('{') {
                    self.pos += 1;
                    parse_inline_flow_mapping(after_colon)?
                } else {
                    self.pos += 1;
                    parse_scalar(after_colon)
                };
                pairs.push((key, val));
            }
        }

        Ok(YamlValue::Object(pairs))
    }

    fn parse_block_list(&mut self, min_indent: usize) -> Result<YamlValue, String> {
        let mut items = Vec::new();
        let block_indent = self.current_indent();

        if block_indent < min_indent {
            return Ok(YamlValue::Null);
        }

        while self.pos < self.lines.len() {
            self.skip_blanks_and_comments();
            if self.pos >= self.lines.len() {
                break;
            }

            let indent = self.current_indent();
            if indent != block_indent {
                break;
            }

            let line = self.lines[self.pos];
            let trimmed = line.trim();

            if !trimmed.starts_with("- ") && trimmed != "-" {
                break;
            }

            let item_text = if trimmed == "-" { "" } else { &trimmed[2..] };

            if item_text.is_empty() {
                // Value on next line
                self.pos += 1;
                self.skip_blanks_and_comments();
                if self.pos < self.lines.len() && self.current_indent() > block_indent {
                    let val = self.parse_value(block_indent + 1)?;
                    items.push(val);
                } else {
                    items.push(YamlValue::Null);
                }
            } else if item_text.starts_with('[') {
                self.pos += 1;
                items.push(parse_inline_flow_sequence(item_text)?);
            } else if item_text.starts_with('{') {
                self.pos += 1;
                items.push(parse_inline_flow_mapping(item_text)?);
            } else if find_colon_separator(item_text).is_some() {
                // Inline mapping within list item
                // We need to handle this specially: the "- key: val" case
                // Create a temp parser-like state for this
                self.pos += 1;
                let first_val = parse_inline_mapping_item(item_text)?;

                // Check if there are more keys at deeper indent
                self.skip_blanks_and_comments();
                if self.pos < self.lines.len() && self.current_indent() > block_indent {
                    let deeper_indent = self.current_indent();
                    let mut pairs = match first_val {
                        YamlValue::Object(p) => p,
                        _ => vec![],
                    };
                    // Parse remaining keys at this deeper indent
                    while self.pos < self.lines.len() {
                        self.skip_blanks_and_comments();
                        if self.pos >= self.lines.len() {
                            break;
                        }
                        if self.current_indent() != deeper_indent {
                            break;
                        }
                        let sub_trimmed = self.lines[self.pos].trim();
                        if let Some(cp) = find_colon_separator(sub_trimmed) {
                            let k = unquote_string(sub_trimmed[..cp].trim());
                            let v_text = sub_trimmed[cp + 1..].trim();
                            if v_text.is_empty() {
                                self.pos += 1;
                                self.skip_blanks_and_comments();
                                if self.pos < self.lines.len()
                                    && self.current_indent() > deeper_indent
                                {
                                    let v = self.parse_value(deeper_indent + 1)?;
                                    pairs.push((k, v));
                                } else {
                                    pairs.push((k, YamlValue::Null));
                                }
                            } else {
                                self.pos += 1;
                                pairs.push((k, parse_scalar(v_text)));
                            }
                        } else {
                            break;
                        }
                    }
                    items.push(YamlValue::Object(pairs));
                } else {
                    items.push(first_val);
                }
            } else {
                self.pos += 1;
                items.push(parse_scalar(item_text));
            }
        }

        Ok(YamlValue::Array(items))
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
        let mut lines = Vec::new();
        let body_indent = if self.pos < self.lines.len() {
            let line = self.lines[self.pos];
            if line.trim().is_empty() {
                // Find first non-empty line to determine indent
                let mut idx = self.pos;
                while idx < self.lines.len() && self.lines[idx].trim().is_empty() {
                    idx += 1;
                }
                if idx < self.lines.len() {
                    let l = self.lines[idx];
                    l.len() - l.trim_start().len()
                } else {
                    return YamlValue::String(String::new());
                }
            } else {
                line.len() - line.trim_start().len()
            }
        } else {
            return YamlValue::String(String::new());
        };

        while self.pos < self.lines.len() {
            let line = self.lines[self.pos];
            let indent = line.len() - line.trim_start().len();
            if !line.trim().is_empty() && indent < body_indent {
                break;
            }
            if line.trim().is_empty() {
                lines.push("");
            } else {
                lines.push(&line[body_indent..]);
            }
            self.pos += 1;
        }

        // Remove trailing empty lines
        while lines.last() == Some(&"") {
            lines.pop();
        }

        let result = if folded {
            // Folded: newlines become spaces (except double newlines)
            let mut out = String::new();
            for (i, line) in lines.iter().enumerate() {
                if line.is_empty() {
                    out.push('\n');
                } else {
                    if i > 0 && !lines[i - 1].is_empty() && !out.ends_with('\n') {
                        out.push(' ');
                    }
                    out.push_str(line);
                }
            }
            out.push('\n');
            out
        } else {
            // Literal: preserve newlines
            let mut out = lines.join("\n");
            out.push('\n');
            out
        };

        YamlValue::String(result)
    }
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
        let trimmed = item.trim();
        if trimmed.starts_with('{') {
            result.push(parse_inline_flow_mapping(trimmed)?);
        } else if trimmed.starts_with('[') {
            result.push(parse_inline_flow_sequence(trimmed)?);
        } else {
            result.push(parse_scalar(trimmed));
        }
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
        let trimmed = item.trim();
        if let Some(colon_pos) = trimmed.find(": ") {
            let key = unquote_string(&trimmed[..colon_pos]);
            let val_str = trimmed[colon_pos + 2..].trim();
            let val = if val_str.starts_with('[') {
                parse_inline_flow_sequence(val_str)?
            } else if val_str.starts_with('{') {
                parse_inline_flow_mapping(val_str)?
            } else {
                parse_scalar(val_str)
            };
            pairs.push((key, val));
        } else if let Some(colon_pos) = trimmed.find(':') {
            // key:value without space (edge case)
            let key = unquote_string(&trimmed[..colon_pos]);
            let val_str = trimmed[colon_pos + 1..].trim();
            if val_str.is_empty() {
                pairs.push((key, YamlValue::Null));
            } else {
                pairs.push((key, parse_scalar(val_str)));
            }
        }
    }

    Ok(YamlValue::Object(pairs))
}

fn parse_inline_mapping_item(s: &str) -> Result<YamlValue, String> {
    if let Some(colon_pos) = find_colon_separator(s) {
        let key = unquote_string(s[..colon_pos].trim());
        let val_str = s[colon_pos + 1..].trim();
        let val = if val_str.is_empty() {
            YamlValue::Null
        } else if val_str.starts_with('[') {
            parse_inline_flow_sequence(val_str)?
        } else if val_str.starts_with('{') {
            parse_inline_flow_mapping(val_str)?
        } else {
            parse_scalar(val_str)
        };
        Ok(YamlValue::Object(vec![(key, val)]))
    } else {
        Ok(parse_scalar(s))
    }
}

/// Split flow items by commas, respecting nesting of `[]` and `{}`.
fn split_flow_items(s: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    for ch in s.chars() {
        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            current.push(ch);
        } else if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            current.push(ch);
        } else if !in_single_quote && !in_double_quote {
            if ch == '[' || ch == '{' {
                depth += 1;
                current.push(ch);
            } else if ch == ']' || ch == '}' {
                depth -= 1;
                current.push(ch);
            } else if ch == ',' && depth == 0 {
                items.push(current.clone());
                current.clear();
            } else {
                current.push(ch);
            }
        } else {
            current.push(ch);
        }
    }

    if !current.trim().is_empty() {
        items.push(current);
    }

    items
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

    // Built-in functions
    match input {
        "keys" => return Ok(Filter::Keys),
        "values" => return Ok(Filter::Values),
        "length" => return Ok(Filter::Length),
        "type" => return Ok(Filter::Type),
        "first" => return Ok(Filter::First),
        "last" => return Ok(Filter::Last),
        "flatten" => return Ok(Filter::Flatten),
        "not" => return Ok(Filter::Not),
        _ => {}
    }

    // select(...)
    if let Some(inner) = input
        .strip_prefix("select(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let f = parse_filter(inner)?;
        return Ok(Filter::Select(Box::new(f)));
    }

    // map(...)
    if let Some(inner) = input.strip_prefix("map(").and_then(|s| s.strip_suffix(')')) {
        let f = parse_filter(inner)?;
        return Ok(Filter::Map(Box::new(f)));
    }

    // .[] — iterate
    if input == ".[]" {
        return Ok(Filter::Iterate);
    }

    // .[N] — index
    if input.starts_with(".[") && input.ends_with(']') {
        let idx_str = &input[2..input.len() - 1];
        if let Ok(idx) = idx_str.parse::<i64>() {
            return Ok(Filter::Index(idx));
        }
    }

    // .key or .key.subkey.subsubkey
    if let Some(rest) = input.strip_prefix('.') {
        // Check for chained field access: .a.b.c
        let parts: Vec<&str> = rest.split('.').collect();
        if parts.len() > 1 && parts.iter().all(|p| !p.is_empty() && !p.contains('[')) {
            return Ok(Filter::FieldChain(
                parts.iter().map(|s| (*s).to_string()).collect(),
            ));
        }
        if !rest.is_empty() && !rest.contains('[') && !rest.contains('|') {
            return Ok(Filter::Field(rest.to_string()));
        }

        // .key[] — field then iterate
        if let Some(field) = rest.strip_suffix("[]") {
            if !field.is_empty() {
                return Ok(Filter::Pipe(
                    Box::new(Filter::Field(field.to_string())),
                    Box::new(Filter::Iterate),
                ));
            }
        }

        // .key[N]
        if rest.contains('[') && rest.ends_with(']') {
            let bracket = rest.find('[').unwrap();
            let field = &rest[..bracket];
            let idx_str = &rest[bracket + 1..rest.len() - 1];
            if let Ok(idx) = idx_str.parse::<i64>() {
                return Ok(Filter::Pipe(
                    Box::new(Filter::Field(field.to_string())),
                    Box::new(Filter::Index(idx)),
                ));
            }
        }
    }

    Err(format!("unsupported filter: {input}"))
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

        Filter::FieldChain(keys) => {
            let mut current = val.clone();
            for key in keys {
                current = current.obj_get(key);
            }
            Ok(vec![current])
        }

        Filter::Index(idx) => {
            if let YamlValue::Array(arr) = val {
                let i = if *idx < 0 {
                    arr.len().wrapping_add(*idx as usize)
                } else {
                    *idx as usize
                };
                Ok(vec![arr.get(i).cloned().unwrap_or(YamlValue::Null)])
            } else {
                Ok(vec![YamlValue::Null])
            }
        }

        Filter::Iterate => match val {
            YamlValue::Array(arr) => Ok(arr.clone()),
            YamlValue::Object(pairs) => Ok(pairs.iter().map(|(_, v)| v.clone()).collect()),
            _ => Err(format!("cannot iterate over {}", val.type_name())),
        },

        Filter::Pipe(left, right) => {
            let intermediate = eval_filter(val, left)?;
            let mut results = Vec::new();
            for v in &intermediate {
                results.extend(eval_filter(v, right)?);
            }
            Ok(results)
        }

        Filter::Keys => match val {
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
        },

        Filter::Values => match val {
            YamlValue::Object(pairs) => Ok(vec![YamlValue::Array(
                pairs.iter().map(|(_, v)| v.clone()).collect(),
            )]),
            YamlValue::Array(_) => Ok(vec![val.clone()]),
            _ => Err("values requires object or array".to_string()),
        },

        Filter::Length => Ok(vec![val.length()]),

        Filter::Type => Ok(vec![YamlValue::String(val.type_name().to_string())]),

        Filter::Select(inner) => {
            let results = eval_filter(val, inner)?;
            if results.first().is_some_and(YamlValue::is_truthy) {
                Ok(vec![val.clone()])
            } else {
                Ok(vec![])
            }
        }

        Filter::First => match val {
            YamlValue::Array(arr) => Ok(vec![arr.first().cloned().unwrap_or(YamlValue::Null)]),
            _ => Ok(vec![val.clone()]),
        },

        Filter::Last => match val {
            YamlValue::Array(arr) => Ok(vec![arr.last().cloned().unwrap_or(YamlValue::Null)]),
            _ => Ok(vec![val.clone()]),
        },

        Filter::Flatten => match val {
            YamlValue::Array(arr) => {
                let mut out = Vec::new();
                for item in arr {
                    if let YamlValue::Array(inner) = item {
                        out.extend(inner.clone());
                    } else {
                        out.push(item.clone());
                    }
                }
                Ok(vec![YamlValue::Array(out)])
            }
            _ => Ok(vec![val.clone()]),
        },

        Filter::Map(inner) => match val {
            YamlValue::Array(arr) => {
                let mut out = Vec::new();
                for item in arr {
                    let results = eval_filter(item, inner)?;
                    out.extend(results);
                }
                Ok(vec![YamlValue::Array(out)])
            }
            _ => Err("map requires array input".to_string()),
        },

        Filter::Not => {
            let truthy = val.is_truthy();
            Ok(vec![YamlValue::Bool(!truthy)])
        }
    }
}

// ---------------------------------------------------------------------------
// Output formatting
// ---------------------------------------------------------------------------

fn yaml_to_string(val: &YamlValue, indent: usize) -> String {
    let prefix = " ".repeat(indent);
    match val {
        YamlValue::Null => "null".to_string(),
        YamlValue::Bool(b) => b.to_string(),
        YamlValue::Number(n) => format_yaml_number(*n),
        YamlValue::String(s) => {
            if s.contains('\n') || s.contains(':') || s.contains('#') || s.is_empty() {
                format!(
                    "\"{}\"",
                    s.replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace('\n', "\\n")
                )
            } else {
                s.clone()
            }
        }
        YamlValue::Array(arr) if arr.is_empty() => "[]".to_string(),
        YamlValue::Array(arr) => {
            let mut out = String::new();
            for (i, item) in arr.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                    out.push_str(&prefix);
                }
                out.push_str("- ");
                match item {
                    YamlValue::Object(_) | YamlValue::Array(_) => {
                        let sub = yaml_to_string(item, indent + 2);
                        out.push_str(&sub);
                    }
                    _ => {
                        out.push_str(&yaml_to_string(item, indent + 2));
                    }
                }
            }
            out
        }
        YamlValue::Object(pairs) if pairs.is_empty() => "{}".to_string(),
        YamlValue::Object(pairs) => {
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
    }
}

fn json_to_string(val: &YamlValue, compact: bool) -> String {
    json_to_string_inner(val, 0, compact)
}

fn json_to_string_inner(val: &YamlValue, indent: usize, compact: bool) -> String {
    let nl = if compact { "" } else { "\n" };
    let sp = if compact { "" } else { " " };
    let ind = if compact {
        String::new()
    } else {
        "  ".repeat(indent)
    };
    let ind1 = if compact {
        String::new()
    } else {
        "  ".repeat(indent + 1)
    };

    match val {
        YamlValue::Null => "null".to_string(),
        YamlValue::Bool(b) => b.to_string(),
        YamlValue::Number(n) => format_yaml_number(*n),
        YamlValue::String(s) => {
            format!(
                "\"{}\"",
                s.replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n")
                    .replace('\t', "\\t")
                    .replace('\r', "\\r")
            )
        }
        YamlValue::Array(arr) if arr.is_empty() => "[]".to_string(),
        YamlValue::Array(arr) => {
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
        YamlValue::Object(pairs) if pairs.is_empty() => "{}".to_string(),
        YamlValue::Object(pairs) => {
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

pub(crate) fn util_yq(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut raw_output = false;
    let mut exit_status_mode = false;
    let mut compact = false;
    let mut json_output = false;

    while let Some(arg) = args.first() {
        match *arg {
            "-r" | "--raw-output" => {
                raw_output = true;
                args = &args[1..];
            }
            "-e" | "--exit-status" => {
                exit_status_mode = true;
                args = &args[1..];
            }
            "-c" | "--compact-output" => {
                compact = true;
                args = &args[1..];
            }
            "-j" | "--json-output" => {
                json_output = true;
                args = &args[1..];
            }
            _ if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") => {
                let flags = &arg[1..];
                let mut recognized = true;
                for ch in flags.chars() {
                    match ch {
                        'r' => raw_output = true,
                        'e' => exit_status_mode = true,
                        'c' => compact = true,
                        'j' => json_output = true,
                        _ => {
                            recognized = false;
                            break;
                        }
                    }
                }
                if recognized {
                    args = &args[1..];
                } else {
                    break;
                }
            }
            _ => break,
        }
    }

    if args.is_empty() {
        ctx.output.stderr(b"yq: missing filter\n");
        return 1;
    }

    let filter_str = args[0];
    let file_args = &args[1..];

    // Parse the filter
    let filter = match parse_filter(filter_str) {
        Ok(f) => f,
        Err(e) => {
            let msg = format!("yq: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            return 1;
        }
    };

    // Get input text
    let text = if file_args.is_empty() {
        if let Some(data) = ctx.stdin {
            String::from_utf8_lossy(data).to_string()
        } else {
            ctx.output.stderr(b"yq: missing input\n");
            return 1;
        }
    } else {
        let mut combined = String::new();
        for path in file_args {
            let full = resolve_path(ctx.cwd, path);
            match read_text(ctx.fs, &full) {
                Ok(t) => {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str(&t);
                }
                Err(e) => {
                    emit_error(ctx.output, "yq", path, &e);
                    return 1;
                }
            }
        }
        combined
    };

    // Parse YAML
    let mut parser = YamlParser::new(&text);
    let value = match parser.parse() {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("yq: parse error: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            return 1;
        }
    };

    // Apply filter
    let results = match eval_filter(&value, &filter) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("yq: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            return 1;
        }
    };

    if exit_status_mode && results.is_empty() {
        return 1;
    }

    for result in &results {
        let output_str = if json_output {
            json_to_string(result, compact)
        } else if raw_output {
            raw_string(result)
        } else {
            yaml_to_string(result, 0)
        };
        ctx.output.stdout(output_str.as_bytes());
        ctx.output.stdout(b"\n");
    }

    if exit_status_mode {
        let all_truthy = results.iter().all(YamlValue::is_truthy);
        i32::from(!all_truthy)
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
                stdin,
                state: None,
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
        assert!(s.contains("a"));
        assert!(s.contains("b"));
        assert!(s.contains("c"));
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
}
