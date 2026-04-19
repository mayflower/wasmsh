//! Evaluator for bash `[[ expr ]]` conditional expressions.
//!
//! Recursive descent over a pre-tokenised expression with precedence:
//! `||` < `&&` < `!` < primary (grouping, unary test, binary test,
//! string truth). Binary evaluation covers string equality/globbing
//! (`==`, `!=`, `=`), regex match (`=~` with `BASH_REMATCH` captures),
//! lexical comparison (`<`, `>`), and integer comparison (`-eq`, ...).

use wasmsh_fs::BackendFs;
use wasmsh_state::ShellState;

use crate::pattern::{glob_match_inner, regex_match_with_captures};

/// Evaluate an `||` expression (lowest precedence).
pub(crate) fn dbl_bracket_eval_or(
    tokens: &[String],
    pos: &mut usize,
    fs: &BackendFs,
    state: &mut ShellState,
) -> bool {
    let mut result = dbl_bracket_eval_and(tokens, pos, fs, state);
    while *pos < tokens.len() && tokens[*pos] == "||" {
        *pos += 1;
        let rhs = dbl_bracket_eval_and(tokens, pos, fs, state);
        result = result || rhs;
    }
    result
}

/// Evaluate an `&&` expression.
fn dbl_bracket_eval_and(
    tokens: &[String],
    pos: &mut usize,
    fs: &BackendFs,
    state: &mut ShellState,
) -> bool {
    let mut result = dbl_bracket_eval_not(tokens, pos, fs, state);
    while *pos < tokens.len() && tokens[*pos] == "&&" {
        *pos += 1;
        let rhs = dbl_bracket_eval_not(tokens, pos, fs, state);
        result = result && rhs;
    }
    result
}

/// Evaluate a `!` (negation) expression.
fn dbl_bracket_eval_not(
    tokens: &[String],
    pos: &mut usize,
    fs: &BackendFs,
    state: &mut ShellState,
) -> bool {
    if *pos < tokens.len() && tokens[*pos] == "!" {
        *pos += 1;
        return !dbl_bracket_eval_not(tokens, pos, fs, state);
    }
    dbl_bracket_eval_primary(tokens, pos, fs, state)
}

/// Evaluate a primary expression: grouped `(expr)`, unary test, binary test, or string truth.
fn dbl_bracket_eval_primary(
    tokens: &[String],
    pos: &mut usize,
    fs: &BackendFs,
    state: &mut ShellState,
) -> bool {
    if *pos >= tokens.len() {
        return false;
    }
    if let Some(result) = dbl_bracket_try_group(tokens, pos, fs, state) {
        return result;
    }
    if let Some(result) = dbl_bracket_try_unary(tokens, pos, fs) {
        return result;
    }
    if *pos + 1 == tokens.len() {
        return dbl_bracket_take_truthy_token(tokens, pos);
    }
    if let Some(result) = dbl_bracket_try_binary(tokens, pos, fs, state) {
        return result;
    }
    dbl_bracket_take_truthy_token(tokens, pos)
}

fn dbl_bracket_try_group(
    tokens: &[String],
    pos: &mut usize,
    fs: &BackendFs,
    state: &mut ShellState,
) -> Option<bool> {
    if tokens.get(*pos).map(String::as_str) != Some("(") {
        return None;
    }

    *pos += 1;
    let result = dbl_bracket_eval_or(tokens, pos, fs, state);
    if tokens.get(*pos).map(String::as_str) == Some(")") {
        *pos += 1;
    }
    Some(result)
}

fn dbl_bracket_take_truthy_token(tokens: &[String], pos: &mut usize) -> bool {
    let Some(token) = tokens.get(*pos) else {
        return false;
    };
    *pos += 1;
    !token.is_empty()
}

/// Try to evaluate a unary test (`-z`, `-n`, `-f`, etc.). Returns `None` if not a unary op.
fn dbl_bracket_try_unary(tokens: &[String], pos: &mut usize, fs: &BackendFs) -> Option<bool> {
    if *pos + 1 >= tokens.len() {
        return None;
    }
    let flag = dbl_bracket_parse_unary_flag(&tokens[*pos])?;
    match flag {
        b'z' | b'n' => Some(dbl_bracket_eval_string_test(tokens, pos, flag)),
        b'f' | b'd' | b'e' | b's' | b'r' | b'w' | b'x' | b'L' | b'h' | b'p' | b'S' | b't'
        | b'N' | b'O' | b'G' => dbl_bracket_eval_file_test(tokens, pos, flag, fs),
        _ => None,
    }
}

fn dbl_bracket_parse_unary_flag(op: &str) -> Option<u8> {
    if !op.starts_with('-') || op.len() != 2 {
        return None;
    }
    Some(op.as_bytes()[1])
}

fn dbl_bracket_eval_string_test(tokens: &[String], pos: &mut usize, flag: u8) -> bool {
    *pos += 1;
    let arg = &tokens[*pos];
    *pos += 1;
    if flag == b'z' {
        arg.is_empty()
    } else {
        !arg.is_empty()
    }
}

fn dbl_bracket_eval_file_test(
    tokens: &[String],
    pos: &mut usize,
    flag: u8,
    fs: &BackendFs,
) -> Option<bool> {
    if *pos + 2 < tokens.len() && is_binary_op(&tokens[*pos + 2]) {
        return None;
    }
    *pos += 1;
    let path_str = &tokens[*pos];
    *pos += 1;
    Some(eval_file_test(flag, path_str, fs))
}

/// Try to evaluate a binary test. Returns `None` if no binary op at pos+1.
fn dbl_bracket_try_binary(
    tokens: &[String],
    pos: &mut usize,
    fs: &BackendFs,
    state: &mut ShellState,
) -> Option<bool> {
    if *pos + 2 > tokens.len() {
        return None;
    }
    let op_idx = *pos + 1;
    if op_idx >= tokens.len() || !is_binary_op(&tokens[op_idx]) {
        return None;
    }
    let lhs = tokens[*pos].clone();
    *pos += 1;
    let op = tokens[*pos].clone();
    *pos += 1;

    let rhs = dbl_bracket_collect_rhs(tokens, pos, &op);
    Some(eval_binary_op(&lhs, &op, &rhs, fs, state))
}

/// Collect the right-hand side for a binary operator. For `=~`, the RHS extends
/// until `&&`, `||`, or end of tokens.
fn dbl_bracket_collect_rhs(tokens: &[String], pos: &mut usize, op: &str) -> String {
    if *pos >= tokens.len() {
        return String::new();
    }
    if op == "=~" {
        return dbl_bracket_collect_regex_rhs(tokens, pos);
    }
    let rhs = tokens[*pos].clone();
    *pos += 1;
    rhs
}

fn dbl_bracket_collect_regex_rhs(tokens: &[String], pos: &mut usize) -> String {
    let mut rhs = String::new();
    while *pos < tokens.len() && tokens[*pos] != "&&" && tokens[*pos] != "||" {
        rhs.push_str(&tokens[*pos]);
        *pos += 1;
    }
    rhs
}

/// Check whether a token is a binary operator in `[[ ]]` context.
fn is_binary_op(s: &str) -> bool {
    matches!(
        s,
        "==" | "!="
            | "=~"
            | "="
            | "<"
            | ">"
            | "-eq"
            | "-ne"
            | "-lt"
            | "-le"
            | "-gt"
            | "-ge"
            | "-ef"
            | "-nt"
            | "-ot"
    )
}

/// Evaluate a binary operation.
fn eval_binary_op(lhs: &str, op: &str, rhs: &str, fs: &BackendFs, state: &mut ShellState) -> bool {
    match op {
        "==" | "=" => glob_cmp(lhs, rhs, state, false),
        "!=" => !glob_cmp(lhs, rhs, state, false),
        "=~" => eval_regex_match(lhs, rhs, state),
        "<" => lhs < rhs,
        ">" => lhs > rhs,
        "-ef" => lhs == rhs,
        "-nt" => eval_file_test(b'e', lhs, fs) && !eval_file_test(b'e', rhs, fs),
        "-ot" => !eval_file_test(b'e', lhs, fs) && eval_file_test(b'e', rhs, fs),
        _ => eval_int_cmp(lhs, op, rhs),
    }
}

/// Glob-compare lhs against rhs pattern, respecting nocasematch.
fn glob_cmp(lhs: &str, rhs: &str, state: &ShellState, _negate: bool) -> bool {
    let nocasematch = state.get_var("SHOPT_nocasematch").as_deref() == Some("1");
    if nocasematch {
        glob_match_inner(rhs.to_lowercase().as_bytes(), lhs.to_lowercase().as_bytes())
    } else {
        glob_match_inner(rhs.as_bytes(), lhs.as_bytes())
    }
}

/// Evaluate a regex match (`=~`) with capture groups for `BASH_REMATCH`.
fn eval_regex_match(lhs: &str, rhs: &str, state: &mut ShellState) -> bool {
    let captures = regex_match_with_captures(lhs, rhs);
    let br_name = smol_str::SmolStr::from("BASH_REMATCH");
    let Some(caps) = captures else {
        state.init_indexed_array(br_name);
        return false;
    };
    state.init_indexed_array(br_name.clone());
    for (i, cap) in caps.iter().enumerate() {
        state.set_array_element(
            br_name.clone(),
            &i.to_string(),
            smol_str::SmolStr::from(cap.as_str()),
        );
    }
    true
}

/// Evaluate an integer comparison operator (`-eq`, `-ne`, `-lt`, `-le`, `-gt`, `-ge`).
fn eval_int_cmp(lhs: &str, op: &str, rhs: &str) -> bool {
    let a: i64 = lhs.trim().parse().unwrap_or(0);
    let b: i64 = rhs.trim().parse().unwrap_or(0);
    match op {
        "-eq" => a == b,
        "-ne" => a != b,
        "-lt" => a < b,
        "-le" => a <= b,
        "-gt" => a > b,
        "-ge" => a >= b,
        _ => false,
    }
}

/// Evaluate a unary file test.
fn eval_file_test(flag: u8, path: &str, fs: &BackendFs) -> bool {
    use wasmsh_fs::Vfs;
    if flag == b't' {
        return path == "0";
    }
    match fs.stat(path) {
        Ok(meta) => match flag {
            b'f' => !meta.is_dir,
            b'd' => meta.is_dir,
            b's' | b'N' => meta.size > 0,
            // -e, -r, -w, -x: in the VFS all existing files are accessible
            b'e' | b'r' | b'w' | b'x' | b'O' | b'G' => true,
            _ => false,
        },
        Err(_) => false,
    }
}
