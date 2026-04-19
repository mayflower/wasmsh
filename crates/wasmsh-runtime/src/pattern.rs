//! Pattern matching primitives: regex (`=~`), glob, and extglob.
//!
//! These are pure byte/string pattern matchers with no runtime/state
//! dependencies. They are deliberately self-contained to avoid pulling a
//! regex crate into the `wasm32-unknown-unknown` and
//! `wasm32-unknown-emscripten` builds.
//!
//! Entry points:
//! - [`regex_match_with_captures`] — bash `=~` with `BASH_REMATCH` captures.
//! - [`glob_match_inner`] / [`glob_match_ext`] — shell globbing.
//! - [`extglob_match`] — `?(...)`, `*(...)`, `+(...)`, `@(...)`, `!(...)`.

/// Strip anchoring from a regex pattern, returning (core, `anchored_start`, `anchored_end`).
fn regex_strip_anchors(pattern: &str) -> (&str, bool, bool) {
    let anchored_start = pattern.starts_with('^');
    let anchored_end = pattern.ends_with('$') && !pattern.ends_with("\\$");
    let core = match (anchored_start, anchored_end) {
        (true, true) if pattern.len() >= 2 => &pattern[1..pattern.len() - 1],
        (true, _) => &pattern[1..],
        (_, true) => &pattern[..pattern.len() - 1],
        _ => pattern,
    };
    (core, anchored_start, anchored_end)
}

/// Check if a regex core has any special regex metacharacters.
fn has_regex_metachar(core: &str) -> bool {
    core.contains('.')
        || core.contains('+')
        || core.contains('*')
        || core.contains('?')
        || core.contains('[')
        || core.contains('(')
        || core.contains('|')
}

/// Find match range for a literal pattern with anchoring.
fn literal_match_range(text: &str, core: &str, start: bool, end: bool) -> Option<(usize, usize)> {
    match (start, end) {
        (true, true) if text == core => Some((0, text.len())),
        (true, false) if text.starts_with(core) => Some((0, core.len())),
        (false, true) if text.ends_with(core) => Some((text.len() - core.len(), text.len())),
        (false, false) => text.find(core).map(|pos| (pos, pos + core.len())),
        _ => None,
    }
}

/// Regex match with capture group support.
///
/// Returns `Some(captures)` if the pattern matches, where `captures[0]` is the
/// full match and `captures[1..]` are the parenthesized subgroup matches.
/// Returns `None` if no match.
pub(crate) fn regex_match_with_captures(text: &str, pattern: &str) -> Option<Vec<String>> {
    let (core, anchored_start, anchored_end) = regex_strip_anchors(pattern);

    if !has_regex_metachar(core) {
        return regex_match_literal_with_captures(text, core, anchored_start, anchored_end);
    }

    regex_find_first_match(text, core, anchored_start, anchored_end)
}

fn regex_find_first_match(
    text: &str,
    core: &str,
    anchored_start: bool,
    anchored_end: bool,
) -> Option<Vec<String>> {
    let end = if anchored_start { 0 } else { text.len() };
    for start in 0..=end {
        if let Some(result) = regex_match_from_start(text, core, anchored_end, start) {
            return Some(result);
        }
    }
    None
}

fn regex_match_literal_with_captures(
    text: &str,
    core: &str,
    anchored_start: bool,
    anchored_end: bool,
) -> Option<Vec<String>> {
    literal_match_range(text, core, anchored_start, anchored_end)
        .map(|(s, e)| vec![text[s..e].to_string()])
}

fn regex_match_from_start(
    text: &str,
    core: &str,
    anchored_end: bool,
    start: usize,
) -> Option<Vec<String>> {
    let mut group_caps: Vec<(usize, usize)> = Vec::new();
    let end = regex_match_capturing(
        text.as_bytes(),
        start,
        core.as_bytes(),
        0,
        anchored_end,
        &mut group_caps,
    )?;
    Some(regex_build_capture_list(text, start, end, &group_caps))
}

fn regex_build_capture_list(
    text: &str,
    start: usize,
    end: usize,
    group_caps: &[(usize, usize)],
) -> Vec<String> {
    let mut result = vec![text[start..end].to_string()];
    for &(gs, ge) in group_caps {
        result.push(text[gs..ge].to_string());
    }
    result
}

/// Backtracking regex matcher with capture group support.
/// Returns `Some(end_position)` on match, `None` on no match.
/// `captures` accumulates (start, end) pairs for each parenthesized group.
fn regex_match_capturing(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    pi: usize,
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
) -> Option<usize> {
    if pi >= pat.len() {
        return regex_check_end(ti, text.len(), must_end);
    }

    if pat[pi] == b'(' {
        return regex_match_group(text, ti, pat, pi, must_end, captures);
    }

    regex_match_elem(text, ti, pat, pi, must_end, captures)
}

/// Check if end-of-pattern is valid given anchoring.
fn regex_check_end(ti: usize, text_len: usize, must_end: bool) -> Option<usize> {
    if must_end && ti < text_len {
        None
    } else {
        Some(ti)
    }
}

/// Handle a parenthesized group in the regex, dispatching by quantifier.
fn regex_match_group(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    pi: usize,
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
) -> Option<usize> {
    let close = find_matching_paren_bytes(pat, pi + 1)?;
    let inner = &pat[pi + 1..close];
    let rest = &pat[close + 1..];
    let (quant, after_quant_offset) = parse_group_quantifier(pat, close);
    let after_quant = &pat[after_quant_offset..];
    let alternatives = split_alternatives_bytes(inner);

    regex_dispatch_group_quant(
        text,
        ti,
        rest,
        after_quant,
        must_end,
        captures,
        &alternatives,
        quant,
    )
}

fn parse_group_quantifier(pat: &[u8], close: usize) -> (u8, usize) {
    if close + 1 < pat.len() {
        match pat[close + 1] {
            q @ (b'*' | b'+' | b'?') => (q, close + 2),
            _ => (0, close + 1),
        }
    } else {
        (0, close + 1)
    }
}

#[allow(clippy::too_many_arguments)]
fn regex_dispatch_group_quant(
    text: &[u8],
    ti: usize,
    rest: &[u8],
    after_quant: &[u8],
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    alternatives: &[Vec<u8>],
    quant: u8,
) -> Option<usize> {
    match quant {
        b'+' => regex_match_group_rep(text, ti, after_quant, must_end, captures, alternatives, 1),
        b'*' => regex_match_group_rep(text, ti, after_quant, must_end, captures, alternatives, 0),
        b'?' => regex_match_group_opt(text, ti, after_quant, must_end, captures, alternatives),
        _ => regex_match_group_exact(text, ti, rest, must_end, captures, alternatives),
    }
}

/// Match a group with repetition quantifier (+ or *).
fn regex_match_group_rep(
    text: &[u8],
    ti: usize,
    after: &[u8],
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    alternatives: &[Vec<u8>],
    min_reps: usize,
) -> Option<usize> {
    let save = captures.len();
    for end_pos in (ti..=text.len()).rev() {
        captures.truncate(save);
        if let Some(result) = regex_try_group_rep_at(
            text,
            ti,
            end_pos,
            after,
            must_end,
            captures,
            alternatives,
            min_reps,
            save,
        ) {
            return Some(result);
        }
    }
    captures.truncate(save);
    None
}

#[allow(clippy::too_many_arguments)]
fn regex_try_group_rep_at(
    text: &[u8],
    ti: usize,
    end_pos: usize,
    after: &[u8],
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    alternatives: &[Vec<u8>],
    min_reps: usize,
    save: usize,
) -> Option<usize> {
    if !regex_match_group_repeated(text, ti, end_pos, alternatives, min_reps) {
        return None;
    }
    let final_end = regex_match_capturing(text, end_pos, after, 0, must_end, captures)?;
    captures.insert(save, (ti, end_pos));
    Some(final_end)
}

/// Match a group with `?` quantifier (zero or one).
fn regex_match_group_opt(
    text: &[u8],
    ti: usize,
    after: &[u8],
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    alternatives: &[Vec<u8>],
) -> Option<usize> {
    let save = captures.len();
    // Try one
    if let Some(result) =
        regex_try_group_one_alt(text, ti, after, must_end, captures, alternatives, save)
    {
        return Some(result);
    }
    // Try zero
    captures.truncate(save);
    if let Some(final_end) = regex_match_capturing(text, ti, after, 0, must_end, captures) {
        captures.insert(save, (ti, ti));
        return Some(final_end);
    }
    captures.truncate(save);
    None
}

fn regex_try_group_one_alt(
    text: &[u8],
    ti: usize,
    after: &[u8],
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    alternatives: &[Vec<u8>],
    save: usize,
) -> Option<usize> {
    for alt in alternatives {
        captures.truncate(save);
        if let Some(result) =
            regex_try_alt_then_continue(text, ti, alt, after, must_end, captures, save)
        {
            return Some(result);
        }
        captures.truncate(save);
    }
    None
}

fn regex_try_alt_then_continue(
    text: &[u8],
    ti: usize,
    alt: &[u8],
    after: &[u8],
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    save: usize,
) -> Option<usize> {
    let end = regex_try_match_at(text, ti, alt)?;
    let final_end = regex_match_capturing(text, end, after, 0, must_end, captures)?;
    captures.insert(save, (ti, end));
    Some(final_end)
}

/// Match a group exactly once (no quantifier).
fn regex_match_group_exact(
    text: &[u8],
    ti: usize,
    rest: &[u8],
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    alternatives: &[Vec<u8>],
) -> Option<usize> {
    regex_try_group_one_alt(
        text,
        ti,
        rest,
        must_end,
        captures,
        alternatives,
        captures.len(),
    )
}

/// Parse a quantifier after a regex element.
fn parse_quantifier(pat: &[u8], pos: usize) -> (u8, usize) {
    if pos < pat.len() {
        match pat[pos] {
            b'*' => (b'*', pos + 1),
            b'+' => (b'+', pos + 1),
            b'?' => (b'?', pos + 1),
            _ => (0, pos),
        }
    } else {
        (0, pos)
    }
}

/// Match a single regex element (not a group) with optional quantifier.
fn regex_match_elem(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    pi: usize,
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
) -> Option<usize> {
    let (elem_end, matches_fn) = parse_regex_elem(pat, pi);
    let (quant, after_quant) = parse_quantifier(pat, elem_end);

    match quant {
        b'*' | b'+' => regex_match_repeated_elem(
            text,
            ti,
            pat,
            after_quant,
            quant,
            must_end,
            captures,
            &matches_fn,
        ),
        b'?' => {
            regex_match_optional_elem(text, ti, pat, after_quant, must_end, captures, &matches_fn)
        }
        _ => regex_match_single_elem(text, ti, pat, elem_end, must_end, captures, &matches_fn),
    }
}

fn count_regex_matches(text: &[u8], ti: usize, matches_fn: &dyn Fn(u8) -> bool) -> usize {
    let mut count = 0;
    while ti + count < text.len() && matches_fn(text[ti + count]) {
        count += 1;
    }
    count
}

fn regex_match_repeated_elem(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    after_quant: usize,
    quant: u8,
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    matches_fn: &dyn Fn(u8) -> bool,
) -> Option<usize> {
    let min = usize::from(quant == b'+');
    let count = count_regex_matches(text, ti, matches_fn);
    for c in (min..=count).rev() {
        if let Some(end) = regex_match_capturing(text, ti + c, pat, after_quant, must_end, captures)
        {
            return Some(end);
        }
    }
    None
}

fn regex_match_optional_elem(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    after_quant: usize,
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    matches_fn: &dyn Fn(u8) -> bool,
) -> Option<usize> {
    if ti < text.len() && matches_fn(text[ti]) {
        if let Some(end) = regex_match_capturing(text, ti + 1, pat, after_quant, must_end, captures)
        {
            return Some(end);
        }
    }
    regex_match_capturing(text, ti, pat, after_quant, must_end, captures)
}

fn regex_match_single_elem(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    elem_end: usize,
    must_end: bool,
    captures: &mut Vec<(usize, usize)>,
    matches_fn: &dyn Fn(u8) -> bool,
) -> Option<usize> {
    if ti < text.len() && matches_fn(text[ti]) {
        regex_match_capturing(text, ti + 1, pat, elem_end, must_end, captures)
    } else {
        None
    }
}

/// Try to match a simple pattern at a position, returning the end position if matched.
fn regex_try_match_at(text: &[u8], start: usize, pattern: &[u8]) -> Option<usize> {
    regex_try_match_inner(text, start, pattern, 0)
}

/// Inner helper to find end position of a pattern match.
fn regex_try_match_inner(text: &[u8], ti: usize, pat: &[u8], pi: usize) -> Option<usize> {
    if pi >= pat.len() {
        return Some(ti);
    }
    if pat[pi] == b'(' {
        return regex_try_match_group(text, ti, pat, pi);
    }
    let (elem_end, matches_fn) = parse_regex_elem(pat, pi);
    let (quant, after_quant) = parse_quantifier(pat, elem_end);
    regex_try_apply_quant(text, ti, pat, elem_end, after_quant, quant, &matches_fn)
}

/// Handle a group in `regex_try_match_inner`.
fn regex_try_match_group(text: &[u8], ti: usize, pat: &[u8], pi: usize) -> Option<usize> {
    let close = find_matching_paren_bytes(pat, pi + 1)?;
    let inner = &pat[pi + 1..close];
    let rest = &pat[close + 1..];
    let alternatives = split_alternatives_bytes(inner);
    for alt in &alternatives {
        if let Some(end) = regex_try_alt_and_rest(text, ti, alt, rest) {
            return Some(end);
        }
    }
    None
}

fn regex_try_alt_and_rest(text: &[u8], ti: usize, alt: &[u8], rest: &[u8]) -> Option<usize> {
    let after_alt = regex_try_match_inner(text, ti, alt, 0)?;
    regex_try_match_inner(text, after_alt, rest, 0)
}

/// Apply quantifier logic for `regex_try_match_inner`.
fn regex_try_apply_quant(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    elem_end: usize,
    after_quant: usize,
    quant: u8,
    matches_fn: &dyn Fn(u8) -> bool,
) -> Option<usize> {
    match quant {
        b'*' | b'+' => regex_try_match_repeated_elem(text, ti, pat, after_quant, quant, matches_fn),
        b'?' => regex_try_match_optional_elem(text, ti, pat, after_quant, matches_fn),
        _ => regex_try_match_single_elem(text, ti, pat, elem_end, matches_fn),
    }
}

fn regex_try_match_repeated_elem(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    after_quant: usize,
    quant: u8,
    matches_fn: &dyn Fn(u8) -> bool,
) -> Option<usize> {
    let min = usize::from(quant == b'+');
    let count = count_regex_matches(text, ti, matches_fn);
    for c in (min..=count).rev() {
        if let Some(end) = regex_try_match_inner(text, ti + c, pat, after_quant) {
            return Some(end);
        }
    }
    None
}

fn regex_try_match_optional_elem(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    after_quant: usize,
    matches_fn: &dyn Fn(u8) -> bool,
) -> Option<usize> {
    if ti < text.len() && matches_fn(text[ti]) {
        if let Some(end) = regex_try_match_inner(text, ti + 1, pat, after_quant) {
            return Some(end);
        }
    }
    regex_try_match_inner(text, ti, pat, after_quant)
}

fn regex_try_match_single_elem(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    elem_end: usize,
    matches_fn: &dyn Fn(u8) -> bool,
) -> Option<usize> {
    if ti < text.len() && matches_fn(text[ti]) {
        regex_try_match_inner(text, ti + 1, pat, elem_end)
    } else {
        None
    }
}

/// Check if alternatives can be matched repeatedly to fill text[start..end].
fn regex_match_group_repeated(
    text: &[u8],
    start: usize,
    end: usize,
    alternatives: &[Vec<u8>],
    min_reps: usize,
) -> bool {
    if start == end {
        return min_reps == 0;
    }
    if start > end {
        return false;
    }
    for alt in alternatives {
        if regex_group_repetition_matches(text, start, end, alternatives, min_reps, alt) {
            return true;
        }
    }
    false
}

fn regex_group_repetition_matches(
    text: &[u8],
    start: usize,
    end: usize,
    alternatives: &[Vec<u8>],
    min_reps: usize,
    alt: &[u8],
) -> bool {
    let Some(after) = regex_try_match_inner(text, start, alt, 0) else {
        return false;
    };
    if after <= start || after > end {
        return false;
    }
    if after == end && min_reps <= 1 {
        return true;
    }
    regex_match_group_repeated(text, after, end, alternatives, min_reps.saturating_sub(1))
}

/// Find matching `)` for a `(` in a byte pattern, handling nesting.
fn find_matching_paren_bytes(pat: &[u8], start: usize) -> Option<usize> {
    let mut depth = 1;
    let mut i = start;
    while i < pat.len() {
        if pat[i] == b'\\' {
            i += 2;
            continue;
        }
        if pat[i] == b'(' {
            depth += 1;
        } else if pat[i] == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Split a byte pattern by `|` at the top level (not inside nested parens).
fn split_alternatives_bytes(pat: &[u8]) -> Vec<Vec<u8>> {
    let mut alternatives = Vec::new();
    let mut current = Vec::new();
    let mut depth = 0i32;
    let mut i = 0;
    while i < pat.len() {
        if pat[i] == b'\\' && i + 1 < pat.len() {
            current.push(pat[i]);
            current.push(pat[i + 1]);
            i += 2;
            continue;
        }
        split_alt_classify_byte(pat[i], &mut depth, &mut current, &mut alternatives);
        i += 1;
    }
    alternatives.push(current);
    alternatives
}

fn split_alt_classify_byte(
    byte: u8,
    depth: &mut i32,
    current: &mut Vec<u8>,
    alternatives: &mut Vec<Vec<u8>>,
) {
    match byte {
        b'(' => {
            *depth += 1;
            current.push(byte);
        }
        b')' => {
            *depth -= 1;
            current.push(byte);
        }
        b'|' if *depth == 0 => {
            alternatives.push(std::mem::take(current));
        }
        _ => {
            current.push(byte);
        }
    }
}

/// Simple regex-like matching for `=~`.
///
/// Supports: `^prefix`, `suffix$`, `^exact$`, and literal substring match.
/// This avoids pulling in a regex crate for wasm32.
#[allow(dead_code)]
fn simple_regex_match(text: &str, pattern: &str) -> bool {
    let (core, anchored_start, anchored_end) = regex_strip_anchors(pattern);

    if has_regex_metachar(core) {
        return regex_like_match(text, pattern);
    }

    // Pure literal matching with anchoring
    literal_match_range(text, core, anchored_start, anchored_end).is_some()
}

/// A simple regex-like matcher supporting: `.` (any char), `*` (zero or more of previous),
/// `+` (one or more of previous), `?` (zero or one of previous), `^`, `$`,
/// `[abc]` character classes, `(a|b)` alternation, and literal chars.
/// This is intentionally limited but handles common bash `=~` patterns.
#[allow(dead_code)]
fn regex_like_match(text: &str, pattern: &str) -> bool {
    let (core, anchored_start, anchored_end) = regex_strip_anchors(pattern);

    if anchored_start {
        regex_match_at(text, 0, core, anchored_end)
    } else {
        (0..=text.len()).any(|start| regex_match_at(text, start, core, anchored_end))
    }
}

/// Try to match `core` pattern starting at byte position `start` in `text`.
/// If `must_end` is true, the match must consume through end of `text`.
#[allow(dead_code)]
fn regex_match_at(text: &str, start: usize, core: &str, must_end: bool) -> bool {
    let text_bytes = text.as_bytes();
    let core_bytes = core.as_bytes();
    regex_backtrack(text_bytes, start, core_bytes, 0, must_end)
}

/// Recursive backtracking regex matcher.
#[allow(dead_code)]
fn regex_backtrack(text: &[u8], ti: usize, pat: &[u8], pi: usize, must_end: bool) -> bool {
    if pi >= pat.len() {
        return if must_end { ti >= text.len() } else { true };
    }

    let (elem_end, matches_fn) = parse_regex_elem(pat, pi);
    let (quant, after_quant) = parse_quantifier(pat, elem_end);

    match quant {
        b'*' => regex_backtrack_star(text, ti, pat, after_quant, must_end, &matches_fn),
        b'+' => regex_backtrack_plus(text, ti, pat, after_quant, must_end, &matches_fn),
        b'?' => regex_backtrack_optional(text, ti, pat, after_quant, must_end, &matches_fn),
        _ => regex_backtrack_single(text, ti, pat, elem_end, must_end, &matches_fn),
    }
}

fn regex_backtrack_star(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    after_quant: usize,
    must_end: bool,
    matches_fn: &dyn Fn(u8) -> bool,
) -> bool {
    let mut count = 0;
    loop {
        if regex_backtrack(text, ti + count, pat, after_quant, must_end) {
            return true;
        }
        if ti + count < text.len() && matches_fn(text[ti + count]) {
            count += 1;
        } else {
            return false;
        }
    }
}

fn regex_backtrack_plus(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    after_quant: usize,
    must_end: bool,
    matches_fn: &dyn Fn(u8) -> bool,
) -> bool {
    let count = count_regex_matches(text, ti, matches_fn);
    (1..=count).any(|matched| regex_backtrack(text, ti + matched, pat, after_quant, must_end))
}

fn regex_backtrack_optional(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    after_quant: usize,
    must_end: bool,
    matches_fn: &dyn Fn(u8) -> bool,
) -> bool {
    regex_backtrack(text, ti, pat, after_quant, must_end)
        || (ti < text.len()
            && matches_fn(text[ti])
            && regex_backtrack(text, ti + 1, pat, after_quant, must_end))
}

fn regex_backtrack_single(
    text: &[u8],
    ti: usize,
    pat: &[u8],
    elem_end: usize,
    must_end: bool,
    matches_fn: &dyn Fn(u8) -> bool,
) -> bool {
    ti < text.len()
        && matches_fn(text[ti])
        && regex_backtrack(text, ti + 1, pat, elem_end, must_end)
}

/// Parse one regex element at position `pi`, return (`end_pos`, `match_fn`).
/// An element is: `.`, `[class]`, `(alt)`, or a literal byte.
fn parse_regex_elem(pat: &[u8], pi: usize) -> (usize, Box<dyn Fn(u8) -> bool>) {
    match pat[pi] {
        b'.' => (pi + 1, Box::new(|_: u8| true)),
        b'[' => parse_regex_char_class(pat, pi),
        b'\\' if pi + 1 < pat.len() => {
            let escaped = pat[pi + 1];
            (pi + 2, Box::new(move |c: u8| c == escaped))
        }
        ch => (pi + 1, Box::new(move |c: u8| c == ch)),
    }
}

fn parse_regex_char_class(pat: &[u8], pi: usize) -> (usize, Box<dyn Fn(u8) -> bool>) {
    let mut i = pi + 1;
    let negate = i < pat.len() && (pat[i] == b'^' || pat[i] == b'!');
    if negate {
        i += 1;
    }
    let mut chars = Vec::new();
    while i < pat.len() && pat[i] != b']' {
        if i + 2 < pat.len() && pat[i + 1] == b'-' {
            chars.extend(pat[i]..=pat[i + 2]);
            i += 3;
        } else {
            chars.push(pat[i]);
            i += 1;
        }
    }
    let end = if i < pat.len() { i + 1 } else { i };
    (
        end,
        Box::new(move |c: u8| regex_char_class_matches(&chars, negate, c)),
    )
}

fn regex_char_class_matches(chars: &[u8], negate: bool, c: u8) -> bool {
    let found = chars.contains(&c);
    if negate {
        !found
    } else {
        found
    }
}

/// Match a glob character class `[...]` at position `pi` (just past the `[`).
/// Returns `(new_pi, matched)` where `new_pi` is past the `]`.
fn glob_match_char_class(pattern: &[u8], mut pi: usize, ch: u8) -> (usize, bool) {
    let negate = pi < pattern.len() && (pattern[pi] == b'!' || pattern[pi] == b'^');
    if negate {
        pi += 1;
    }
    let mut matched = false;
    let mut first = true;
    while pi < pattern.len() && (first || pattern[pi] != b']') {
        first = false;
        let (next_pi, item_matched) = glob_match_char_class_item(pattern, pi, ch);
        matched |= item_matched;
        pi = next_pi;
    }
    if pi < pattern.len() && pattern[pi] == b']' {
        pi += 1;
    }
    (pi, matched != negate)
}

fn glob_match_char_class_item(pattern: &[u8], pi: usize, ch: u8) -> (usize, bool) {
    if pi + 2 < pattern.len() && pattern[pi + 1] == b'-' {
        let lo = pattern[pi];
        let hi = pattern[pi + 2];
        return (pi + 3, ch >= lo && ch <= hi);
    }
    (pi + 1, pattern[pi] == ch)
}

enum GlobPatternStep {
    Consume(usize),
    Star,
    Class(usize, bool),
    Mismatch,
}

fn glob_step(pattern: &[u8], pi: usize, ch: u8) -> GlobPatternStep {
    if pi >= pattern.len() {
        return GlobPatternStep::Mismatch;
    }

    match pattern[pi] {
        b'?' => GlobPatternStep::Consume(pi + 1),
        b'*' => GlobPatternStep::Star,
        b'[' => {
            let (new_pi, matched) = glob_match_char_class(pattern, pi + 1, ch);
            GlobPatternStep::Class(new_pi, matched)
        }
        literal if literal == ch => GlobPatternStep::Consume(pi + 1),
        _ => GlobPatternStep::Mismatch,
    }
}

fn glob_backtrack(pi: &mut usize, ni: &mut usize, star_pi: usize, star_ni: &mut usize) -> bool {
    if star_pi == usize::MAX {
        return false;
    }

    *pi = star_pi + 1;
    *star_ni += 1;
    *ni = *star_ni;
    true
}

/// Core glob pattern matching (byte-level).
///
/// Supports `*` (any sequence), `?` (one char), and `[abc]` (character class).
pub(crate) fn glob_match_inner(pattern: &[u8], name: &[u8]) -> bool {
    let mut pi = 0;
    let mut ni = 0;
    let mut star_pi = usize::MAX;
    let mut star_ni = usize::MAX;

    while ni < name.len() {
        match glob_step(pattern, pi, name[ni]) {
            GlobPatternStep::Star => {
                star_pi = pi;
                star_ni = ni;
                pi += 1;
            }
            GlobPatternStep::Consume(new_pi) | GlobPatternStep::Class(new_pi, true) => {
                pi = new_pi;
                ni += 1;
            }
            GlobPatternStep::Class(_, false) | GlobPatternStep::Mismatch => {
                if !glob_backtrack(&mut pi, &mut ni, star_pi, &mut star_ni) {
                    return false;
                }
            }
        }
    }

    // Consume trailing stars
    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }

    pi == pattern.len()
}

/// Extended glob matching with dotglob and extglob support.
pub(crate) fn glob_match_ext(pattern: &str, name: &str, dotglob: bool, extglob: bool) -> bool {
    // Don't match hidden files unless dotglob is enabled or pattern starts with '.'
    if name.starts_with('.') && !pattern.starts_with('.') && !dotglob {
        return false;
    }
    if extglob && has_extglob_pattern(pattern) {
        return extglob_match(pattern, name);
    }
    glob_match_inner(pattern.as_bytes(), name.as_bytes())
}

/// Check if a pattern contains extglob operators: `?(`, `*(`, `+(`, `@(`, `!(`.
pub(crate) fn has_extglob_pattern(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    for i in 0..bytes.len().saturating_sub(1) {
        if bytes[i + 1] == b'(' && matches!(bytes[i], b'?' | b'*' | b'+' | b'@' | b'!') {
            return true;
        }
    }
    false
}

/// Match a name against an extglob pattern.
///
/// Supports: `?(pat|pat)`, `*(pat|pat)`, `+(pat|pat)`, `@(pat|pat)`, `!(pat|pat)`.
/// Non-extglob portions are handled by regular glob matching.
pub fn extglob_match(pattern: &str, name: &str) -> bool {
    extglob_match_recursive(pattern.as_bytes(), name.as_bytes())
}

fn extglob_match_recursive(pattern: &[u8], name: &[u8]) -> bool {
    // Find the first extglob operator
    let Some((pi, op, close)) = find_extglob_operator(pattern) else {
        return glob_match_inner(pattern, name);
    };

    let open = pi + 2;
    let alternatives = split_alternatives(&pattern[open..close]);
    let prefix = &pattern[..pi];
    let suffix = &pattern[close + 1..];

    match op {
        b'@' | b'?' => extglob_match_at_or_opt(op, prefix, &alternatives, suffix, name),
        b'*' => extglob_star(prefix, &alternatives, suffix, name, 0),
        b'+' => extglob_plus(prefix, &alternatives, suffix, name, 0),
        b'!' => extglob_match_negate(prefix, &alternatives, suffix, name),
        _ => unreachable!(),
    }
}

/// Find the first extglob operator in a pattern, returning (position, operator, `close_paren`).
fn find_extglob_operator(pattern: &[u8]) -> Option<(usize, u8, usize)> {
    let mut pi = 0;
    while pi < pattern.len() {
        if pi + 1 < pattern.len()
            && pattern[pi + 1] == b'('
            && matches!(pattern[pi], b'?' | b'*' | b'+' | b'@' | b'!')
        {
            if let Some(close) = find_matching_paren(pattern, pi + 2) {
                return Some((pi, pattern[pi], close));
            }
        }
        pi += 1;
    }
    None
}

/// Build a combined pattern from prefix + alt + suffix.
fn build_combined(prefix: &[u8], mid: &[u8], suffix: &[u8]) -> Vec<u8> {
    let mut combined = Vec::with_capacity(prefix.len() + mid.len() + suffix.len());
    combined.extend_from_slice(prefix);
    combined.extend_from_slice(mid);
    combined.extend_from_slice(suffix);
    combined
}

/// Handle `@(...)` (exactly one) and `?(...)` (zero or one) extglob patterns.
fn extglob_match_at_or_opt(
    op: u8,
    prefix: &[u8],
    alternatives: &[Vec<u8>],
    suffix: &[u8],
    name: &[u8],
) -> bool {
    // For `?`, try zero first
    if op == b'?' && extglob_match_recursive(&build_combined(prefix, &[], suffix), name) {
        return true;
    }
    // Try each alternative exactly once
    for alt in alternatives {
        if extglob_match_recursive(&build_combined(prefix, alt, suffix), name) {
            return true;
        }
    }
    false
}

/// Handle `!(...)` extglob pattern: matches if no alternative matches.
fn extglob_match_negate(
    prefix: &[u8],
    alternatives: &[Vec<u8>],
    suffix: &[u8],
    name: &[u8],
) -> bool {
    for alt in alternatives {
        if extglob_match_recursive(&build_combined(prefix, alt, suffix), name) {
            return false;
        }
    }
    let wildcard = build_combined(prefix, b"*", suffix);
    glob_match_inner(&wildcard, name)
}

/// Try zero or more repetitions of alternatives for `*(...)`.
fn extglob_star(
    prefix: &[u8],
    alternatives: &[Vec<u8>],
    suffix: &[u8],
    name: &[u8],
    depth: u32,
) -> bool {
    if depth > 20 {
        return false;
    }
    // Try zero repetitions
    if extglob_match_recursive(&build_combined(prefix, &[], suffix), name) {
        return true;
    }
    // Try one repetition followed by zero or more
    extglob_try_extend(prefix, alternatives, suffix, name, depth)
}

fn extglob_try_extend(
    prefix: &[u8],
    alternatives: &[Vec<u8>],
    suffix: &[u8],
    name: &[u8],
    depth: u32,
) -> bool {
    let prefix_len = prefix.len();
    for alt in alternatives {
        let new_prefix = build_combined(prefix, alt, &[]);
        if new_prefix.len() > prefix_len
            && extglob_star(&new_prefix, alternatives, suffix, name, depth + 1)
        {
            return true;
        }
    }
    false
}

/// Try one or more repetitions of alternatives for `+(...)`.
fn extglob_plus(
    prefix: &[u8],
    alternatives: &[Vec<u8>],
    suffix: &[u8],
    name: &[u8],
    depth: u32,
) -> bool {
    if depth > 20 {
        return false;
    }
    for alt in alternatives {
        let new_prefix = build_combined(prefix, alt, &[]);
        if extglob_star(&new_prefix, alternatives, suffix, name, depth + 1) {
            return true;
        }
    }
    false
}

/// Find the matching `)` for a `(` at position `open` (character after `(`).
fn find_matching_paren(pattern: &[u8], open: usize) -> Option<usize> {
    let mut depth: u32 = 1;
    let mut i = open;
    while i < pattern.len() {
        if pattern[i] == b'(' {
            depth += 1;
        } else if pattern[i] == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Split alternatives by `|` at the top level (not inside nested parens).
fn split_alternatives(pat: &[u8]) -> Vec<Vec<u8>> {
    let mut result = Vec::new();
    let mut current = Vec::new();
    let mut depth: u32 = 0;
    for &b in pat {
        if b == b'(' {
            depth += 1;
            current.push(b);
        } else if b == b')' {
            depth -= 1;
            current.push(b);
        } else if b == b'|' && depth == 0 {
            result.push(std::mem::take(&mut current));
        } else {
            current.push(b);
        }
    }
    result.push(current);
    result
}
