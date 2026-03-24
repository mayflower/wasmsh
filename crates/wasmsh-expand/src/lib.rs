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

/// Expand `$var` and `${...}` references in a raw string (e.g. here-doc body).
pub fn expand_string(text: &str, state: &mut ShellState) -> String {
    expand_operand(text, state)
}

fn expand_part(part: &WordPart, state: &mut ShellState, out: &mut String) {
    expand_part_depth(part, state, out, 0);
}

fn expand_part_depth(part: &WordPart, state: &mut ShellState, out: &mut String, depth: usize) {
    if depth > MAX_EXPAND_DEPTH {
        // Push the raw text instead of silently returning nothing
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
        WordPart::Parameter(name) => {
            // ── Array subscript expansions ──────────────────────────────
            // ${#arr[@]} or ${#arr[*]} — array length
            if let Some(var_name) = name.strip_prefix('#') {
                if let Some(base) = var_name
                    .strip_suffix("[@]")
                    .or_else(|| var_name.strip_suffix("[*]"))
                {
                    if !base.is_empty() {
                        let len = state.get_array_length(base);
                        out.push_str(&len.to_string());
                        return;
                    }
                }
            }
            // ${!arr[@]} or ${!arr[*]} — array keys
            if let Some(var_name) = name.strip_prefix('!') {
                if let Some(base) = var_name
                    .strip_suffix("[@]")
                    .or_else(|| var_name.strip_suffix("[*]"))
                {
                    if !base.is_empty() {
                        let keys = state.get_array_keys(base);
                        out.push_str(&keys.join(" "));
                        return;
                    }
                }
            }
            // ${arr[@]} or ${arr[*]} — all array values
            if let Some(base) = name
                .strip_suffix("[@]")
                .or_else(|| name.strip_suffix("[*]"))
            {
                if !base.is_empty() {
                    let values = state.get_array_values(base);
                    let joined: Vec<&str> = values.iter().map(SmolStr::as_str).collect();
                    out.push_str(&joined.join(" "));
                    return;
                }
            }
            // ${arr[N]} — single element access
            if let Some(bracket_pos) = name.find('[') {
                if let Some(end) = name[bracket_pos..].find(']') {
                    let base = &name[..bracket_pos];
                    let index = &name[bracket_pos + 1..bracket_pos + end];
                    if !base.is_empty() && !index.is_empty() {
                        // Expand variable references in the index (e.g. $key in ${arr[$key]})
                        let expanded_index = if index.contains('$') {
                            expand_string(index, state)
                        } else {
                            index.to_string()
                        };
                        if let Some(val) = state.get_array_element(base, &expanded_index) {
                            out.push_str(&val);
                        }
                        return;
                    }
                }
            }

            // ${#var} — string length
            if let Some(var_name) = name.strip_prefix('#') {
                if !var_name.is_empty() {
                    let len = state.get_var(var_name).map_or(0, |v| v.len());
                    out.push_str(&len.to_string());
                    return;
                }
                // Bare "#" is the special parameter $# (handled by get_var)
            }

            // ── Indirect expansion ${!name} and prefix expansion ${!prefix*}/${!prefix@} ──
            if let Some(rest) = name.strip_prefix('!') {
                // ${!prefix*} or ${!prefix@} — all variable names with prefix
                if let Some(prefix) = rest.strip_suffix('*').or_else(|| rest.strip_suffix('@')) {
                    if !prefix.is_empty()
                        && prefix
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    {
                        let names = state.var_names_with_prefix(prefix);
                        let joined: Vec<&str> = names.iter().map(SmolStr::as_str).collect();
                        out.push_str(&joined.join(" "));
                        return;
                    }
                }
                // ${!name} — indirect expansion (simple variable name only)
                if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    if let Some(indirect_name) = state.get_var(rest) {
                        if let Some(val) = state.get_var(&indirect_name) {
                            out.push_str(&val);
                        }
                    }
                    return;
                }
            }

            // ── Case modification ${var^}, ${var^^}, ${var,}, ${var,,} ──
            if let Some(case_result) = try_case_modification(name.as_str(), state) {
                out.push_str(&case_result);
                return;
            }

            // ── Transformation operators ${var@Q}, ${var@E}, ${var@U}, ${var@L}, etc. ──
            if let Some(transform_result) = try_transform_operator(name.as_str(), state) {
                out.push_str(&transform_result);
                return;
            }

            // ${var/pat/rep} or ${var//pat/rep} — substitution (with glob and anchoring)
            // Only match when '/' directly follows a valid variable name.
            if let Some(slash_pos) = name.find('/') {
                let var_name = &name[..slash_pos];
                let is_valid_var = !var_name.is_empty()
                    && var_name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_');
                if is_valid_var {
                    let rest = &name[slash_pos + 1..];
                    let global = rest.starts_with('/');
                    let rest = if global { &rest[1..] } else { rest };

                    // Detect anchor: # = start, % = end
                    let (anchor, pat_str) = if let Some(p) = rest.strip_prefix('#') {
                        if let Some(sep) = p.find('/') {
                            ('S', &p[..sep])
                        } else {
                            ('S', p)
                        }
                    } else if let Some(p) = rest.strip_prefix('%') {
                        if let Some(sep) = p.find('/') {
                            ('E', &p[..sep])
                        } else {
                            ('E', p)
                        }
                    } else {
                        ('N', "") // no anchor; pat_str unused, computed below
                    };

                    let (pat, rep) = if anchor != 'N' {
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
                    } else if let Some(sep) = rest.find('/') {
                        (&rest[..sep], &rest[sep + 1..])
                    } else {
                        (rest, "")
                    };

                    if let Some(val) = state.get_var(var_name) {
                        let result = match anchor {
                            'S' => {
                                if let Some(match_len) = glob_match_at_start(&val, pat_str) {
                                    format!("{rep}{}", &val[match_len..])
                                } else {
                                    val.to_string()
                                }
                            }
                            'E' => {
                                if let Some(match_start) = glob_match_at_end(&val, pat_str) {
                                    format!("{}{rep}", &val[..match_start])
                                } else {
                                    val.to_string()
                                }
                            }
                            _ => {
                                if global {
                                    glob_replace_all(&val, pat, rep)
                                } else {
                                    glob_replace_first(&val, pat, rep)
                                }
                            }
                        };
                        out.push_str(&result);
                    }
                    return;
                }
            }
            // ${var:offset} or ${var:offset:length} — substring
            if let Some(colon_pos) = name.find(':') {
                let var_name = &name[..colon_pos];
                let rest = &name[colon_pos + 1..];
                // Check it's a numeric offset, not an operator like :-, :+, :=, :?
                if rest.starts_with(|c: char| c.is_ascii_digit())
                    || (rest.starts_with('-')
                        && rest.len() > 1
                        && rest.as_bytes()[1].is_ascii_digit())
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
                expand_param_op_depth(var_name, operator, operand, state, out, depth);
            } else if let Some(val) = state.get_var(name) {
                out.push_str(&val);
            } else if state.get_var("SHOPT_u").as_deref() == Some("1") {
                // nounset: error on unset variable (skip special params)
                let is_special =
                    matches!(name.as_str(), "?" | "#" | "0" | "@" | "*" | "-" | "$" | "!")
                        || name.parse::<usize>().is_ok();
                if !is_special {
                    state.set_var(
                        SmolStr::from("_NOUNSET_ERROR"),
                        SmolStr::from(name.as_str()),
                    );
                }
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

/// Inner implementation with depth tracking.
fn expand_operand_inner(operand: &str, state: &mut ShellState, depth: usize) -> String {
    if depth > MAX_EXPAND_DEPTH || !operand.contains('$') {
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
                let inner = &operand[start..pos];
                if pos < bytes.len() {
                    pos += 1;
                } // skip }
                  // Recursively expand as a Parameter
                expand_part_depth(
                    &WordPart::Parameter(SmolStr::from(inner)),
                    state,
                    &mut out,
                    depth + 1,
                );
            } else if pos < bytes.len() && (bytes[pos].is_ascii_alphabetic() || bytes[pos] == b'_')
            {
                let start = pos;
                while pos < bytes.len()
                    && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_')
                {
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
        ":-" => match val {
            Some(v) if !v.is_empty() => out.push_str(&v),
            _ => out.push_str(&expand_operand_inner(operand, state, depth + 1)),
        },
        "-" => match val {
            Some(v) => out.push_str(&v),
            None => out.push_str(&expand_operand_inner(operand, state, depth + 1)),
        },
        ":=" => match val {
            Some(v) if !v.is_empty() => out.push_str(&v),
            _ => {
                let expanded = expand_operand_inner(operand, state, depth + 1);
                state.set_var(SmolStr::from(var_name), SmolStr::from(expanded.as_str()));
                out.push_str(&expanded);
            }
        },
        "=" => {
            if let Some(v) = val {
                out.push_str(&v);
            } else {
                let expanded = expand_operand_inner(operand, state, depth + 1);
                state.set_var(SmolStr::from(var_name), SmolStr::from(expanded.as_str()));
                out.push_str(&expanded);
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
                let pat = expand_operand_inner(operand, state, depth + 1);
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
                let pat = expand_operand_inner(operand, state, depth + 1);
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
                let pat = expand_operand_inner(operand, state, depth + 1);
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
                let pat = expand_operand_inner(operand, state, depth + 1);
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
fn try_case_modification(name: &str, state: &ShellState) -> Option<String> {
    // Look for ^, ^^, ,, or , suffix on a variable name.
    // We need to find where the variable name ends and the modifier begins.
    // Variable names consist of alphanumeric chars and underscores.
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
    let rest = &name[var_end..];

    let modifier = if rest.starts_with("^^") {
        "^^"
    } else if rest.starts_with('^') {
        "^"
    } else if rest.starts_with(",,") {
        ",,"
    } else if rest.starts_with(',') {
        ","
    } else {
        return None;
    };

    // Pattern after modifier (optional, currently unused — default is `?` i.e. any char)
    // Bash supports ${var^^pattern} but most common usage has no pattern.

    let val = state.get_var(var_name)?;

    let result = match modifier {
        "^" => {
            // Uppercase first character
            let mut chars = val.chars();
            match chars.next() {
                Some(c) => {
                    let mut s = c.to_uppercase().to_string();
                    s.extend(chars);
                    s
                }
                None => String::new(),
            }
        }
        "^^" => val.to_uppercase(),
        "," => {
            // Lowercase first character
            let mut chars = val.chars();
            match chars.next() {
                Some(c) => {
                    let mut s = c.to_lowercase().to_string();
                    s.extend(chars);
                    s
                }
                None => String::new(),
            }
        }
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
        b'u' => {
            // Uppercase first character only
            let mut chars = val.chars();
            match chars.next() {
                Some(c) => {
                    let mut s = c.to_uppercase().to_string();
                    s.extend(chars);
                    s
                }
                None => String::new(),
            }
        }
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

/// Expand backslash escape sequences in a string (for ${var@E}).
fn expand_backslash_escapes(s: &str) -> String {
    let mut out = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 1;
            match bytes[i] {
                b'n' => out.push('\n'),
                b't' => out.push('\t'),
                b'r' => out.push('\r'),
                b'a' => out.push('\x07'),
                b'b' => out.push('\x08'),
                b'e' | b'E' => out.push('\x1b'),
                b'f' => out.push('\x0c'),
                b'v' => out.push('\x0b'),
                b'\\' => out.push('\\'),
                b'\'' => out.push('\''),
                b'"' => out.push('"'),
                b'0' => {
                    // Octal: \0NNN (up to 3 octal digits)
                    let start = i + 1;
                    let mut end = start;
                    while end < bytes.len()
                        && end - start < 3
                        && bytes[end] >= b'0'
                        && bytes[end] <= b'7'
                    {
                        end += 1;
                    }
                    if end > start {
                        let val = u8::from_str_radix(&s[start..end], 8).unwrap_or(0);
                        out.push(val as char);
                        i = end - 1; // will be incremented below
                    } else {
                        out.push('\0');
                    }
                }
                b'x' => {
                    // Hex: \xNN (up to 2 hex digits)
                    let start = i + 1;
                    let mut end = start;
                    while end < bytes.len() && end - start < 2 && bytes[end].is_ascii_hexdigit() {
                        end += 1;
                    }
                    if end > start {
                        let val = u8::from_str_radix(&s[start..end], 16).unwrap_or(0);
                        out.push(val as char);
                        i = end - 1;
                    } else {
                        out.push_str("\\x");
                    }
                }
                other => {
                    out.push('\\');
                    out.push(other as char);
                }
            }
            i += 1;
        } else {
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

        // Skip whitespace
        if b.is_ascii_whitespace() {
            pos += 1;
            continue;
        }

        // Numbers: decimal, hex (0x/0X), binary (0b/0B), octal (0 prefix)
        if b.is_ascii_digit() {
            let start = pos;
            if b == b'0' && pos + 1 < bytes.len() {
                let next = bytes[pos + 1];
                if next == b'x' || next == b'X' {
                    // Hex
                    pos += 2;
                    while pos < bytes.len() && bytes[pos].is_ascii_hexdigit() {
                        pos += 1;
                    }
                    let s = &input[start + 2..pos];
                    tokens.push(ArithToken::Number(i64::from_str_radix(s, 16).unwrap_or(0)));
                    continue;
                } else if next == b'b' || next == b'B' {
                    // Binary
                    pos += 2;
                    while pos < bytes.len() && (bytes[pos] == b'0' || bytes[pos] == b'1') {
                        pos += 1;
                    }
                    let s = &input[start + 2..pos];
                    tokens.push(ArithToken::Number(i64::from_str_radix(s, 2).unwrap_or(0)));
                    continue;
                } else if next.is_ascii_digit() {
                    // Octal (leading zero)
                    pos += 1;
                    while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                        pos += 1;
                    }
                    let s = &input[start + 1..pos];
                    tokens.push(ArithToken::Number(i64::from_str_radix(s, 8).unwrap_or(0)));
                    continue;
                }
            }
            // Regular decimal
            pos += 1;
            while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                pos += 1;
            }
            // Check for base#value syntax: e.g. 16#ff
            if pos < bytes.len() && bytes[pos] == b'#' {
                let base_str = &input[start..pos];
                if let Ok(base) = base_str.parse::<u32>() {
                    if (2..=64).contains(&base) {
                        pos += 1; // skip #
                        let val_start = pos;
                        while pos < bytes.len()
                            && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_')
                        {
                            pos += 1;
                        }
                        let val_str = &input[val_start..pos];
                        tokens.push(ArithToken::Number(
                            i64::from_str_radix(val_str, base).unwrap_or(0),
                        ));
                        continue;
                    }
                }
            }
            let s = &input[start..pos];
            tokens.push(ArithToken::Number(s.parse::<i64>().unwrap_or(0)));
            continue;
        }

        // Identifiers (variable names)
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = pos;
            pos += 1;
            while pos < bytes.len() && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') {
                pos += 1;
            }
            tokens.push(ArithToken::Ident(input[start..pos].to_string()));
            continue;
        }

        // Multi-character and single-character operators
        let remaining = bytes.len() - pos;
        match b {
            b'+' => {
                if remaining > 1 && bytes[pos + 1] == b'+' {
                    tokens.push(ArithToken::PlusPlus);
                    pos += 2;
                } else if remaining > 1 && bytes[pos + 1] == b'=' {
                    tokens.push(ArithToken::PlusEq);
                    pos += 2;
                } else {
                    tokens.push(ArithToken::Plus);
                    pos += 1;
                }
            }
            b'-' => {
                if remaining > 1 && bytes[pos + 1] == b'-' {
                    tokens.push(ArithToken::MinusMinus);
                    pos += 2;
                } else if remaining > 1 && bytes[pos + 1] == b'=' {
                    tokens.push(ArithToken::MinusEq);
                    pos += 2;
                } else {
                    tokens.push(ArithToken::Minus);
                    pos += 1;
                }
            }
            b'*' => {
                if remaining > 2 && bytes[pos + 1] == b'*' && bytes[pos + 2] == b'=' {
                    // **= is not standard bash, treat as ** then =
                    tokens.push(ArithToken::StarStar);
                    pos += 2;
                } else if remaining > 1 && bytes[pos + 1] == b'*' {
                    tokens.push(ArithToken::StarStar);
                    pos += 2;
                } else if remaining > 1 && bytes[pos + 1] == b'=' {
                    tokens.push(ArithToken::StarEq);
                    pos += 2;
                } else {
                    tokens.push(ArithToken::Star);
                    pos += 1;
                }
            }
            b'/' => {
                if remaining > 1 && bytes[pos + 1] == b'=' {
                    tokens.push(ArithToken::SlashEq);
                    pos += 2;
                } else {
                    tokens.push(ArithToken::Slash);
                    pos += 1;
                }
            }
            b'%' => {
                if remaining > 1 && bytes[pos + 1] == b'=' {
                    tokens.push(ArithToken::PercentEq);
                    pos += 2;
                } else {
                    tokens.push(ArithToken::Percent);
                    pos += 1;
                }
            }
            b'<' => {
                if remaining > 2 && bytes[pos + 1] == b'<' && bytes[pos + 2] == b'=' {
                    tokens.push(ArithToken::LShiftEq);
                    pos += 3;
                } else if remaining > 1 && bytes[pos + 1] == b'<' {
                    tokens.push(ArithToken::LShift);
                    pos += 2;
                } else if remaining > 1 && bytes[pos + 1] == b'=' {
                    tokens.push(ArithToken::Le);
                    pos += 2;
                } else {
                    tokens.push(ArithToken::Lt);
                    pos += 1;
                }
            }
            b'>' => {
                if remaining > 2 && bytes[pos + 1] == b'>' && bytes[pos + 2] == b'=' {
                    tokens.push(ArithToken::RShiftEq);
                    pos += 3;
                } else if remaining > 1 && bytes[pos + 1] == b'>' {
                    tokens.push(ArithToken::RShift);
                    pos += 2;
                } else if remaining > 1 && bytes[pos + 1] == b'=' {
                    tokens.push(ArithToken::Ge);
                    pos += 2;
                } else {
                    tokens.push(ArithToken::Gt);
                    pos += 1;
                }
            }
            b'=' => {
                if remaining > 1 && bytes[pos + 1] == b'=' {
                    tokens.push(ArithToken::EqEq);
                    pos += 2;
                } else {
                    tokens.push(ArithToken::Eq);
                    pos += 1;
                }
            }
            b'!' => {
                if remaining > 1 && bytes[pos + 1] == b'=' {
                    tokens.push(ArithToken::Ne);
                    pos += 2;
                } else {
                    tokens.push(ArithToken::Bang);
                    pos += 1;
                }
            }
            b'&' => {
                if remaining > 1 && bytes[pos + 1] == b'&' {
                    tokens.push(ArithToken::AmpAmp);
                    pos += 2;
                } else if remaining > 1 && bytes[pos + 1] == b'=' {
                    tokens.push(ArithToken::AmpEq);
                    pos += 2;
                } else {
                    tokens.push(ArithToken::Amp);
                    pos += 1;
                }
            }
            b'|' => {
                if remaining > 1 && bytes[pos + 1] == b'|' {
                    tokens.push(ArithToken::PipePipe);
                    pos += 2;
                } else if remaining > 1 && bytes[pos + 1] == b'=' {
                    tokens.push(ArithToken::PipeEq);
                    pos += 2;
                } else {
                    tokens.push(ArithToken::Pipe);
                    pos += 1;
                }
            }
            b'^' => {
                if remaining > 1 && bytes[pos + 1] == b'=' {
                    tokens.push(ArithToken::CaretEq);
                    pos += 2;
                } else {
                    tokens.push(ArithToken::Caret);
                    pos += 1;
                }
            }
            b'~' => {
                tokens.push(ArithToken::Tilde);
                pos += 1;
            }
            b'?' => {
                tokens.push(ArithToken::Question);
                pos += 1;
            }
            b':' => {
                tokens.push(ArithToken::Colon);
                pos += 1;
            }
            b',' => {
                tokens.push(ArithToken::Comma);
                pos += 1;
            }
            b'(' => {
                tokens.push(ArithToken::LParen);
                pos += 1;
            }
            b')' => {
                tokens.push(ArithToken::RParen);
                pos += 1;
            }
            b'$' => {
                // Skip $ before variable names (bash allows $var in arithmetic)
                pos += 1;
            }
            _ => {
                // Skip unknown characters
                pos += 1;
            }
        }
    }

    tokens
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
        // We need to check if the current token is an identifier followed by an
        // assignment operator. Save position so we can backtrack.
        let save = self.pos;

        if let Some(ArithToken::Ident(name)) = self.peek().cloned() {
            self.pos += 1;
            if let Some(op) = self.peek().cloned() {
                match op {
                    ArithToken::Eq => {
                        self.pos += 1;
                        let rhs = self.parse_assign();
                        return self.var_set(&name, rhs);
                    }
                    ArithToken::PlusEq => {
                        self.pos += 1;
                        let rhs = self.parse_assign();
                        let cur = self.var_get(&name);
                        return self.var_set(&name, cur.wrapping_add(rhs));
                    }
                    ArithToken::MinusEq => {
                        self.pos += 1;
                        let rhs = self.parse_assign();
                        let cur = self.var_get(&name);
                        return self.var_set(&name, cur.wrapping_sub(rhs));
                    }
                    ArithToken::StarEq => {
                        self.pos += 1;
                        let rhs = self.parse_assign();
                        let cur = self.var_get(&name);
                        return self.var_set(&name, cur.wrapping_mul(rhs));
                    }
                    ArithToken::SlashEq => {
                        self.pos += 1;
                        let rhs = self.parse_assign();
                        let cur = self.var_get(&name);
                        let result = if rhs == 0 { 0 } else { cur.wrapping_div(rhs) };
                        return self.var_set(&name, result);
                    }
                    ArithToken::PercentEq => {
                        self.pos += 1;
                        let rhs = self.parse_assign();
                        let cur = self.var_get(&name);
                        let result = if rhs == 0 { 0 } else { cur.wrapping_rem(rhs) };
                        return self.var_set(&name, result);
                    }
                    ArithToken::LShiftEq => {
                        self.pos += 1;
                        let rhs = self.parse_assign();
                        let cur = self.var_get(&name);
                        return self.var_set(&name, cur.wrapping_shl(rhs as u32));
                    }
                    ArithToken::RShiftEq => {
                        self.pos += 1;
                        let rhs = self.parse_assign();
                        let cur = self.var_get(&name);
                        return self.var_set(&name, cur.wrapping_shr(rhs as u32));
                    }
                    ArithToken::AmpEq => {
                        self.pos += 1;
                        let rhs = self.parse_assign();
                        let cur = self.var_get(&name);
                        return self.var_set(&name, cur & rhs);
                    }
                    ArithToken::CaretEq => {
                        self.pos += 1;
                        let rhs = self.parse_assign();
                        let cur = self.var_get(&name);
                        return self.var_set(&name, cur ^ rhs);
                    }
                    ArithToken::PipeEq => {
                        self.pos += 1;
                        let rhs = self.parse_assign();
                        let cur = self.var_get(&name);
                        return self.var_set(&name, cur | rhs);
                    }
                    _ => {}
                }
            }
            // Not an assignment — backtrack
            self.pos = save;
        }

        self.parse_ternary()
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
                let val = self.parse_unary();
                i64::from(val == 0)
            }
            Some(ArithToken::Tilde) => {
                self.pos += 1;
                let val = self.parse_unary();
                !val
            }
            Some(ArithToken::Plus) => {
                self.pos += 1;
                self.parse_unary()
            }
            Some(ArithToken::Minus) => {
                self.pos += 1;
                let val = self.parse_unary();
                val.wrapping_neg()
            }
            Some(ArithToken::PlusPlus) => {
                // Pre-increment: ++var
                self.pos += 1;
                if let Some(ArithToken::Ident(name)) = self.peek().cloned() {
                    self.pos += 1;
                    let cur = self.var_get(&name);
                    self.var_set(&name, cur.wrapping_add(1))
                } else {
                    0
                }
            }
            Some(ArithToken::MinusMinus) => {
                // Pre-decrement: --var
                self.pos += 1;
                if let Some(ArithToken::Ident(name)) = self.peek().cloned() {
                    self.pos += 1;
                    let cur = self.var_get(&name);
                    self.var_set(&name, cur.wrapping_sub(1))
                } else {
                    0
                }
            }
            _ => self.parse_postfix(),
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
                                    if result.len() >= MAX_BRACE_ITEMS {
                                        break;
                                    }
                                    // Recursively expand suffix
                                    let combined = format!("{prefix}{item}{suffix}");
                                    result.extend(expand_braces(&combined));
                                }
                                result.truncate(MAX_BRACE_ITEMS);
                                return result;
                            }

                            // Try comma list: {a,b,c}
                            if let Some(items) = split_brace_items(inner) {
                                if items.len() > 1 {
                                    let mut result = Vec::new();
                                    for item in &items {
                                        if result.len() >= MAX_BRACE_ITEMS {
                                            break;
                                        }
                                        let combined = format!("{prefix}{item}{suffix}");
                                        result.extend(expand_braces(&combined));
                                    }
                                    result.truncate(MAX_BRACE_ITEMS);
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

/// Maximum number of items a single brace expansion can produce.
const MAX_BRACE_ITEMS: usize = 10_000;

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
        if s == 0 {
            return None;
        }
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
        while cur <= end && result.len() < MAX_BRACE_ITEMS {
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
    } else {
        while cur >= end && result.len() < MAX_BRACE_ITEMS {
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
