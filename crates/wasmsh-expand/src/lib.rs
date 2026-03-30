//! Word expansion engine for the wasmsh shell.
//!
//! Performs expansions on structured `Word`/`WordPart` nodes in the
//! correct POSIX order:
//! 1. Tilde expansion
//! 2. Parameter expansion
//! 3. Command substitution (resolved at runtime layer)
//! 4. Arithmetic expansion (basic integer expressions)
//! 5. Field splitting
//! 6. Pathname expansion / globbing (resolved at runtime layer)
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
    if out.starts_with('~') && (out == "~" || out.starts_with("~/")) {
        if let Some(home) = state.get_var("HOME") {
            out = format!("{home}{}", &out[1..]);
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
        return ExpandedFields { fields: Vec::new() };
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

/// An expanded word paired with its quoting context.
#[derive(Debug, Clone)]
pub struct ExpandedWord {
    /// The expanded text (quotes removed, parameters resolved).
    pub text: String,
    /// True when the original word contained any quoting (single, double, or escape).
    /// Brace expansion must be suppressed for quoted words.
    pub was_quoted: bool,
}

/// Expand a list of words for argv, preserving quote metadata so the
/// runtime can skip brace expansion on quoted arguments.
pub fn expand_words_argv(words: &[Word], state: &mut ShellState) -> Vec<ExpandedWord> {
    words
        .iter()
        .map(|w| {
            let was_quoted = w.parts.iter().any(|p| {
                matches!(
                    p,
                    WordPart::SingleQuoted(_) | WordPart::DoubleQuoted(_)
                )
            });
            ExpandedWord {
                text: expand_word(w, state),
                was_quoted,
            }
        })
        .collect()
}

/// Expand `$var` and `${...}` references in a raw string (e.g. here-doc body).
pub fn expand_string(text: &str, state: &mut ShellState) -> String {
    expand_operand(text, state)
}

fn expand_part(part: &WordPart, state: &mut ShellState, out: &mut String) {
    expand_part_depth(part, state, out, 0);
}

fn expand_part_depth(part: &WordPart, state: &mut ShellState, out: &mut String, depth: usize) {
    if depth > MAX_EXPAND_DEPTH {
        match part {
            WordPart::Literal(s) | WordPart::SingleQuoted(s) | WordPart::Parameter(s) => {
                out.push_str(s);
            }
            _ => {}
        }
        return;
    }
    match part {
        WordPart::Literal(s) | WordPart::SingleQuoted(s) => out.push_str(s),
        WordPart::DoubleQuoted(parts) => {
            for p in parts {
                expand_part_depth(p, state, out, depth);
            }
        }
        WordPart::Parameter(name) => expand_parameter(name, state, out, depth),
        WordPart::Arithmetic(expr) => {
            let result = eval_arithmetic(expr, state);
            out.push_str(&result.to_string());
        }
        _ => {}
    }
}

/// Expand a parameter reference (`$name` or `${...}`).
fn expand_parameter(name: &SmolStr, state: &mut ShellState, out: &mut String, depth: usize) {
    // Try each expansion type in order; return early when handled.
    if try_expand_array_subscript(name, state, out) {
        return;
    }
    if try_expand_string_length(name, state, out) {
        return;
    }
    if try_expand_indirect_or_prefix(name, state, out) {
        return;
    }
    if let Some(case_result) = try_case_modification(name.as_str(), state) {
        out.push_str(&case_result);
        return;
    }
    if let Some(transform_result) = try_transform_operator(name.as_str(), state) {
        out.push_str(&transform_result);
        return;
    }
    if try_expand_substitution(name, state, out) {
        return;
    }
    if try_expand_substring(name, state, out) {
        return;
    }
    if let Some(op_pos) = find_param_operator(name) {
        let var_name = &name[..op_pos];
        let operator = &name[op_pos..op_pos + param_op_len(name, op_pos)];
        let operand = &name[op_pos + operator.len()..];
        expand_param_op_depth(var_name, operator, operand, state, out, depth);
    } else if let Some(val) = state.get_var(name) {
        out.push_str(&val);
    } else if state.get_var("SHOPT_u").as_deref() == Some("1") {
        let is_special = matches!(name.as_str(), "?" | "#" | "0" | "@" | "*" | "-" | "$" | "!")
            || name.parse::<usize>().is_ok();
        if !is_special {
            state.set_var(
                SmolStr::from("_NOUNSET_ERROR"),
                SmolStr::from(name.as_str()),
            );
        }
    }
}

/// Check if `name` is a valid identifier (alphanumeric + underscore).
fn is_valid_identifier(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Strip a `[@]` or `[*]` suffix and return the base name, if non-empty.
fn strip_array_glob_suffix(s: &str) -> Option<&str> {
    let base = s.strip_suffix("[@]").or_else(|| s.strip_suffix("[*]"))?;
    if base.is_empty() {
        None
    } else {
        Some(base)
    }
}

/// Try to expand array subscript expressions. Returns true if handled.
fn try_expand_array_subscript(name: &str, state: &mut ShellState, out: &mut String) -> bool {
    if try_expand_array_length_or_keys(name, state, out) {
        return true;
    }
    if try_expand_array_all_values(name, state, out) {
        return true;
    }
    try_expand_array_single_element(name, state, out)
}

/// Handle `${#arr[@]}` (array length) and `${!arr[@]}` (array keys).
fn try_expand_array_length_or_keys(name: &str, state: &mut ShellState, out: &mut String) -> bool {
    // ${#arr[@]} or ${#arr[*]} -- array length
    if let Some(rest) = name.strip_prefix('#') {
        if let Some(base) = strip_array_glob_suffix(rest) {
            out.push_str(&state.get_array_length(base).to_string());
            return true;
        }
    }
    // ${!arr[@]} or ${!arr[*]} -- array keys
    if let Some(rest) = name.strip_prefix('!') {
        if let Some(base) = strip_array_glob_suffix(rest) {
            out.push_str(&state.get_array_keys(base).join(" "));
            return true;
        }
    }
    false
}

/// Handle `${arr[@]}` or `${arr[*]}` -- all array values.
fn try_expand_array_all_values(name: &str, state: &mut ShellState, out: &mut String) -> bool {
    let Some(base) = strip_array_glob_suffix(name) else {
        return false;
    };
    let values = state.get_array_values(base);
    let joined: Vec<&str> = values.iter().map(SmolStr::as_str).collect();
    out.push_str(&joined.join(" "));
    true
}

/// Handle `${arr[N]}` -- single element access.
fn try_expand_array_single_element(name: &str, state: &mut ShellState, out: &mut String) -> bool {
    let Some(bracket_pos) = name.find('[') else {
        return false;
    };
    let Some(end) = name[bracket_pos..].find(']') else {
        return false;
    };
    let base = &name[..bracket_pos];
    let index = &name[bracket_pos + 1..bracket_pos + end];
    if base.is_empty() || index.is_empty() {
        return false;
    }
    let expanded_index = if index.contains('$') {
        expand_string(index, state)
    } else {
        index.to_string()
    };
    if let Some(val) = state.get_array_element(base, &expanded_index) {
        out.push_str(&val);
    }
    true
}

/// Try to expand ${#var} (string length). Returns true if handled.
fn try_expand_string_length(name: &str, state: &ShellState, out: &mut String) -> bool {
    if let Some(var_name) = name.strip_prefix('#') {
        if !var_name.is_empty() {
            let len = state.get_var(var_name).map_or(0, |v| v.len());
            out.push_str(&len.to_string());
            return true;
        }
    }
    false
}

/// Try to expand ${!name} (indirect) or ${!prefix*}/${!prefix@} (prefix). Returns true if handled.
fn try_expand_indirect_or_prefix(name: &str, state: &ShellState, out: &mut String) -> bool {
    let Some(rest) = name.strip_prefix('!') else {
        return false;
    };
    // ${!prefix*} or ${!prefix@}
    if let Some(prefix) = rest.strip_suffix('*').or_else(|| rest.strip_suffix('@')) {
        if is_valid_identifier(prefix) {
            let names = state.var_names_with_prefix(prefix);
            let joined: Vec<&str> = names.iter().map(SmolStr::as_str).collect();
            out.push_str(&joined.join(" "));
            return true;
        }
    }
    // ${!name} — indirect expansion
    if is_valid_identifier(rest) {
        if let Some(indirect_name) = state.get_var(rest) {
            if let Some(val) = state.get_var(&indirect_name) {
                out.push_str(&val);
            }
        }
        return true;
    }
    false
}

/// Anchor type for substitution patterns.
enum SubstAnchor {
    Start,
    End,
    None,
}

/// Try to expand ${var/pat/rep} or ${var//pat/rep} (substitution). Returns true if handled.
fn try_expand_substitution(name: &str, state: &mut ShellState, out: &mut String) -> bool {
    let Some(slash_pos) = name.find('/') else {
        return false;
    };
    let var_name = &name[..slash_pos];
    if !is_valid_identifier(var_name) {
        return false;
    }
    let rest = &name[slash_pos + 1..];
    let global = rest.starts_with('/');
    let rest = if global { &rest[1..] } else { rest };

    let (anchor, pat_str) = parse_subst_anchor(rest);
    let (pat, rep) = parse_subst_pat_rep(rest, &anchor);

    if let Some(val) = state.get_var(var_name) {
        let result = substitution_result(&val, &anchor, pat_str, pat, rep, global);
        out.push_str(&result);
    }
    true
}

fn substitution_result(
    val: &str,
    anchor: &SubstAnchor,
    pat_str: &str,
    pat: &str,
    rep: &str,
    global: bool,
) -> String {
    match anchor {
        SubstAnchor::Start => substitution_at_start(val, pat_str, rep),
        SubstAnchor::End => substitution_at_end(val, pat_str, rep),
        SubstAnchor::None => substitution_unanchored(val, pat, rep, global),
    }
}

fn substitution_at_start(val: &str, pattern: &str, rep: &str) -> String {
    glob_match_at_start(val, pattern).map_or_else(
        || val.to_string(),
        |match_len| format!("{rep}{}", &val[match_len..]),
    )
}

fn substitution_at_end(val: &str, pattern: &str, rep: &str) -> String {
    glob_match_at_end(val, pattern).map_or_else(
        || val.to_string(),
        |match_start| format!("{}{rep}", &val[..match_start]),
    )
}

fn substitution_unanchored(val: &str, pat: &str, rep: &str, global: bool) -> String {
    if global {
        glob_replace_all(val, pat, rep)
    } else {
        glob_replace_first(val, pat, rep)
    }
}

/// Parse the anchor (#, %, or none) and pattern string from substitution rest.
fn parse_subst_anchor<'a>(rest: &'a str) -> (SubstAnchor, &'a str) {
    if let Some(p) = rest.strip_prefix('#') {
        let pat = p.split('/').next().unwrap_or(p);
        (SubstAnchor::Start, pat)
    } else if let Some(p) = rest.strip_prefix('%') {
        let pat = p.split('/').next().unwrap_or(p);
        (SubstAnchor::End, pat)
    } else {
        (SubstAnchor::None, "")
    }
}

/// Parse pattern and replacement from substitution rest, accounting for anchor.
fn parse_subst_pat_rep<'a>(rest: &'a str, anchor: &SubstAnchor) -> (&'a str, &'a str) {
    match anchor {
        SubstAnchor::Start | SubstAnchor::End => {
            let after_anchor = if rest.starts_with('#') || rest.starts_with('%') {
                &rest[1..]
            } else {
                rest
            };
            if let Some(sep) = after_anchor.find('/') {
                (&after_anchor[..sep], &after_anchor[sep + 1..])
            } else {
                (after_anchor, "")
            }
        }
        SubstAnchor::None => {
            if let Some(sep) = rest.find('/') {
                (&rest[..sep], &rest[sep + 1..])
            } else {
                (rest, "")
            }
        }
    }
}

/// Try to expand ${var:offset} or ${var:offset:length} (substring). Returns true if handled.
fn try_expand_substring(name: &str, state: &ShellState, out: &mut String) -> bool {
    let Some(colon_pos) = name.find(':') else {
        return false;
    };
    let var_name = &name[..colon_pos];
    let rest = &name[colon_pos + 1..];
    // Check it's a numeric offset, not an operator like :-, :+, :=, :?
    let is_numeric = rest.starts_with(|c: char| c.is_ascii_digit())
        || (rest.starts_with('-') && rest.len() > 1 && rest.as_bytes()[1].is_ascii_digit());
    if !is_numeric {
        return false;
    }
    let Some(val) = state.get_var(var_name) else {
        return true; // Handled but empty
    };
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
    true
}

/// Find the position of a parameter expansion operator (`:−`, `:-`, `:+`, `:=`, etc.).
fn find_param_operator(name: &str) -> Option<usize> {
    let bytes = name.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if i == 0 && (b == b'#' || b == b'!') {
            continue;
        }
        match b {
            b':' if i + 1 < bytes.len() && matches!(bytes[i + 1], b'-' | b'=' | b'+' | b'?') => {
                return Some(i);
            }
            b'-' | b'=' | b'+' | b'?' if i > 0 => {
                // Simple operators without colon
                if !bytes[..i]
                    .iter()
                    .all(|c| c.is_ascii_alphanumeric() || *c == b'_')
                {
                    continue;
                }
                return Some(i);
            }
            b'#' | b'%' if i > 0 => return Some(i),
            _ => {}
        }
    }
    None
}

fn param_op_len(name: &str, pos: usize) -> usize {
    let bytes = name.as_bytes();
    if bytes[pos] == b':' || (pos + 1 < bytes.len() && bytes[pos] == bytes[pos + 1]) {
        2 // :-, :=, :+, :? or ##, %%
    } else {
        1
    }
}

/// Maximum expansion depth for variable references to prevent infinite recursion.
const MAX_EXPAND_DEPTH: usize = 50;

/// Expand an operand string that may contain `$var` or `${...}` references.
fn expand_operand(operand: &str, state: &mut ShellState) -> String {
    expand_operand_inner(operand, state, 0)
}

/// Scan a `${...}` braced parameter reference starting at `pos` (which should point
/// just past the `{`). Returns `(content_end, new_pos)` where `new_pos` is past the
/// closing `}`.
fn scan_braced_param(bytes: &[u8], mut pos: usize) -> (usize, usize) {
    let mut brace_depth: u32 = 1;
    while pos < bytes.len() && brace_depth > 0 {
        if bytes[pos] == b'{' {
            brace_depth += 1;
        }
        if bytes[pos] == b'}' {
            brace_depth -= 1;
        }
        if brace_depth > 0 {
            pos += 1;
        }
    }
    let end = pos;
    if pos < bytes.len() {
        pos += 1; // skip closing }
    }
    (end, pos)
}

/// Scan a bare `$name` (alphanumeric + underscore) starting at `pos`. Returns `(end, new_pos)`.
fn scan_bare_var(bytes: &[u8], mut pos: usize) -> usize {
    while pos < bytes.len() && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') {
        pos += 1;
    }
    pos
}

/// Inner implementation with depth tracking.
fn expand_operand_inner(operand: &str, state: &mut ShellState, depth: usize) -> String {
    if depth > MAX_EXPAND_DEPTH || !operand.contains('$') {
        return operand.to_string();
    }
    let mut out = String::new();
    let bytes = operand.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() {
        if bytes[pos] != b'$' {
            out.push(bytes[pos] as char);
            pos += 1;
            continue;
        }
        pos += 1; // skip $
        expand_dollar_in_operand(operand, bytes, &mut pos, state, &mut out, depth);
    }
    out
}

/// Expand a `$` reference within an operand string. Called with `pos` just past `$`.
fn expand_dollar_in_operand(
    operand: &str,
    bytes: &[u8],
    pos: &mut usize,
    state: &mut ShellState,
    out: &mut String,
    depth: usize,
) {
    if *pos < bytes.len() && bytes[*pos] == b'{' {
        *pos += 1; // skip {
        let (end, new_pos) = scan_braced_param(bytes, *pos);
        let inner = &operand[*pos..end];
        *pos = new_pos;
        expand_part_depth(
            &WordPart::Parameter(SmolStr::from(inner)),
            state,
            out,
            depth + 1,
        );
    } else if *pos < bytes.len() && (bytes[*pos].is_ascii_alphabetic() || bytes[*pos] == b'_') {
        let start = *pos;
        *pos = scan_bare_var(bytes, *pos);
        if let Some(val) = state.get_var(&operand[start..*pos]) {
            out.push_str(&val);
        }
    } else {
        out.push('$');
    }
}

/// Handle default/assign/error/alternative operators (`:−`, `-`, `:=`, `=`, `:?`, `:+`, `+`).
fn expand_param_default_op(
    var_name: &str,
    operator: &str,
    operand: &str,
    val: Option<SmolStr>,
    state: &mut ShellState,
    out: &mut String,
    depth: usize,
) {
    match operator {
        ":-" => expand_param_default_value(val, operand, state, out, depth, true),
        "-" => expand_param_default_value(val, operand, state, out, depth, false),
        ":=" => expand_param_assign_value(var_name, val, operand, state, out, depth, true),
        "=" => expand_param_assign_value(var_name, val, operand, state, out, depth, false),
        ":?" => expand_param_error_value(var_name, val, operand, out, true),
        ":+" => expand_param_alt_value(val.as_ref(), operand, out, true),
        _ => expand_param_alt_value(val.as_ref(), operand, out, false),
    }
}

fn expand_param_default_value(
    val: Option<SmolStr>,
    operand: &str,
    state: &mut ShellState,
    out: &mut String,
    depth: usize,
    require_non_empty: bool,
) {
    if param_has_value(val.as_ref(), require_non_empty) {
        if let Some(value) = val {
            out.push_str(&value);
        }
        return;
    }
    out.push_str(&expand_operand_inner(operand, state, depth + 1));
}

fn expand_param_assign_value(
    var_name: &str,
    val: Option<SmolStr>,
    operand: &str,
    state: &mut ShellState,
    out: &mut String,
    depth: usize,
    require_non_empty: bool,
) {
    if param_has_value(val.as_ref(), require_non_empty) {
        if let Some(value) = val {
            out.push_str(&value);
        }
        return;
    }
    let expanded = expand_operand_inner(operand, state, depth + 1);
    state.set_var(SmolStr::from(var_name), SmolStr::from(expanded.as_str()));
    out.push_str(&expanded);
}

fn expand_param_error_value(
    var_name: &str,
    val: Option<SmolStr>,
    operand: &str,
    out: &mut String,
    require_non_empty: bool,
) {
    if param_has_value(val.as_ref(), require_non_empty) {
        if let Some(value) = val {
            out.push_str(&value);
        }
        return;
    }
    let msg = if operand.is_empty() {
        format!("{var_name}: parameter null or not set")
    } else {
        format!("{var_name}: {operand}")
    };
    out.push_str(&msg);
}

fn expand_param_alt_value(
    val: Option<&SmolStr>,
    operand: &str,
    out: &mut String,
    require_non_empty: bool,
) {
    if param_has_value(val, require_non_empty) {
        out.push_str(operand);
    }
}

fn param_has_value(val: Option<&SmolStr>, require_non_empty: bool) -> bool {
    match val {
        Some(value) if require_non_empty => !value.is_empty(),
        Some(_) => true,
        None => false,
    }
}

/// Handle prefix/suffix stripping operators (`#`, `##`, `%`, `%%`).
fn expand_param_strip_op(
    operator: &str,
    operand: &str,
    val: &SmolStr,
    state: &mut ShellState,
    out: &mut String,
    depth: usize,
) {
    let pat = expand_operand_inner(operand, state, depth + 1);
    let stripped = match operator {
        "#" => strip_prefix_glob(val, &pat, false),
        "##" => strip_prefix_glob(val, &pat, true),
        "%" => strip_suffix_glob(val, &pat, false),
        // "%%"
        _ => strip_suffix_glob(val, &pat, true),
    };
    out.push_str(stripped.unwrap_or(val));
}

fn expand_param_op_depth(
    var_name: &str,
    operator: &str,
    operand: &str,
    state: &mut ShellState,
    out: &mut String,
    depth: usize,
) {
    let val = state.get_var(var_name);
    match operator {
        ":-" | "-" | ":=" | "=" | ":?" | ":+" | "+" => {
            expand_param_default_op(var_name, operator, operand, val, state, out, depth);
        }
        "#" | "##" | "%" | "%%" => {
            if let Some(val) = val {
                expand_param_strip_op(operator, operand, &val, state, out, depth);
            }
        }
        _ => {
            if let Some(v) = val {
                out.push_str(&v);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Glob matching utility (for substitution patterns)
// ---------------------------------------------------------------------------

/// Match a glob pattern against a string. Supports `*` (any string) and `?` (any char).
fn simple_glob_match(pattern: &str, text: &str) -> bool {
    let p = pattern.as_bytes();
    let n = text.as_bytes();
    let mut pi = 0;
    let mut ni = 0;
    let mut star_p = usize::MAX;
    let mut star_n = 0;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star_p = pi;
            star_n = ni;
            pi += 1;
        } else if star_p != usize::MAX {
            pi = star_p + 1;
            star_n += 1;
            ni = star_n;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// Check whether `pattern` contains glob meta-characters (`*` or `?`).
fn is_glob_pattern(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?')
}

/// Find the first substring of `text` that matches the glob `pattern` and return
/// `(start, end)`. Returns `None` if no match.
fn glob_find_first(text: &str, pattern: &str) -> Option<(usize, usize)> {
    if pattern.is_empty() {
        return Some((0, 0));
    }
    let text_len = text.len();
    // Try each start position; for each, try each end position (shortest match first).
    for start in 0..=text_len {
        for end in start..=text_len {
            if simple_glob_match(pattern, &text[start..end]) {
                return Some((start, end));
            }
        }
    }
    None
}

/// Replace the first occurrence of glob `pattern` in `text` with `rep`.
fn glob_replace_first(text: &str, pattern: &str, rep: &str) -> String {
    if !is_glob_pattern(pattern) {
        // Literal replacement — use standard method for efficiency
        return text.replacen(pattern, rep, 1);
    }
    if let Some((start, end)) = glob_find_first(text, pattern) {
        format!("{}{rep}{}", &text[..start], &text[end..])
    } else {
        text.to_string()
    }
}

/// Replace all non-overlapping occurrences of glob `pattern` in `text` with `rep`.
fn glob_replace_all(text: &str, pattern: &str, rep: &str) -> String {
    if !is_glob_pattern(pattern) {
        // Literal replacement
        return text.replace(pattern, rep);
    }
    let mut result = String::new();
    let mut pos = 0;
    let text_len = text.len();
    while pos <= text_len {
        // Try to match starting at pos — find the shortest match
        let matched = (pos..=text_len).find(|&end| simple_glob_match(pattern, &text[pos..end]));
        if let Some(end) = matched {
            result.push_str(rep);
            pos = if end == pos { end + 1 } else { end };
        } else {
            if pos < text_len {
                result.push(text.as_bytes()[pos] as char);
            }
            pos += 1;
        }
    }
    result
}

/// Check if glob `pattern` matches at the start of `text`. Returns the length of
/// the longest match, or `None` if no match at start. Bash uses longest match
/// for anchored substitution (${var/#pat/rep}).
fn glob_match_at_start(text: &str, pattern: &str) -> Option<usize> {
    let text_len = text.len();
    let mut best = None;
    for end in 0..=text_len {
        if simple_glob_match(pattern, &text[..end]) {
            best = Some(end);
        }
    }
    best
}

/// Check if glob `pattern` matches at the end of `text`. Returns the start position
/// of the longest match, or `None` if no match at end. Bash uses longest match
/// for anchored substitution (${var/%pat/rep}).
fn glob_match_at_end(text: &str, pattern: &str) -> Option<usize> {
    let text_len = text.len();
    let mut best = None;
    for start in (0..=text_len).rev() {
        if simple_glob_match(pattern, &text[start..]) {
            best = Some(start);
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Case modification: ${var^}, ${var^^}, ${var,}, ${var,,}
// ---------------------------------------------------------------------------

/// Try to parse and apply case modification. Returns `Some(result)` if the name
/// contains a case modifier, `None` otherwise.
/// Transform the first character of a string using the given function, leaving the rest.
fn transform_first_char(s: &str, f: fn(char) -> String) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => {
            let mut result = f(c);
            result.extend(chars);
            result
        }
        None => String::new(),
    }
}

/// Parse a case modifier from the rest-of-name after the variable identifier.
fn parse_case_modifier(rest: &str) -> Option<&str> {
    if rest.starts_with("^^") {
        Some("^^")
    } else if rest.starts_with('^') {
        Some("^")
    } else if rest.starts_with(",,") {
        Some(",,")
    } else if rest.starts_with(',') {
        Some(",")
    } else {
        None
    }
}

fn try_case_modification(name: &str, state: &ShellState) -> Option<String> {
    let bytes = name.as_bytes();
    let mut var_end = 0;
    while var_end < bytes.len()
        && (bytes[var_end].is_ascii_alphanumeric() || bytes[var_end] == b'_')
    {
        var_end += 1;
    }
    if var_end == 0 || var_end >= bytes.len() {
        return None;
    }

    let var_name = &name[..var_end];
    let modifier = parse_case_modifier(&name[var_end..])?;
    let val = state.get_var(var_name)?;

    let result = match modifier {
        "^" => transform_first_char(&val, |c| c.to_uppercase().to_string()),
        "^^" => val.to_uppercase(),
        "," => transform_first_char(&val, |c| c.to_lowercase().to_string()),
        ",," => val.to_lowercase(),
        _ => return None,
    };

    Some(result)
}

// ---------------------------------------------------------------------------
// Transformation operators: ${var@Q}, ${var@E}, ${var@U}, ${var@L}, etc.
// ---------------------------------------------------------------------------

/// Try to parse and apply a transformation operator. Returns `Some(result)` if the
/// name contains `@X` at the end (where X is a recognized operator letter), `None`
/// otherwise.
fn try_transform_operator(name: &str, state: &ShellState) -> Option<String> {
    // Find the last '@' that is preceded by a valid variable name.
    let bytes = name.as_bytes();

    // The name must end with @X where X is one character.
    if bytes.len() < 3 {
        return None;
    }
    let at_pos = bytes.len() - 2;
    if bytes[at_pos] != b'@' {
        return None;
    }
    let op = bytes[bytes.len() - 1];
    if !matches!(op, b'Q' | b'E' | b'U' | b'L' | b'u' | b'a' | b'A') {
        return None;
    }

    let var_name = &name[..at_pos];
    // Validate variable name
    if var_name.is_empty()
        || !var_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }

    let val = state.get_var(var_name).unwrap_or_default();

    let result = match op {
        b'Q' => {
            // Quote for reuse: wrap in single quotes, escaping existing single quotes
            let escaped = val.replace('\'', "'\\''");
            format!("'{escaped}'")
        }
        b'E' => {
            // Expand backslash escape sequences
            expand_backslash_escapes(&val)
        }
        b'U' => val.to_uppercase(),
        b'L' => val.to_lowercase(),
        b'u' => transform_first_char(&val, |c| c.to_uppercase().to_string()),
        b'a' => {
            // Attribute flags — for now return empty (attributes not tracked in expand)
            String::new()
        }
        b'A' => {
            // Assignment statement form: declare -- var="value"
            format!("declare -- {var_name}=\"{val}\"")
        }
        _ => return None,
    };

    Some(result)
}

/// Map a single-character escape code to its replacement character.
fn simple_escape_char(b: u8) -> Option<char> {
    match b {
        b'n' => Some('\n'),
        b't' => Some('\t'),
        b'r' => Some('\r'),
        b'a' => Some('\x07'),
        b'b' => Some('\x08'),
        b'e' | b'E' => Some('\x1b'),
        b'f' => Some('\x0c'),
        b'v' => Some('\x0b'),
        b'\\' => Some('\\'),
        b'\'' => Some('\''),
        b'"' => Some('"'),
        _ => None,
    }
}

/// Parse an octal escape `\0NNN` starting at `i` (pointing at the `0`). Returns
/// `(char, new_position)`.
fn parse_octal_escape(s: &str, bytes: &[u8], i: usize) -> (char, usize) {
    let start = i + 1;
    let mut end = start;
    while end < bytes.len() && end - start < 3 && bytes[end] >= b'0' && bytes[end] <= b'7' {
        end += 1;
    }
    if end > start {
        let val = u8::from_str_radix(&s[start..end], 8).unwrap_or(0);
        (val as char, end)
    } else {
        ('\0', i + 1)
    }
}

/// Parse a hex escape `\xNN` starting at `i` (pointing at the `x`). Returns
/// `(replacement_string, new_position)`. Returns `"\\x"` when no hex digits follow.
fn parse_hex_escape(s: &str, bytes: &[u8], i: usize) -> (char, usize, bool) {
    let start = i + 1;
    let mut end = start;
    while end < bytes.len() && end - start < 2 && bytes[end].is_ascii_hexdigit() {
        end += 1;
    }
    if end > start {
        let val = u8::from_str_radix(&s[start..end], 16).unwrap_or(0);
        (val as char, end, true)
    } else {
        ('\0', i + 1, false)
    }
}

/// Expand backslash escape sequences in a string (for ${var@E}).
fn expand_backslash_escapes(s: &str) -> String {
    let mut out = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }
        i += 1;
        if let Some(ch) = simple_escape_char(bytes[i]) {
            out.push(ch);
            i += 1;
        } else if bytes[i] == b'0' {
            let (ch, new_pos) = parse_octal_escape(s, bytes, i);
            out.push(ch);
            i = new_pos;
        } else if bytes[i] == b'x' {
            let (ch, new_pos, ok) = parse_hex_escape(s, bytes, i);
            if ok {
                out.push(ch);
            } else {
                out.push_str("\\x");
            }
            i = new_pos;
        } else {
            out.push('\\');
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Strip a glob pattern from the prefix of a string.
/// If `greedy` is true, remove the longest match; otherwise the shortest.
fn strip_prefix_glob<'a>(val: &'a str, pattern: &str, greedy: bool) -> Option<&'a str> {
    if pattern == "*" {
        return Some("");
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
        if let Some(rest) = val.strip_prefix(prefix) {
            Some(if greedy { "" } else { rest })
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
        return Some("");
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
            Some(if greedy {
                ""
            } else {
                val.strip_suffix(suffix).unwrap_or("")
            })
        } else {
            None
        }
    } else {
        // Literal suffix match
        val.strip_suffix(pattern)
    }
}

// ---------------------------------------------------------------------------
// Arithmetic evaluator — full recursive-descent parser
// ---------------------------------------------------------------------------

/// Token types produced by the arithmetic tokenizer.
#[derive(Debug, Clone, PartialEq)]
enum ArithToken {
    Number(i64),
    Ident(String),
    // Binary / compound operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    StarStar, // **
    LShift,   // <<
    RShift,   // >>
    Lt,       // <
    Gt,       // >
    Le,       // <=
    Ge,       // >=
    EqEq,     // ==
    Ne,       // !=
    Amp,      // &
    Caret,    // ^
    Pipe,     // |
    AmpAmp,   // &&
    PipePipe, // ||
    Bang,     // !
    Tilde,    // ~
    Question, // ?
    Colon,    // :
    Comma,    // ,
    LParen,
    RParen,
    // Assignment operators
    Eq,        // =
    PlusEq,    // +=
    MinusEq,   // -=
    StarEq,    // *=
    SlashEq,   // /=
    PercentEq, // %=
    LShiftEq,  // <<=
    RShiftEq,  // >>=
    AmpEq,     // &=
    CaretEq,   // ^=
    PipeEq,    // |=
    // Increment/Decrement
    PlusPlus,   // ++
    MinusMinus, // --
}

/// Tokenize an arithmetic expression string into a sequence of tokens.
fn arith_tokenize(input: &str) -> Vec<ArithToken> {
    let bytes = input.as_bytes();
    let mut tokens = Vec::new();
    let mut pos = 0;

    while pos < bytes.len() {
        let b = bytes[pos];
        if b.is_ascii_whitespace() {
            pos += 1;
        } else if b.is_ascii_digit() {
            arith_tokenize_number(input, bytes, &mut pos, &mut tokens);
        } else if b.is_ascii_alphabetic() || b == b'_' {
            arith_tokenize_ident(input, bytes, &mut pos, &mut tokens);
        } else {
            arith_tokenize_operator(bytes, &mut pos, &mut tokens);
        }
    }
    tokens
}

/// Tokenize a numeric literal (decimal, hex, binary, octal, or base#value).
fn arith_tokenize_number(input: &str, bytes: &[u8], pos: &mut usize, tokens: &mut Vec<ArithToken>) {
    let start = *pos;
    let b = bytes[*pos];
    if let Some(number) = arith_tokenize_prefixed_number(input, bytes, pos, start, b) {
        tokens.push(ArithToken::Number(number));
        return;
    }
    *pos += 1;
    while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
        *pos += 1;
    }
    if let Some(number) = arith_tokenize_base_syntax(input, bytes, pos, start) {
        tokens.push(ArithToken::Number(number));
        return;
    }
    tokens.push(ArithToken::Number(
        input[start..*pos].parse::<i64>().unwrap_or(0),
    ));
}

fn arith_tokenize_prefixed_number(
    input: &str,
    bytes: &[u8],
    pos: &mut usize,
    start: usize,
    first: u8,
) -> Option<i64> {
    if first != b'0' || *pos + 1 >= bytes.len() {
        return None;
    }

    match bytes[*pos + 1] {
        b'x' | b'X' => Some(arith_take_radix_number(
            input,
            bytes,
            pos,
            start + 2,
            2,
            16,
            |b| b.is_ascii_hexdigit(),
        )),
        b'b' | b'B' => Some(arith_take_radix_number(
            input,
            bytes,
            pos,
            start + 2,
            2,
            2,
            |b| b == b'0' || b == b'1',
        )),
        next if next.is_ascii_digit() => Some(arith_take_radix_number(
            input,
            bytes,
            pos,
            start + 1,
            1,
            8,
            |b| b.is_ascii_digit(),
        )),
        _ => None,
    }
}

fn arith_take_radix_number(
    input: &str,
    bytes: &[u8],
    pos: &mut usize,
    digits_start: usize,
    prefix_len: usize,
    radix: u32,
    pred: impl Fn(u8) -> bool,
) -> i64 {
    *pos += prefix_len;
    while *pos < bytes.len() && pred(bytes[*pos]) {
        *pos += 1;
    }
    i64::from_str_radix(&input[digits_start..*pos], radix).unwrap_or(0)
}

fn arith_tokenize_base_syntax(
    input: &str,
    bytes: &[u8],
    pos: &mut usize,
    start: usize,
) -> Option<i64> {
    if *pos >= bytes.len() || bytes[*pos] != b'#' {
        return None;
    }
    let Ok(base) = input[start..*pos].parse::<u32>() else {
        return None;
    };
    if !(2..=64).contains(&base) {
        return None;
    }

    *pos += 1;
    let val_start = *pos;
    while *pos < bytes.len() && (bytes[*pos].is_ascii_alphanumeric() || bytes[*pos] == b'_') {
        *pos += 1;
    }
    Some(i64::from_str_radix(&input[val_start..*pos], base).unwrap_or(0))
}

/// Tokenize an identifier (variable name).
fn arith_tokenize_ident(input: &str, bytes: &[u8], pos: &mut usize, tokens: &mut Vec<ArithToken>) {
    let start = *pos;
    *pos += 1;
    while *pos < bytes.len() && (bytes[*pos].is_ascii_alphanumeric() || bytes[*pos] == b'_') {
        *pos += 1;
    }
    tokens.push(ArithToken::Ident(input[start..*pos].to_string()));
}

/// Helper: check if a second byte matches and push a 2-char or 1-char token accordingly.
fn arith_push_op2(
    bytes: &[u8],
    pos: &mut usize,
    next: u8,
    tok2: ArithToken,
    tok1: ArithToken,
    tokens: &mut Vec<ArithToken>,
) {
    let remaining = bytes.len() - *pos;
    if remaining > 1 && bytes[*pos + 1] == next {
        tokens.push(tok2);
        *pos += 2;
    } else {
        tokens.push(tok1);
        *pos += 1;
    }
}

/// Tokenize a `+`, `-`, or `*` operator (which each have three possible forms).
fn arith_tokenize_plus_minus_star(bytes: &[u8], pos: &mut usize, tokens: &mut Vec<ArithToken>) {
    let b = bytes[*pos];
    let remaining = bytes.len() - *pos;
    let next = if remaining > 1 {
        Some(bytes[*pos + 1])
    } else {
        None
    };

    match b {
        b'+' => match next {
            Some(b'+') => {
                tokens.push(ArithToken::PlusPlus);
                *pos += 2;
            }
            Some(b'=') => {
                tokens.push(ArithToken::PlusEq);
                *pos += 2;
            }
            _ => {
                tokens.push(ArithToken::Plus);
                *pos += 1;
            }
        },
        b'-' => match next {
            Some(b'-') => {
                tokens.push(ArithToken::MinusMinus);
                *pos += 2;
            }
            Some(b'=') => {
                tokens.push(ArithToken::MinusEq);
                *pos += 2;
            }
            _ => {
                tokens.push(ArithToken::Minus);
                *pos += 1;
            }
        },
        // b'*'
        _ => {
            if remaining > 1 && bytes[*pos + 1] == b'*' {
                tokens.push(ArithToken::StarStar);
                *pos += 2;
            } else if next == Some(b'=') {
                tokens.push(ArithToken::StarEq);
                *pos += 2;
            } else {
                tokens.push(ArithToken::Star);
                *pos += 1;
            }
        }
    }
}

/// Tokenize `<` or `>` operators (which each have up to four forms including shift-assign).
fn arith_tokenize_angle(bytes: &[u8], pos: &mut usize, tokens: &mut Vec<ArithToken>) {
    if bytes[*pos] == b'<' {
        arith_tokenize_left_angle(bytes, pos, tokens);
    } else {
        arith_tokenize_right_angle(bytes, pos, tokens);
    }
}

fn arith_tokenize_left_angle(bytes: &[u8], pos: &mut usize, tokens: &mut Vec<ArithToken>) {
    match (bytes.get(*pos + 1), bytes.get(*pos + 2)) {
        (Some(b'<'), Some(b'=')) => {
            tokens.push(ArithToken::LShiftEq);
            *pos += 3;
        }
        (Some(b'<'), _) => {
            tokens.push(ArithToken::LShift);
            *pos += 2;
        }
        (Some(b'='), _) => {
            tokens.push(ArithToken::Le);
            *pos += 2;
        }
        _ => {
            tokens.push(ArithToken::Lt);
            *pos += 1;
        }
    }
}

fn arith_tokenize_right_angle(bytes: &[u8], pos: &mut usize, tokens: &mut Vec<ArithToken>) {
    match (bytes.get(*pos + 1), bytes.get(*pos + 2)) {
        (Some(b'>'), Some(b'=')) => {
            tokens.push(ArithToken::RShiftEq);
            *pos += 3;
        }
        (Some(b'>'), _) => {
            tokens.push(ArithToken::RShift);
            *pos += 2;
        }
        (Some(b'='), _) => {
            tokens.push(ArithToken::Ge);
            *pos += 2;
        }
        _ => {
            tokens.push(ArithToken::Gt);
            *pos += 1;
        }
    }
}

/// Tokenize `&` or `|` operators (which each have three forms).
fn arith_tokenize_amp_pipe(bytes: &[u8], pos: &mut usize, tokens: &mut Vec<ArithToken>) {
    let b = bytes[*pos];
    let remaining = bytes.len() - *pos;
    let next = if remaining > 1 {
        Some(bytes[*pos + 1])
    } else {
        None
    };

    if b == b'&' {
        match next {
            Some(b'&') => {
                tokens.push(ArithToken::AmpAmp);
                *pos += 2;
            }
            Some(b'=') => {
                tokens.push(ArithToken::AmpEq);
                *pos += 2;
            }
            _ => {
                tokens.push(ArithToken::Amp);
                *pos += 1;
            }
        }
    } else {
        // b'|'
        match next {
            Some(b'|') => {
                tokens.push(ArithToken::PipePipe);
                *pos += 2;
            }
            Some(b'=') => {
                tokens.push(ArithToken::PipeEq);
                *pos += 2;
            }
            _ => {
                tokens.push(ArithToken::Pipe);
                *pos += 1;
            }
        }
    }
}

/// Tokenize a single-character punctuation token.
fn arith_tokenize_single(pos: &mut usize, tok: ArithToken, tokens: &mut Vec<ArithToken>) {
    tokens.push(tok);
    *pos += 1;
}

/// Tokenize an operator (single or multi-character).
fn arith_tokenize_operator(bytes: &[u8], pos: &mut usize, tokens: &mut Vec<ArithToken>) {
    match bytes[*pos] {
        b'+' | b'-' | b'*' => arith_tokenize_plus_minus_star(bytes, pos, tokens),
        b'/' => arith_push_op2(
            bytes,
            pos,
            b'=',
            ArithToken::SlashEq,
            ArithToken::Slash,
            tokens,
        ),
        b'%' => arith_push_op2(
            bytes,
            pos,
            b'=',
            ArithToken::PercentEq,
            ArithToken::Percent,
            tokens,
        ),
        b'<' | b'>' => arith_tokenize_angle(bytes, pos, tokens),
        b'=' => arith_push_op2(bytes, pos, b'=', ArithToken::EqEq, ArithToken::Eq, tokens),
        b'!' => arith_push_op2(bytes, pos, b'=', ArithToken::Ne, ArithToken::Bang, tokens),
        b'&' | b'|' => arith_tokenize_amp_pipe(bytes, pos, tokens),
        b'^' => arith_push_op2(
            bytes,
            pos,
            b'=',
            ArithToken::CaretEq,
            ArithToken::Caret,
            tokens,
        ),
        b'~' => arith_tokenize_single(pos, ArithToken::Tilde, tokens),
        b'?' => arith_tokenize_single(pos, ArithToken::Question, tokens),
        b':' => arith_tokenize_single(pos, ArithToken::Colon, tokens),
        b',' => arith_tokenize_single(pos, ArithToken::Comma, tokens),
        b'(' => arith_tokenize_single(pos, ArithToken::LParen, tokens),
        b')' => arith_tokenize_single(pos, ArithToken::RParen, tokens),
        _ => {
            *pos += 1;
        } // Skip $ and unknown characters
    }
}

/// Parser state for the arithmetic evaluator.
struct ArithParser<'a> {
    tokens: Vec<ArithToken>,
    pos: usize,
    state: &'a mut ShellState,
}

impl<'a> ArithParser<'a> {
    fn new(tokens: Vec<ArithToken>, state: &'a mut ShellState) -> Self {
        Self {
            tokens,
            pos: 0,
            state,
        }
    }

    fn peek(&self) -> Option<&ArithToken> {
        self.tokens.get(self.pos)
    }

    fn eat(&mut self, expected: &ArithToken) -> bool {
        if self.peek() == Some(expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// Resolve a variable name to its integer value (0 if unset or non-numeric).
    fn var_get(&self, name: &str) -> i64 {
        self.state
            .get_var(name)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
    }

    /// Set a variable and return the value.
    fn var_set(&mut self, name: &str, val: i64) -> i64 {
        self.state
            .set_var(SmolStr::from(name), SmolStr::from(val.to_string()));
        val
    }

    // ------------------------------------------------------------------
    // Recursive-descent entry point: comma expression (lowest precedence)
    // ------------------------------------------------------------------

    fn parse_expr(&mut self) -> i64 {
        let mut val = self.parse_assign();
        while self.eat(&ArithToken::Comma) {
            val = self.parse_assign();
        }
        val
    }

    // Assignment: right-associative
    fn parse_assign(&mut self) -> i64 {
        // Check if the current token is an identifier followed by an assignment operator.
        // Save position so we can backtrack.
        let save = self.pos;

        if let Some(ArithToken::Ident(name)) = self.peek().cloned() {
            self.pos += 1;
            if let Some(result) = self.try_compound_assign(&name) {
                return result;
            }
            // Not an assignment -- backtrack
            self.pos = save;
        }

        self.parse_ternary()
    }

    /// Try to parse a compound assignment operator after an identifier.
    /// Returns `Some(result)` if an assignment operator was found, `None` otherwise.
    fn try_compound_assign(&mut self, name: &str) -> Option<i64> {
        let op = self.peek().cloned()?;
        if !Self::is_assign_op(&op) {
            return None;
        }
        self.pos += 1;
        let rhs = self.parse_assign();

        let result = if matches!(op, ArithToken::Eq) {
            rhs
        } else {
            let cur = self.var_get(name);
            Self::apply_compound_op(&op, cur, rhs)
        };
        Some(self.var_set(name, result))
    }

    /// Check whether a token is an assignment operator.
    fn is_assign_op(tok: &ArithToken) -> bool {
        matches!(
            tok,
            ArithToken::Eq
                | ArithToken::PlusEq
                | ArithToken::MinusEq
                | ArithToken::StarEq
                | ArithToken::SlashEq
                | ArithToken::PercentEq
                | ArithToken::LShiftEq
                | ArithToken::RShiftEq
                | ArithToken::AmpEq
                | ArithToken::CaretEq
                | ArithToken::PipeEq
        )
    }

    /// Apply a compound assignment operation to `cur` and `rhs`.
    fn apply_compound_op(op: &ArithToken, cur: i64, rhs: i64) -> i64 {
        match op {
            ArithToken::PlusEq => cur.wrapping_add(rhs),
            ArithToken::MinusEq => cur.wrapping_sub(rhs),
            ArithToken::StarEq => cur.wrapping_mul(rhs),
            ArithToken::SlashEq => {
                if rhs == 0 {
                    0
                } else {
                    cur.wrapping_div(rhs)
                }
            }
            ArithToken::PercentEq => {
                if rhs == 0 {
                    0
                } else {
                    cur.wrapping_rem(rhs)
                }
            }
            ArithToken::LShiftEq => cur.wrapping_shl(rhs as u32),
            ArithToken::RShiftEq => cur.wrapping_shr(rhs as u32),
            ArithToken::AmpEq => cur & rhs,
            ArithToken::CaretEq => cur ^ rhs,
            ArithToken::PipeEq => cur | rhs,
            _ => rhs,
        }
    }

    // Ternary: expr ? expr : expr (right-associative)
    fn parse_ternary(&mut self) -> i64 {
        let cond = self.parse_logical_or();
        if self.eat(&ArithToken::Question) {
            let then_val = self.parse_assign();
            // Expect colon
            let _ = self.eat(&ArithToken::Colon);
            let else_val = self.parse_assign();
            if cond != 0 {
                then_val
            } else {
                else_val
            }
        } else {
            cond
        }
    }

    // Logical OR: ||
    fn parse_logical_or(&mut self) -> i64 {
        let mut val = self.parse_logical_and();
        while self.eat(&ArithToken::PipePipe) {
            let rhs = self.parse_logical_and();
            val = i64::from(val != 0 || rhs != 0);
        }
        val
    }

    // Logical AND: &&
    fn parse_logical_and(&mut self) -> i64 {
        let mut val = self.parse_bitwise_or();
        while self.eat(&ArithToken::AmpAmp) {
            let rhs = self.parse_bitwise_or();
            val = i64::from(val != 0 && rhs != 0);
        }
        val
    }

    // Bitwise OR: |
    fn parse_bitwise_or(&mut self) -> i64 {
        let mut val = self.parse_bitwise_xor();
        while self.eat(&ArithToken::Pipe) {
            let rhs = self.parse_bitwise_xor();
            val |= rhs;
        }
        val
    }

    // Bitwise XOR: ^
    fn parse_bitwise_xor(&mut self) -> i64 {
        let mut val = self.parse_bitwise_and();
        while self.eat(&ArithToken::Caret) {
            let rhs = self.parse_bitwise_and();
            val ^= rhs;
        }
        val
    }

    // Bitwise AND: &
    fn parse_bitwise_and(&mut self) -> i64 {
        let mut val = self.parse_equality();
        while self.eat(&ArithToken::Amp) {
            let rhs = self.parse_equality();
            val &= rhs;
        }
        val
    }

    // Equality: == !=
    fn parse_equality(&mut self) -> i64 {
        let mut val = self.parse_relational();
        loop {
            if self.eat(&ArithToken::EqEq) {
                let rhs = self.parse_relational();
                val = i64::from(val == rhs);
            } else if self.eat(&ArithToken::Ne) {
                let rhs = self.parse_relational();
                val = i64::from(val != rhs);
            } else {
                break;
            }
        }
        val
    }

    // Relational: < > <= >=
    fn parse_relational(&mut self) -> i64 {
        let mut val = self.parse_shift();
        loop {
            if self.eat(&ArithToken::Lt) {
                let rhs = self.parse_shift();
                val = i64::from(val < rhs);
            } else if self.eat(&ArithToken::Gt) {
                let rhs = self.parse_shift();
                val = i64::from(val > rhs);
            } else if self.eat(&ArithToken::Le) {
                let rhs = self.parse_shift();
                val = i64::from(val <= rhs);
            } else if self.eat(&ArithToken::Ge) {
                let rhs = self.parse_shift();
                val = i64::from(val >= rhs);
            } else {
                break;
            }
        }
        val
    }

    // Shift: << >>
    fn parse_shift(&mut self) -> i64 {
        let mut val = self.parse_additive();
        loop {
            if self.eat(&ArithToken::LShift) {
                let rhs = self.parse_additive();
                val = val.wrapping_shl(rhs as u32);
            } else if self.eat(&ArithToken::RShift) {
                let rhs = self.parse_additive();
                val = val.wrapping_shr(rhs as u32);
            } else {
                break;
            }
        }
        val
    }

    // Addition/subtraction: + -
    fn parse_additive(&mut self) -> i64 {
        let mut val = self.parse_multiplicative();
        loop {
            if self.eat(&ArithToken::Plus) {
                let rhs = self.parse_multiplicative();
                val = val.wrapping_add(rhs);
            } else if self.eat(&ArithToken::Minus) {
                let rhs = self.parse_multiplicative();
                val = val.wrapping_sub(rhs);
            } else {
                break;
            }
        }
        val
    }

    // Multiplication/division/modulo: * / %
    fn parse_multiplicative(&mut self) -> i64 {
        let mut val = self.parse_exponentiation();
        loop {
            if self.eat(&ArithToken::Star) {
                let rhs = self.parse_exponentiation();
                val = val.wrapping_mul(rhs);
            } else if self.eat(&ArithToken::Slash) {
                let rhs = self.parse_exponentiation();
                val = if rhs == 0 { 0 } else { val.wrapping_div(rhs) };
            } else if self.eat(&ArithToken::Percent) {
                let rhs = self.parse_exponentiation();
                val = if rhs == 0 { 0 } else { val.wrapping_rem(rhs) };
            } else {
                break;
            }
        }
        val
    }

    // Exponentiation: ** (right-associative)
    fn parse_exponentiation(&mut self) -> i64 {
        let base = self.parse_unary();
        if self.eat(&ArithToken::StarStar) {
            let exp = self.parse_exponentiation(); // right-associative recursion
            wrapping_pow(base, exp)
        } else {
            base
        }
    }

    // Unary: ! ~ + - ++var --var
    fn parse_unary(&mut self) -> i64 {
        match self.peek().cloned() {
            Some(ArithToken::Bang) => {
                self.pos += 1;
                i64::from(self.parse_unary() == 0)
            }
            Some(ArithToken::Tilde) => {
                self.pos += 1;
                !self.parse_unary()
            }
            Some(ArithToken::Plus) => {
                self.pos += 1;
                self.parse_unary()
            }
            Some(ArithToken::Minus) => {
                self.pos += 1;
                self.parse_unary().wrapping_neg()
            }
            Some(ArithToken::PlusPlus) => self.parse_pre_incdec(1),
            Some(ArithToken::MinusMinus) => self.parse_pre_incdec(-1),
            _ => self.parse_postfix(),
        }
    }

    /// Parse a pre-increment (`++var`) or pre-decrement (`--var`) expression.
    fn parse_pre_incdec(&mut self, delta: i64) -> i64 {
        self.pos += 1; // skip ++ or --
        if let Some(ArithToken::Ident(name)) = self.peek().cloned() {
            self.pos += 1;
            let cur = self.var_get(&name);
            self.var_set(&name, cur.wrapping_add(delta))
        } else {
            0
        }
    }

    // Postfix: var++ var-- (handled inside parse_primary for identifiers)
    fn parse_postfix(&mut self) -> i64 {
        self.parse_primary()
    }

    // Primary: number, variable (with postfix ++/--), parenthesized expression
    fn parse_primary(&mut self) -> i64 {
        match self.peek().cloned() {
            Some(ArithToken::Number(n)) => {
                self.pos += 1;
                n
            }
            Some(ArithToken::Ident(name)) => {
                self.pos += 1;
                // Check for postfix ++ / --
                if let Some(ArithToken::PlusPlus) = self.peek() {
                    self.pos += 1;
                    let cur = self.var_get(&name);
                    self.var_set(&name, cur.wrapping_add(1));
                    cur // return old value (postfix)
                } else if let Some(ArithToken::MinusMinus) = self.peek() {
                    self.pos += 1;
                    let cur = self.var_get(&name);
                    self.var_set(&name, cur.wrapping_sub(1));
                    cur // return old value (postfix)
                } else {
                    self.var_get(&name)
                }
            }
            Some(ArithToken::LParen) => {
                self.pos += 1;
                let val = self.parse_expr();
                let _ = self.eat(&ArithToken::RParen);
                val
            }
            _ => {
                // Unexpected token or end — skip and return 0
                if self.pos < self.tokens.len() {
                    self.pos += 1;
                }
                0
            }
        }
    }
}

/// Wrapping integer exponentiation (base ** exp). Negative exponents yield 0.
fn wrapping_pow(base: i64, exp: i64) -> i64 {
    if exp < 0 {
        return 0;
    }
    let mut exp = exp as u64;
    let mut result: i64 = 1;
    let mut b = base;
    while exp > 0 {
        if exp & 1 == 1 {
            result = result.wrapping_mul(b);
        }
        b = b.wrapping_mul(b);
        exp >>= 1;
    }
    result
}

/// Evaluate a bash arithmetic expression and return its i64 result.
///
/// Supports the full set of bash arithmetic operators including parentheses,
/// assignment, ternary, logical, bitwise, comparison, shift, arithmetic,
/// exponentiation, and unary/postfix increment/decrement operators.
/// Variable references (bare names without `$`) are resolved from shell state;
/// unset variables default to 0.
pub fn eval_arithmetic(expr: &str, state: &mut ShellState) -> i64 {
    let tokens = arith_tokenize(expr.trim());
    if tokens.is_empty() {
        return 0;
    }
    let mut parser = ArithParser::new(tokens, state);
    parser.parse_expr()
}

/// Expand brace expressions in a word.
///
/// Supports comma lists (`{a,b,c}`) and integer ranges (`{1..10}`).
/// Braces can have a prefix and/or suffix: `pre{a,b}suf` → `preasuf`, `prebsuf`.
/// Returns a vec with a single element (the input) when no brace expansion applies.
pub fn expand_braces(word: &str) -> Vec<String> {
    let Some((start, end)) = find_brace_pair(word) else {
        return vec![word.to_string()];
    };
    try_expand_brace_pair(word, start, end).unwrap_or_else(|| vec![word.to_string()])
}

fn find_brace_pair(word: &str) -> Option<(usize, usize)> {
    let bytes = word.as_bytes();
    let mut brace_start = None;
    let mut depth: u32 = 0;

    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && bytes[i - 1] == b'\\' {
            continue;
        }
        match b {
            b'{' => {
                if depth == 0 {
                    brace_start = Some(i);
                }
                depth += 1;
            }
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    return Some((brace_start.unwrap_or(0), i));
                }
            }
            _ => {}
        }
    }
    None
}

/// Maximum number of items a single brace expansion can produce.
const MAX_BRACE_ITEMS: usize = 10_000;

/// Try to expand a matched brace pair at `(start, end)` within `word`.
/// Returns `Some(results)` if the brace content was a valid range or comma list.
fn try_expand_brace_pair(word: &str, start: usize, end: usize) -> Option<Vec<String>> {
    let prefix = &word[..start];
    let inner = &word[start + 1..end];
    let suffix = &word[end + 1..];

    // Try range pattern first, then comma list
    if let Some(expansions) = try_brace_range(inner) {
        return Some(brace_combine_and_recurse(prefix, &expansions, suffix));
    }
    if let Some(items) = split_brace_items(inner) {
        if items.len() > 1 {
            return Some(brace_combine_and_recurse(prefix, &items, suffix));
        }
    }
    None
}

/// Combine prefix/suffix with each brace item and recursively expand.
fn brace_combine_and_recurse(prefix: &str, items: &[String], suffix: &str) -> Vec<String> {
    let mut result = Vec::new();
    for item in items {
        if result.len() >= MAX_BRACE_ITEMS {
            break;
        }
        let combined = format!("{prefix}{item}{suffix}");
        result.extend(expand_braces(&combined));
    }
    result.truncate(MAX_BRACE_ITEMS);
    result
}

/// Try to parse `inner` as a range pattern `N..M` or `N..M..S`.
fn try_brace_range(inner: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = inner.splitn(3, "..").collect();
    if parts.len() < 2 {
        return None;
    }
    let start: i64 = parts[0].parse().ok()?;
    let end: i64 = parts[1].parse().ok()?;
    let step: i64 = parse_brace_step(&parts, start, end)?;

    let width = std::cmp::max(parts[0].len(), parts[1].len());
    let needs_pad = (parts[0].starts_with('0') && parts[0].len() > 1)
        || (parts[1].starts_with('0') && parts[1].len() > 1);

    let result = generate_range(start, end, step, width, needs_pad);
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Parse the step value from brace range parts, defaulting to +1 or -1.
fn parse_brace_step(parts: &[&str], start: i64, end: i64) -> Option<i64> {
    if parts.len() == 3 {
        let s: i64 = parts[2].parse().ok()?;
        if s == 0 {
            None
        } else {
            Some(s)
        }
    } else if start <= end {
        Some(1)
    } else {
        Some(-1)
    }
}

/// Generate the integer range items.
fn generate_range(start: i64, end: i64, step: i64, width: usize, needs_pad: bool) -> Vec<String> {
    let mut result = Vec::new();
    let mut cur = start;
    let in_bounds = |c: i64| -> bool {
        if step > 0 {
            c <= end
        } else {
            c >= end
        }
    };
    while in_bounds(cur) && result.len() < MAX_BRACE_ITEMS {
        if needs_pad {
            result.push(format!("{cur:0>width$}"));
        } else {
            result.push(cur.to_string());
        }
        cur = match cur.checked_add(step) {
            Some(v) => v,
            None => break,
        };
    }
    result
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
            b'}' => depth = depth.saturating_sub(1),
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
        assert_eq!(expand_braces("pre{x,y}suf"), vec!["prexsuf", "preysuf"]);
    }

    #[test]
    fn brace_range_ascending() {
        assert_eq!(expand_braces("{1..5}"), vec!["1", "2", "3", "4", "5"]);
    }

    #[test]
    fn brace_range_descending() {
        assert_eq!(expand_braces("{5..1}"), vec!["5", "4", "3", "2", "1"]);
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
        assert_eq!(expand_braces("{a,{b,c}}"), vec!["a", "b", "c"]);
    }

    #[test]
    fn brace_range_with_prefix() {
        assert_eq!(
            expand_braces("file{1..3}.txt"),
            vec!["file1.txt", "file2.txt", "file3.txt"]
        );
    }

    // ---- Case modification ----

    #[test]
    fn case_mod_upper_first() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "hello".into());
        let word = make_word(vec![WordPart::Parameter("x^".into())]);
        assert_eq!(expand_word(&word, &mut state), "Hello");
    }

    #[test]
    fn case_mod_upper_all() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "hello".into());
        let word = make_word(vec![WordPart::Parameter("x^^".into())]);
        assert_eq!(expand_word(&word, &mut state), "HELLO");
    }

    #[test]
    fn case_mod_lower_first() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "HELLO".into());
        let word = make_word(vec![WordPart::Parameter("x,".into())]);
        assert_eq!(expand_word(&word, &mut state), "hELLO");
    }

    #[test]
    fn case_mod_lower_all() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "HELLO".into());
        let word = make_word(vec![WordPart::Parameter("x,,".into())]);
        assert_eq!(expand_word(&word, &mut state), "hello");
    }

    #[test]
    fn case_mod_empty_string() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "".into());
        let word = make_word(vec![WordPart::Parameter("x^^".into())]);
        assert_eq!(expand_word(&word, &mut state), "");
    }

    #[test]
    fn case_mod_mixed() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "hELLO wORLD".into());
        let word = make_word(vec![WordPart::Parameter("x^^".into())]);
        assert_eq!(expand_word(&word, &mut state), "HELLO WORLD");
    }

    // ---- Anchored substitution ----

    #[test]
    fn subst_anchored_start() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "hello world".into());
        // ${x/#hello/hi}
        let word = make_word(vec![WordPart::Parameter("x/#hello/hi".into())]);
        assert_eq!(expand_word(&word, &mut state), "hi world");
    }

    #[test]
    fn subst_anchored_start_no_match() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "hello world".into());
        // ${x/#world/hi} — "world" is not at the start
        let word = make_word(vec![WordPart::Parameter("x/#world/hi".into())]);
        assert_eq!(expand_word(&word, &mut state), "hello world");
    }

    #[test]
    fn subst_anchored_end() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "hello world".into());
        // ${x/%world/earth}
        let word = make_word(vec![WordPart::Parameter("x/%world/earth".into())]);
        assert_eq!(expand_word(&word, &mut state), "hello earth");
    }

    #[test]
    fn subst_anchored_end_no_match() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "hello world".into());
        // ${x/%hello/hi} — "hello" is not at the end
        let word = make_word(vec![WordPart::Parameter("x/%hello/hi".into())]);
        assert_eq!(expand_word(&word, &mut state), "hello world");
    }

    #[test]
    fn subst_anchored_start_glob() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "hello world".into());
        // ${x/#hel*/HI} — hel* matches the entire string from start (greedy)
        let word = make_word(vec![WordPart::Parameter("x/#hel*/HI".into())]);
        assert_eq!(expand_word(&word, &mut state), "HI");
    }

    #[test]
    fn subst_anchored_start_glob_partial() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "hello world".into());
        // ${x/#h?llo/HI} — h?llo matches exactly "hello"
        let word = make_word(vec![WordPart::Parameter("x/#h?llo/HI".into())]);
        assert_eq!(expand_word(&word, &mut state), "HI world");
    }

    #[test]
    fn subst_anchored_end_glob() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "hello world".into());
        // ${x/%*rld/EARTH} — *rld matches the entire string from end (greedy)
        let word = make_word(vec![WordPart::Parameter("x/%*rld/EARTH".into())]);
        assert_eq!(expand_word(&word, &mut state), "EARTH");
    }

    #[test]
    fn subst_anchored_end_glob_partial() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "hello world".into());
        // ${x/%w?rld/EARTH} — w?rld matches "world" at end
        let word = make_word(vec![WordPart::Parameter("x/%w?rld/EARTH".into())]);
        assert_eq!(expand_word(&word, &mut state), "hello EARTH");
    }

    // ---- Glob patterns in substitution ----

    #[test]
    fn subst_glob_star() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "foo.bar.baz".into());
        // ${x/.*./::} — replace first match of .*.
        let word = make_word(vec![WordPart::Parameter("x/.*./::.".into())]);
        assert_eq!(expand_word(&word, &mut state), "foo::.baz");
    }

    #[test]
    fn subst_glob_question() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "cat".into());
        // ${x/c?t/dog}
        let word = make_word(vec![WordPart::Parameter("x/c?t/dog".into())]);
        assert_eq!(expand_word(&word, &mut state), "dog");
    }

    #[test]
    fn subst_glob_global() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "aXbXc".into());
        // ${x//X/Y} — literal global replace still works
        let word = make_word(vec![WordPart::Parameter("x//X/Y".into())]);
        assert_eq!(expand_word(&word, &mut state), "aYbYc");
    }

    #[test]
    fn subst_literal_still_works() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "hello world".into());
        // ${x/world/earth} — literal (no glob chars)
        let word = make_word(vec![WordPart::Parameter("x/world/earth".into())]);
        assert_eq!(expand_word(&word, &mut state), "hello earth");
    }

    // ---- Indirect expansion ----

    #[test]
    fn indirect_expansion() {
        let mut state = ShellState::new();
        state.set_var("ref".into(), "target".into());
        state.set_var("target".into(), "value".into());
        // ${!ref} — should expand to "value"
        let word = make_word(vec![WordPart::Parameter("!ref".into())]);
        assert_eq!(expand_word(&word, &mut state), "value");
    }

    #[test]
    fn indirect_expansion_unset_ref() {
        let mut state = ShellState::new();
        // ${!ref} — ref is unset
        let word = make_word(vec![WordPart::Parameter("!ref".into())]);
        assert_eq!(expand_word(&word, &mut state), "");
    }

    #[test]
    fn indirect_expansion_unset_target() {
        let mut state = ShellState::new();
        state.set_var("ref".into(), "nonexistent".into());
        // ${!ref} — ref -> nonexistent, but nonexistent is unset
        let word = make_word(vec![WordPart::Parameter("!ref".into())]);
        assert_eq!(expand_word(&word, &mut state), "");
    }

    // ---- Prefix name expansion ----

    #[test]
    fn prefix_expansion_star() {
        let mut state = ShellState::new();
        state.set_var("FOO_A".into(), "1".into());
        state.set_var("FOO_B".into(), "2".into());
        state.set_var("BAR_C".into(), "3".into());
        // ${!FOO_*}
        let word = make_word(vec![WordPart::Parameter("!FOO_*".into())]);
        let result = expand_word(&word, &mut state);
        // Names should be sorted alphabetically
        assert_eq!(result, "FOO_A FOO_B");
    }

    #[test]
    fn prefix_expansion_at() {
        let mut state = ShellState::new();
        state.set_var("APP_X".into(), "a".into());
        state.set_var("APP_Y".into(), "b".into());
        // ${!APP_@}
        let word = make_word(vec![WordPart::Parameter("!APP_@".into())]);
        let result = expand_word(&word, &mut state);
        assert_eq!(result, "APP_X APP_Y");
    }

    #[test]
    fn prefix_expansion_no_matches() {
        let mut state = ShellState::new();
        state.set_var("OTHER".into(), "x".into());
        // ${!ZZZ_*}
        let word = make_word(vec![WordPart::Parameter("!ZZZ_*".into())]);
        assert_eq!(expand_word(&word, &mut state), "");
    }

    // ---- Transformation operators ----

    #[test]
    fn transform_at_q() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "hello world".into());
        // ${x@Q}
        let word = make_word(vec![WordPart::Parameter("x@Q".into())]);
        assert_eq!(expand_word(&word, &mut state), "'hello world'");
    }

    #[test]
    fn transform_at_q_with_quotes() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "it's".into());
        // ${x@Q}
        let word = make_word(vec![WordPart::Parameter("x@Q".into())]);
        assert_eq!(expand_word(&word, &mut state), "'it'\\''s'");
    }

    #[test]
    fn transform_at_u() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "hello".into());
        // ${x@U}
        let word = make_word(vec![WordPart::Parameter("x@U".into())]);
        assert_eq!(expand_word(&word, &mut state), "HELLO");
    }

    #[test]
    fn transform_at_l() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "HELLO".into());
        // ${x@L}
        let word = make_word(vec![WordPart::Parameter("x@L".into())]);
        assert_eq!(expand_word(&word, &mut state), "hello");
    }

    #[test]
    fn transform_at_e() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "hello\\nworld".into());
        // ${x@E}
        let word = make_word(vec![WordPart::Parameter("x@E".into())]);
        assert_eq!(expand_word(&word, &mut state), "hello\nworld");
    }

    #[test]
    fn transform_at_e_tab() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "a\\tb".into());
        let word = make_word(vec![WordPart::Parameter("x@E".into())]);
        assert_eq!(expand_word(&word, &mut state), "a\tb");
    }

    #[test]
    fn transform_at_lowercase_u() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "hello".into());
        // ${x@u} — uppercase first char only
        let word = make_word(vec![WordPart::Parameter("x@u".into())]);
        assert_eq!(expand_word(&word, &mut state), "Hello");
    }

    #[test]
    fn transform_at_a_uppercase() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "val".into());
        // ${x@A} — assignment form
        let word = make_word(vec![WordPart::Parameter("x@A".into())]);
        assert_eq!(expand_word(&word, &mut state), "declare -- x=\"val\"");
    }

    #[test]
    fn transform_unset_var() {
        let mut state = ShellState::new();
        // ${x@Q} where x is unset — should quote empty
        let word = make_word(vec![WordPart::Parameter("x@Q".into())]);
        assert_eq!(expand_word(&word, &mut state), "''");
    }

    // ---- Glob matching utility tests ----

    #[test]
    fn glob_match_basic() {
        assert!(simple_glob_match("hello", "hello"));
        assert!(!simple_glob_match("hello", "world"));
    }

    #[test]
    fn glob_match_star() {
        assert!(simple_glob_match("hel*", "hello"));
        assert!(simple_glob_match("*lo", "hello"));
        assert!(simple_glob_match("h*o", "hello"));
        assert!(simple_glob_match("*", "anything"));
        assert!(simple_glob_match("*", ""));
    }

    #[test]
    fn glob_match_question() {
        assert!(simple_glob_match("h?llo", "hello"));
        assert!(!simple_glob_match("h?llo", "hllo"));
        assert!(simple_glob_match("???", "abc"));
        assert!(!simple_glob_match("???", "abcd"));
    }

    #[test]
    fn glob_match_combined() {
        assert!(simple_glob_match("h?l*", "hello"));
        assert!(simple_glob_match("*l?o", "hello"));
    }

    // ---- Existing features still work ----

    #[test]
    fn existing_subst_still_works() {
        let mut state = ShellState::new();
        state.set_var("PATH".into(), "/usr/bin:/usr/local/bin:/bin".into());
        // ${PATH//:/ } — global replace literal
        let word = make_word(vec![WordPart::Parameter("PATH//:/ ".into())]);
        assert_eq!(
            expand_word(&word, &mut state),
            "/usr/bin /usr/local/bin /bin"
        );
    }

    #[test]
    fn existing_prefix_strip_still_works() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "/home/user/file.txt".into());
        let word = make_word(vec![WordPart::Parameter("x##*/".into())]);
        assert_eq!(expand_word(&word, &mut state), "file.txt");
    }

    #[test]
    fn existing_suffix_strip_still_works() {
        let mut state = ShellState::new();
        state.set_var("x".into(), "file.tar.gz".into());
        let word = make_word(vec![WordPart::Parameter("x%%.*".into())]);
        assert_eq!(expand_word(&word, &mut state), "file");
    }
}
