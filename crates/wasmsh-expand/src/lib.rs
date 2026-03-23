//! Word expansion engine for the wasmsh shell.
//!
//! Performs expansions on structured `Word`/`WordPart` nodes in the
//! correct POSIX order:
//! 1. Tilde expansion
//! 2. Parameter expansion
//! 3. Command substitution (placeholder — requires VM callback)
//! 4. Arithmetic expansion (placeholder — basic integer eval)
//! 5. Field splitting
//! 6. Pathname expansion / globbing (not yet)
//! 7. Quote removal

use smol_str::SmolStr;
use wasmsh_ast::{Word, WordPart};
use wasmsh_state::ShellState;

/// The result of expanding a word into zero or more fields.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpandedFields {
    pub fields: Vec<String>,
}

/// Expand a single `Word` AST node into a string, performing parameter
/// expansion and quote removal. Returns a single string (no field splitting).
pub fn expand_word(word: &Word, state: &mut ShellState) -> String {
    let mut out = String::new();
    for part in &word.parts {
        expand_part(part, state, &mut out);
    }
    // Tilde expansion: ~ at start of word → $HOME
    if out.starts_with('~') {
        if out == "~" || out.starts_with("~/") {
            if let Some(home) = state.get_var("HOME") {
                out = format!("{home}{}", &out[1..]);
            }
        }
    }
    out
}

/// Expand a word into multiple fields (after field splitting on IFS).
pub fn expand_word_split(word: &Word, state: &mut ShellState) -> ExpandedFields {
    let expanded = expand_word(word, state);
    let ifs = state
        .get_var("IFS")
        .unwrap_or_else(|| SmolStr::from(" \t\n"));

    if expanded.is_empty() {
        return ExpandedFields {
            fields: Vec::new(),
        };
    }

    let fields: Vec<String> = if ifs.is_empty() {
        vec![expanded]
    } else {
        expanded
            .split(|c: char| ifs.contains(c))
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    };

    ExpandedFields { fields }
}

/// Expand a list of words (e.g. argv), each into one string.
pub fn expand_words(words: &[Word], state: &mut ShellState) -> Vec<String> {
    words.iter().map(|w| expand_word(w, state)).collect()
}

/// Expand `$var` and `${...}` references in a raw string (e.g. here-doc body).
pub fn expand_string(text: &str, state: &mut ShellState) -> String {
    expand_operand(text, state)
}

fn expand_part(part: &WordPart, state: &mut ShellState, out: &mut String) {
    match part {
        WordPart::Literal(s) => out.push_str(s),
        WordPart::SingleQuoted(s) => out.push_str(s),
        WordPart::DoubleQuoted(parts) => {
            for p in parts {
                expand_part(p, state, out);
            }
        }
        WordPart::Parameter(name) => {
            // ${#var} — string length
            if let Some(var_name) = name.strip_prefix('#') {
                if !var_name.is_empty() {
                    let len = state
                        .get_var(var_name)
                        .map(|v| v.len())
                        .unwrap_or(0);
                    out.push_str(&len.to_string());
                    return;
                }
                // Bare "#" is the special parameter $# (handled by get_var)
            }
            // ${var/pat/rep} or ${var//pat/rep} — substitution
            if let Some(slash_pos) = name.find('/') {
                let var_name = &name[..slash_pos];
                let rest = &name[slash_pos + 1..];
                let global = rest.starts_with('/');
                let rest = if global { &rest[1..] } else { rest };
                let (pat, rep) = if let Some(sep) = rest.find('/') {
                    (&rest[..sep], &rest[sep + 1..])
                } else {
                    (rest, "")
                };
                if let Some(val) = state.get_var(var_name) {
                    let result = if global {
                        val.replace(pat, rep)
                    } else {
                        val.replacen(pat, rep, 1)
                    };
                    out.push_str(&result);
                }
                return;
            }
            // ${var:offset} or ${var:offset:length} — substring
            if let Some(colon_pos) = name.find(':') {
                let var_name = &name[..colon_pos];
                let rest = &name[colon_pos + 1..];
                // Check it's a numeric offset, not an operator like :-, :+, :=, :?
                if rest.starts_with(|c: char| c.is_ascii_digit())
                    || (rest.starts_with('-') && rest.len() > 1 && rest.as_bytes()[1].is_ascii_digit())
                {
                    if let Some(val) = state.get_var(var_name) {
                        let (offset_str, length_str) = if let Some(sep) = rest.find(':') {
                            (&rest[..sep], Some(&rest[sep + 1..]))
                        } else {
                            (rest, None)
                        };
                        let offset: usize = offset_str.parse().unwrap_or(0);
                        let s = val.as_str();
                        if offset <= s.len() {
                            let substr = &s[offset..];
                            if let Some(len_s) = length_str {
                                let len: usize = len_s.parse().unwrap_or(substr.len());
                                out.push_str(&substr[..len.min(substr.len())]);
                            } else {
                                out.push_str(substr);
                            }
                        }
                        return;
                    }
                }
            }
            if let Some(op_pos) = find_param_operator(name) {
                let var_name = &name[..op_pos];
                let operator = &name[op_pos..op_pos + param_op_len(name, op_pos)];
                let operand = &name[op_pos + operator.len()..];
                expand_param_op(var_name, operator, operand, state, out);
            } else if let Some(val) = state.get_var(name) {
                out.push_str(&val);
            }
        }
        WordPart::CommandSubstitution(_) => {
            // Command substitution is resolved at the runtime level
            // before expand_word is called. If we get here, the
            // substitution was not pre-resolved (e.g. in unit tests).
        }
        WordPart::Arithmetic(expr) => {
            let result = eval_arithmetic(expr, state);
            out.push_str(&result.to_string());
        }
    }
}

/// Find the position of a parameter expansion operator (`:−`, `:-`, `:+`, `:=`, etc.).
fn find_param_operator(name: &str) -> Option<usize> {
    let bytes = name.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if i == 0 && (b == b'#' || b == b'!') {
            continue;
        }
        match b {
            b':' if i + 1 < bytes.len()
                && matches!(bytes[i + 1], b'-' | b'=' | b'+' | b'?') =>
            {
                return Some(i);
            }
            b'-' | b'=' | b'+' | b'?' if i > 0 => {
                // Simple operators without colon
                if !bytes[..i].iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_') {
                    continue;
                }
                return Some(i);
            }
            b'#' if i > 0 => return Some(i),
            b'%' if i > 0 => return Some(i),
            _ => {}
        }
    }
    None
}

fn param_op_len(name: &str, pos: usize) -> usize {
    let bytes = name.as_bytes();
    if bytes[pos] == b':' {
        2 // :-, :=, :+, :?
    } else if pos + 1 < bytes.len() && bytes[pos] == bytes[pos + 1] {
        2 // ##, %%
    } else {
        1
    }
}

/// Expand an operand string that may contain `$var` or `${...}` references.
fn expand_operand(operand: &str, state: &mut ShellState) -> String {
    if !operand.contains('$') {
        return operand.to_string();
    }
    let mut out = String::new();
    let bytes = operand.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() {
        if bytes[pos] == b'$' {
            pos += 1;
            if pos < bytes.len() && bytes[pos] == b'{' {
                pos += 1; // skip {
                let start = pos;
                let mut depth: u32 = 1;
                while pos < bytes.len() && depth > 0 {
                    if bytes[pos] == b'{' { depth += 1; }
                    if bytes[pos] == b'}' { depth -= 1; }
                    if depth > 0 { pos += 1; }
                }
                let inner = &operand[start..pos];
                if pos < bytes.len() { pos += 1; } // skip }
                // Recursively expand as a Parameter
                expand_part(&WordPart::Parameter(SmolStr::from(inner)), state, &mut out);
            } else if pos < bytes.len() && (bytes[pos].is_ascii_alphabetic() || bytes[pos] == b'_') {
                let start = pos;
                while pos < bytes.len() && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') {
                    pos += 1;
                }
                let name = &operand[start..pos];
                if let Some(val) = state.get_var(name) {
                    out.push_str(&val);
                }
            } else {
                out.push('$');
            }
        } else {
            out.push(bytes[pos] as char);
            pos += 1;
        }
    }
    out
}

fn expand_param_op(
    var_name: &str,
    operator: &str,
    operand: &str,
    state: &mut ShellState,
    out: &mut String,
) {
    let val = state.get_var(var_name);
    match operator {
        ":-" => {
            match val {
                Some(v) if !v.is_empty() => out.push_str(&v),
                _ => out.push_str(&expand_operand(operand, state)),
            }
        }
        "-" => {
            match val {
                Some(v) => out.push_str(&v),
                None => out.push_str(&expand_operand(operand, state)),
            }
        }
        ":=" => {
            match val {
                Some(v) if !v.is_empty() => out.push_str(&v),
                _ => {
                    let expanded = expand_operand(operand, state);
                    state.set_var(SmolStr::from(var_name), SmolStr::from(expanded.as_str()));
                    out.push_str(&expanded);
                }
            }
        }
        "=" => {
            match val {
                Some(v) => out.push_str(&v),
                None => {
                    let expanded = expand_operand(operand, state);
                    state.set_var(SmolStr::from(var_name), SmolStr::from(expanded.as_str()));
                    out.push_str(&expanded);
                }
            }
        }
        ":?" => {
            // Error if unset or empty
            match val {
                Some(v) if !v.is_empty() => out.push_str(&v),
                _ => {
                    // In a real shell this would abort. We just output the error message.
                    let msg = if operand.is_empty() {
                        format!("{var_name}: parameter null or not set")
                    } else {
                        format!("{var_name}: {operand}")
                    };
                    out.push_str(&msg);
                }
            }
        }
        ":+" => {
            // Use alternative if set and non-empty
            if let Some(v) = val {
                if !v.is_empty() {
                    out.push_str(operand);
                }
            }
        }
        "+" => {
            // Use alternative if set
            if val.is_some() {
                out.push_str(operand);
            }
        }
        "#" => {
            // ${var#pattern} — remove shortest prefix match
            if let Some(val) = val {
                let pat = expand_operand(operand, state);
                if let Some(rest) = strip_prefix_glob(&val, &pat, false) {
                    out.push_str(rest);
                } else {
                    out.push_str(&val);
                }
            }
        }
        "##" => {
            // ${var##pattern} — remove longest prefix match
            if let Some(val) = val {
                let pat = expand_operand(operand, state);
                if let Some(rest) = strip_prefix_glob(&val, &pat, true) {
                    out.push_str(rest);
                } else {
                    out.push_str(&val);
                }
            }
        }
        "%" => {
            // ${var%pattern} — remove shortest suffix match
            if let Some(val) = val {
                let pat = expand_operand(operand, state);
                if let Some(rest) = strip_suffix_glob(&val, &pat, false) {
                    out.push_str(rest);
                } else {
                    out.push_str(&val);
                }
            }
        }
        "%%" => {
            // ${var%%pattern} — remove longest suffix match
            if let Some(val) = val {
                let pat = expand_operand(operand, state);
                if let Some(rest) = strip_suffix_glob(&val, &pat, true) {
                    out.push_str(rest);
                } else {
                    out.push_str(&val);
                }
            }
        }
        _ => {
            // Unsupported operator — just output the raw value
            if let Some(v) = val {
                out.push_str(&v);
            }
        }
    }
}

/// Strip a glob pattern from the prefix of a string.
/// If `greedy` is true, remove the longest match; otherwise the shortest.
fn strip_prefix_glob<'a>(val: &'a str, pattern: &str, greedy: bool) -> Option<&'a str> {
    if pattern == "*" {
        return Some(if greedy { "" } else { "" });
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        // *SUFFIX pattern — find where suffix appears
        if greedy {
            val.rfind(suffix).map(|pos| &val[pos + suffix.len()..])
        } else {
            val.find(suffix).map(|pos| &val[pos + suffix.len()..])
        }
    } else if let Some(prefix) = pattern.strip_suffix('*') {
        // PREFIX* pattern — find prefix
        if val.starts_with(prefix) {
            Some(if greedy { "" } else { &val[prefix.len()..] })
        } else {
            None
        }
    } else {
        // Literal prefix match
        val.strip_prefix(pattern)
    }
}

/// Strip a glob pattern from the suffix of a string.
/// If `greedy` is true, remove the longest match; otherwise the shortest.
fn strip_suffix_glob<'a>(val: &'a str, pattern: &str, greedy: bool) -> Option<&'a str> {
    if pattern == "*" {
        return Some(if greedy { "" } else { "" });
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        // PREFIX* pattern — find prefix
        if greedy {
            val.find(prefix).map(|pos| &val[..pos])
        } else {
            val.rfind(prefix).map(|pos| &val[..pos])
        }
    } else if let Some(suffix) = pattern.strip_prefix('*') {
        // *SUFFIX pattern — find suffix
        if val.ends_with(suffix) {
            Some(if greedy { "" } else { &val[..val.len() - suffix.len()] })
        } else {
            None
        }
    } else {
        // Literal suffix match
        val.strip_suffix(pattern)
    }
}

/// Simple integer arithmetic evaluation.
fn eval_arithmetic(expr: &str, state: &mut ShellState) -> i64 {
    let trimmed = expr.trim();

    // Try as integer literal
    if let Ok(n) = trimmed.parse::<i64>() {
        return n;
    }

    // Try as variable reference
    if let Some(val) = state.get_var(trimmed) {
        if let Ok(n) = val.parse::<i64>() {
            return n;
        }
    }

    // Simple binary operations: a + b, a - b, a * b, a / b, a % b
    for &(op_str, op_fn) in &[
        ("+", add as fn(i64, i64) -> i64),
        ("-", sub as fn(i64, i64) -> i64),
        ("*", mul as fn(i64, i64) -> i64),
        ("/", div as fn(i64, i64) -> i64),
        ("%", rem as fn(i64, i64) -> i64),
    ] {
        // Find the operator (scan from right for + and - to handle precedence simply)
        if let Some(pos) = if op_str == "+" || op_str == "-" {
            trimmed.rfind(op_str)
        } else {
            trimmed.find(op_str)
        } {
            if pos > 0 && pos < trimmed.len() - 1 {
                let left = eval_arithmetic(&trimmed[..pos], state);
                let right = eval_arithmetic(&trimmed[pos + op_str.len()..], state);
                return op_fn(left, right);
            }
        }
    }

    0
}

/// Expand brace expressions in a word.
///
/// Supports comma lists (`{a,b,c}`) and integer ranges (`{1..10}`).
/// Braces can have a prefix and/or suffix: `pre{a,b}suf` → `preasuf`, `prebsuf`.
/// Returns a vec with a single element (the input) when no brace expansion applies.
pub fn expand_braces(word: &str) -> Vec<String> {
    // Find the first top-level '{' ... '}' pair (respecting nesting)
    let bytes = word.as_bytes();
    let mut brace_start = None;
    let mut depth: u32 = 0;

    for (i, &b) in bytes.iter().enumerate() {
        // Skip escaped characters
        if i > 0 && bytes[i - 1] == b'\\' {
            continue;
        }
        // Skip characters inside single or double quotes
        // (simple approach: if we find a quote char, skip to its pair)
        match b {
            b'{' => {
                if depth == 0 {
                    brace_start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                if depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(start) = brace_start {
                            let prefix = &word[..start];
                            let inner = &word[start + 1..i];
                            let suffix = &word[i + 1..];

                            // Try range pattern: {N..M}
                            if let Some(expansions) = try_brace_range(inner) {
                                let mut result = Vec::new();
                                for item in &expansions {
                                    // Recursively expand suffix
                                    let combined = format!("{prefix}{item}{suffix}");
                                    result.extend(expand_braces(&combined));
                                }
                                return result;
                            }

                            // Try comma list: {a,b,c}
                            if let Some(items) = split_brace_items(inner) {
                                if items.len() > 1 {
                                    let mut result = Vec::new();
                                    for item in &items {
                                        let combined = format!("{prefix}{item}{suffix}");
                                        result.extend(expand_braces(&combined));
                                    }
                                    return result;
                                }
                            }
                        }
                        // Not a valid brace expansion — reset and keep scanning
                        brace_start = None;
                    }
                }
            }
            _ => {}
        }
    }

    vec![word.to_string()]
}

/// Try to parse `inner` as a range pattern `N..M` or `N..M..S`.
fn try_brace_range(inner: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = inner.splitn(3, "..").collect();
    if parts.len() < 2 {
        return None;
    }
    // Integer ranges
    let start: i64 = parts[0].parse().ok()?;
    let end: i64 = parts[1].parse().ok()?;
    let step: i64 = if parts.len() == 3 {
        let s: i64 = parts[2].parse().ok()?;
        if s == 0 { return None; }
        s
    } else if start <= end {
        1
    } else {
        -1
    };

    // Detect zero-padding: if either endpoint has leading zeros
    let width = std::cmp::max(parts[0].len(), parts[1].len());
    let needs_pad = parts[0].starts_with('0') && parts[0].len() > 1
        || parts[1].starts_with('0') && parts[1].len() > 1;

    let mut result = Vec::new();
    let mut cur = start;
    if step > 0 {
        while cur <= end {
            if needs_pad {
                result.push(format!("{cur:0>width$}"));
            } else {
                result.push(cur.to_string());
            }
            cur += step;
        }
    } else {
        while cur >= end {
            if needs_pad {
                result.push(format!("{cur:0>width$}"));
            } else {
                result.push(cur.to_string());
            }
            cur += step;
        }
    }

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Split brace content by top-level commas (respecting nested braces).
fn split_brace_items(inner: &str) -> Option<Vec<String>> {
    let bytes = inner.as_bytes();
    let mut items = Vec::new();
    let mut depth: u32 = 0;
    let mut start = 0;
    let mut has_comma = false;

    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            b',' if depth == 0 => {
                has_comma = true;
                items.push(inner[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
    }

    if !has_comma {
        return None;
    }

    items.push(inner[start..].to_string());
    Some(items)
}

fn add(a: i64, b: i64) -> i64 { a + b }
fn sub(a: i64, b: i64) -> i64 { a - b }
fn mul(a: i64, b: i64) -> i64 { a * b }
fn div(a: i64, b: i64) -> i64 {
    if b == 0 { 0 } else { a / b }
}
fn rem(a: i64, b: i64) -> i64 {
    if b == 0 { 0 } else { a % b }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_word(parts: Vec<WordPart>) -> Word {
        Word {
            parts,
            span: wasmsh_ast::Span { start: 0, end: 0 },
        }
    }

    #[test]
    fn expand_literal() {
        let mut state = ShellState::new();
        let word = make_word(vec![WordPart::Literal("hello".into())]);
        assert_eq!(expand_word(&word, &mut state), "hello");
    }

    #[test]
    fn expand_single_quoted() {
        let mut state = ShellState::new();
        let word = make_word(vec![WordPart::SingleQuoted("$HOME".into())]);
        assert_eq!(expand_word(&word, &mut state), "$HOME"); // no expansion
    }

    #[test]
    fn expand_parameter() {
        let mut state = ShellState::new();
        state.set_var("FOO".into(), "bar".into());
        let word = make_word(vec![WordPart::Parameter("FOO".into())]);
        assert_eq!(expand_word(&word, &mut state), "bar");
    }

    #[test]
    fn expand_unset_parameter() {
        let mut state = ShellState::new();
        let word = make_word(vec![WordPart::Parameter("UNSET".into())]);
        assert_eq!(expand_word(&word, &mut state), "");
    }

    #[test]
    fn expand_special_params() {
        let mut state = ShellState::new();
        state.last_status = 42;
        let word = make_word(vec![WordPart::Parameter("?".into())]);
        assert_eq!(expand_word(&word, &mut state), "42");
    }

    #[test]
    fn expand_double_quoted_with_param() {
        let mut state = ShellState::new();
        state.set_var("USER".into(), "alice".into());
        let word = make_word(vec![WordPart::DoubleQuoted(vec![
            WordPart::Literal("hello ".into()),
            WordPart::Parameter("USER".into()),
        ])]);
        assert_eq!(expand_word(&word, &mut state), "hello alice");
    }

    #[test]
    fn expand_default_value() {
        let mut state = ShellState::new();
        let word = make_word(vec![WordPart::Parameter("X:-default".into())]);
        assert_eq!(expand_word(&word, &mut state), "default");
    }

    #[test]
    fn expand_default_value_set() {
        let mut state = ShellState::new();
        state.set_var("X".into(), "val".into());
        let word = make_word(vec![WordPart::Parameter("X:-default".into())]);
        assert_eq!(expand_word(&word, &mut state), "val");
    }

    #[test]
    fn expand_assign_default() {
        let mut state = ShellState::new();
        let word = make_word(vec![WordPart::Parameter("X:=fallback".into())]);
        assert_eq!(expand_word(&word, &mut state), "fallback");
        // Variable should now be set
        assert_eq!(state.get_var("X").unwrap(), "fallback");
    }

    #[test]
    fn expand_assign_default_already_set() {
        let mut state = ShellState::new();
        state.set_var("X".into(), "existing".into());
        let word = make_word(vec![WordPart::Parameter("X:=fallback".into())]);
        assert_eq!(expand_word(&word, &mut state), "existing");
    }

    #[test]
    fn expand_error_if_unset() {
        let mut state = ShellState::new();
        let word = make_word(vec![WordPart::Parameter("X:?missing".into())]);
        let result = expand_word(&word, &mut state);
        assert!(result.contains("missing"));
    }

    #[test]
    fn expand_alternative_value() {
        let mut state = ShellState::new();
        state.set_var("X".into(), "val".into());
        let word = make_word(vec![WordPart::Parameter("X:+alt".into())]);
        assert_eq!(expand_word(&word, &mut state), "alt");
    }

    #[test]
    fn expand_alternative_unset() {
        let mut state = ShellState::new();
        let word = make_word(vec![WordPart::Parameter("X:+alt".into())]);
        assert_eq!(expand_word(&word, &mut state), "");
    }

    #[test]
    fn expand_arithmetic_literal() {
        let mut state = ShellState::new();
        let word = make_word(vec![WordPart::Arithmetic("42".into())]);
        assert_eq!(expand_word(&word, &mut state), "42");
    }

    #[test]
    fn expand_arithmetic_addition() {
        let mut state = ShellState::new();
        let word = make_word(vec![WordPart::Arithmetic("1+2".into())]);
        assert_eq!(expand_word(&word, &mut state), "3");
    }

    #[test]
    fn expand_arithmetic_with_var() {
        let mut state = ShellState::new();
        state.set_var("X".into(), "10".into());
        let word = make_word(vec![WordPart::Arithmetic("X+5".into())]);
        assert_eq!(expand_word(&word, &mut state), "15");
    }

    #[test]
    fn expand_arithmetic_precedence() {
        let mut state = ShellState::new();
        // * should bind tighter than +
        let word = make_word(vec![WordPart::Arithmetic("2+3*4".into())]);
        assert_eq!(expand_word(&word, &mut state), "14");
    }

    #[test]
    fn expand_arithmetic_subtraction() {
        let mut state = ShellState::new();
        let word = make_word(vec![WordPart::Arithmetic("10-3-2".into())]);
        assert_eq!(expand_word(&word, &mut state), "5");
    }

    #[test]
    fn expand_arithmetic_division() {
        let mut state = ShellState::new();
        let word = make_word(vec![WordPart::Arithmetic("10/3".into())]);
        assert_eq!(expand_word(&word, &mut state), "3");
    }

    #[test]
    fn expand_arithmetic_modulo() {
        let mut state = ShellState::new();
        let word = make_word(vec![WordPart::Arithmetic("10%3".into())]);
        assert_eq!(expand_word(&word, &mut state), "1");
    }

    #[test]
    fn expand_mixed_parts() {
        let mut state = ShellState::new();
        state.set_var("HOME".into(), "/home/user".into());
        let word = make_word(vec![
            WordPart::Parameter("HOME".into()),
            WordPart::Literal("/bin".into()),
        ]);
        assert_eq!(expand_word(&word, &mut state), "/home/user/bin");
    }

    #[test]
    fn expand_word_split_basic() {
        let mut state = ShellState::new();
        state.set_var("X".into(), "a b c".into());
        let word = make_word(vec![WordPart::Parameter("X".into())]);
        let fields = expand_word_split(&word, &mut state);
        assert_eq!(fields.fields, vec!["a", "b", "c"]);
    }

    #[test]
    fn expand_words_list() {
        let mut state = ShellState::new();
        state.set_var("X".into(), "hello".into());
        let words = vec![
            make_word(vec![WordPart::Literal("echo".into())]),
            make_word(vec![WordPart::Parameter("X".into())]),
        ];
        assert_eq!(expand_words(&words, &mut state), vec!["echo", "hello"]);
    }

    #[test]
    fn command_substitution_placeholder() {
        let mut state = ShellState::new();
        let word = make_word(vec![WordPart::CommandSubstitution("echo hi".into())]);
        assert_eq!(expand_word(&word, &mut state), ""); // placeholder
    }

    #[test]
    fn expand_string_length() {
        let mut state = ShellState::new();
        state.set_var("X".into(), "hello".into());
        let word = make_word(vec![WordPart::Parameter("#X".into())]);
        assert_eq!(expand_word(&word, &mut state), "5");
    }

    #[test]
    fn expand_string_length_empty() {
        let mut state = ShellState::new();
        state.set_var("X".into(), "".into());
        let word = make_word(vec![WordPart::Parameter("#X".into())]);
        assert_eq!(expand_word(&word, &mut state), "0");
    }

    #[test]
    fn expand_string_length_unset() {
        let mut state = ShellState::new();
        let word = make_word(vec![WordPart::Parameter("#UNSET".into())]);
        assert_eq!(expand_word(&word, &mut state), "0");
    }

    #[test]
    fn expand_hash_special_param() {
        // $# (argument count) should still work
        let mut state = ShellState::new();
        state.positional = vec!["a".into(), "b".into()];
        let word = make_word(vec![WordPart::Parameter("#".into())]);
        assert_eq!(expand_word(&word, &mut state), "2");
    }

    // ---- Brace expansion ----

    #[test]
    fn brace_comma_list() {
        assert_eq!(expand_braces("{a,b,c}"), vec!["a", "b", "c"]);
    }

    #[test]
    fn brace_with_prefix_suffix() {
        assert_eq!(
            expand_braces("pre{x,y}suf"),
            vec!["prexsuf", "preysuf"]
        );
    }

    #[test]
    fn brace_range_ascending() {
        assert_eq!(
            expand_braces("{1..5}"),
            vec!["1", "2", "3", "4", "5"]
        );
    }

    #[test]
    fn brace_range_descending() {
        assert_eq!(
            expand_braces("{5..1}"),
            vec!["5", "4", "3", "2", "1"]
        );
    }

    #[test]
    fn brace_no_expansion() {
        assert_eq!(expand_braces("hello"), vec!["hello"]);
    }

    #[test]
    fn brace_single_item_no_expansion() {
        assert_eq!(expand_braces("{hello}"), vec!["{hello}"]);
    }

    #[test]
    fn brace_nested() {
        assert_eq!(
            expand_braces("{a,{b,c}}"),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn brace_range_with_prefix() {
        assert_eq!(
            expand_braces("file{1..3}.txt"),
            vec!["file1.txt", "file2.txt", "file3.txt"]
        );
    }
}
