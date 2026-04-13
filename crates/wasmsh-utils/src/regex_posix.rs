//! Thin wrapper over the `posix-regex` crate, shared by `sed` and `grep`.
//!
//! `posix-regex` is an ASCII-only POSIX BRE/ERE implementation with zero
//! runtime dependencies and `no_std` support, which keeps the wasm footprint
//! small and the license story clean.  We wrap it here so the callers
//! (`text_ops.rs` for now, potentially `search_ops.rs` later) don't need
//! to deal with its byte-slice API or builder pattern directly.
//!
//! ## API contract
//!
//! - Patterns are `&str`.  Input text is `&str`.  Byte offsets returned by
//!   matchers are byte offsets into the input string.  Callers that need
//!   character-level positions must translate themselves.
//! - Default dialect is POSIX **BRE** (matches GNU `sed` and `grep`
//!   without `-E`).  Use `Regex::compile_ere` for POSIX **ERE**
//!   (matches `awk`, `egrep`, `sed -E`).
//! - Patterns that fail to compile are surfaced as `Err(String)` so the
//!   caller can emit a human-readable diagnostic and fall back to literal
//!   matching if desired.
//! - Substitution replacement strings follow the `sed`/`grep` convention:
//!   `&` expands to the whole match, `\1`..`\9` expand to the Nth capture
//!   group, `\\` is a literal backslash.

#![allow(clippy::module_name_repetitions)]
// Some of the helpers below (`find`, `replace_nth`) are exposed for
// call sites that will be wired up incrementally — notably sed's `s//N`
// numeric flag.  They are exercised by unit tests in this module.
#![allow(dead_code)]

use posix_regex::compile::PosixRegexBuilder;
use posix_regex::PosixRegex;

/// A compiled POSIX regex.  Wraps `posix_regex::PosixRegex` with a
/// slimmer, `str`-oriented API.
///
/// `empty` is `true` when the source pattern was the empty string.
/// POSIX defines an empty regex as matching the null string at every
/// position in the subject — in practice this means `grep ""` matches
/// every line.  `posix-regex` reports zero matches for an empty
/// pattern, so we short-circuit the empty-pattern case in the wrapper
/// and behave as "always matches, with a zero-width match at position
/// zero."
pub(crate) struct Regex {
    inner: PosixRegex<'static>,
    empty: bool,
}

impl Regex {
    /// Compile a pattern as POSIX BRE (the default dialect for `sed` and
    /// `grep` without `-E`).
    ///
    /// # Errors
    ///
    /// Returns a human-readable error string if the pattern cannot be
    /// parsed.
    pub(crate) fn compile_bre(pattern: &str) -> Result<Self, String> {
        Self::compile(pattern, false)
    }

    /// Compile a pattern as POSIX ERE (the default for `awk` and for
    /// `grep -E` / `sed -E`).
    ///
    /// # Errors
    ///
    /// Returns a human-readable error string if the pattern cannot be
    /// parsed.
    pub(crate) fn compile_ere(pattern: &str) -> Result<Self, String> {
        Self::compile(pattern, true)
    }

    fn compile(pattern: &str, extended: bool) -> Result<Self, String> {
        let empty = pattern.is_empty();
        let inner = PosixRegexBuilder::new(pattern.as_bytes())
            .with_default_classes()
            .extended(extended)
            .compile()
            .map_err(|e| format!("{e:?}"))?;
        Ok(Self { inner, empty })
    }

    /// Return `true` if the regex matches anywhere in `subject`.
    pub(crate) fn is_match(&self, subject: &str) -> bool {
        if self.empty {
            // POSIX: the empty regex matches at every position.
            // `grep ""` is a common way to count lines.
            return true;
        }
        let _ = subject;
        !self.inner.matches(subject.as_bytes(), Some(1)).is_empty()
    }

    /// Find the first match in `subject`, returning the byte offset range
    /// of the full match (group 0).
    pub(crate) fn find(&self, subject: &str) -> Option<(usize, usize)> {
        if self.empty {
            return Some((0, 0));
        }
        let matches = self.inner.matches(subject.as_bytes(), Some(1));
        let first = matches.into_iter().next()?;
        first.first().copied().flatten()
    }

    /// Return all non-overlapping match ranges (byte offsets) in `subject`.
    ///
    /// Each element is the `(start, end)` pair for group 0 of one match.
    /// Zero-width matches are skipped forward by one byte to avoid
    /// infinite loops.
    pub(crate) fn find_iter_offsets(&self, subject: &str) -> Vec<(usize, usize)> {
        if self.empty {
            return vec![(0, 0)];
        }
        let mut out = Vec::new();
        let mut cursor = 0usize;
        loop {
            if cursor > subject.len() {
                break;
            }
            let remaining = &subject.as_bytes()[cursor..];
            let matches = self.inner.matches(remaining, Some(1));
            let Some(caps) = matches.into_iter().next() else {
                break;
            };
            let Some(Some((rel_start, rel_end))) = caps.first().copied() else {
                break;
            };
            out.push((cursor + rel_start, cursor + rel_end));
            cursor += if rel_end == rel_start {
                rel_end + 1
            } else {
                rel_end
            };
        }
        out
    }

    /// Replace the first match of this regex in `subject` with
    /// `replacement`.  Returns the original string if no match.
    ///
    /// `replacement` supports `sed`-style expansions:
    ///
    /// - `&` or `\0` — full match
    /// - `\1`..`\9` — captured subgroup N (empty if absent)
    /// - `\\` — literal backslash
    /// - `\&` — literal `&`
    pub(crate) fn replace(&self, subject: &str, replacement: &str) -> String {
        self.replace_impl(subject, replacement, ReplaceMode::First)
    }

    /// Replace all non-overlapping matches of this regex in `subject`
    /// with `replacement`.  Empty-match handling follows sed's `g`
    /// semantics: a zero-width match advances the cursor by one byte.
    pub(crate) fn replace_all(&self, subject: &str, replacement: &str) -> String {
        self.replace_impl(subject, replacement, ReplaceMode::All)
    }

    /// Replace only the `nth` (1-based) match of this regex in `subject`.
    /// If `nth` is larger than the number of matches, returns `subject`
    /// unchanged.  This implements sed's `s///N` flag.
    pub(crate) fn replace_nth(&self, subject: &str, replacement: &str, nth: usize) -> String {
        if nth == 0 {
            return subject.to_string();
        }
        self.replace_impl(subject, replacement, ReplaceMode::Nth(nth))
    }

    /// Internal replace loop.
    ///
    /// We iterate one match at a time (calling `matches(remaining, 1)`)
    /// instead of `matches(all, None)` because `posix-regex` may return
    /// fewer results than expected for simple patterns when asked for
    /// all matches at once.  The find-then-advance loop is safe for all
    /// pattern types and handles zero-width matches correctly.
    fn replace_impl(&self, subject: &str, replacement: &str, mode: ReplaceMode) -> String {
        if self.empty {
            return subject.to_string();
        }

        let mut out = String::with_capacity(subject.len());
        let mut cursor = 0usize;
        let mut applied = 0usize;
        let mut match_count = 0usize;

        while cursor <= subject.len() {
            let Some((caps, rel_start, rel_end)) = self.next_match(subject, cursor) else {
                break;
            };

            match_count += 1;
            let should_apply = match mode {
                ReplaceMode::First => applied == 0,
                ReplaceMode::All => true,
                ReplaceMode::Nth(n) => match_count == n,
            };
            let advance = advance_past_match(rel_start, rel_end);

            if !should_apply {
                out.push_str(&subject[cursor..cursor + advance]);
                cursor += advance;
                continue;
            }

            out.push_str(&subject[cursor..cursor + rel_start]);
            let remaining_str = &subject[cursor..];
            append_replacement(&mut out, replacement, remaining_str, &caps);
            cursor += advance;
            applied += 1;
            if matches!(mode, ReplaceMode::First | ReplaceMode::Nth(_)) {
                break;
            }
        }

        // Append the tail.
        if cursor <= subject.len() {
            out.push_str(&subject[cursor..]);
        }
        out
    }

    /// Find the next match starting at `cursor` and return captures plus
    /// the relative start/end of the overall match.
    fn next_match(
        &self,
        subject: &str,
        cursor: usize,
    ) -> Option<(Vec<Option<(usize, usize)>>, usize, usize)> {
        let remaining = &subject.as_bytes()[cursor..];
        let matches = self.inner.matches(remaining, Some(1));
        let caps = matches.into_iter().next()?;
        let (rel_start, rel_end) = caps.first().copied().flatten()?;
        Some((caps.to_vec(), rel_start, rel_end))
    }
}

#[derive(Clone, Copy)]
enum ReplaceMode {
    First,
    All,
    Nth(usize),
}

/// Compute how far to advance the cursor past a match.  For zero-length
/// matches, advance by one extra byte to avoid infinite loops.
fn advance_past_match(rel_start: usize, rel_end: usize) -> usize {
    if rel_end == rel_start {
        rel_end + 1
    } else {
        rel_end
    }
}

/// Push the captured group slice (if present) from `caps[idx]` onto `out`.
fn push_capture(out: &mut String, subject: &str, caps: &[Option<(usize, usize)>], idx: usize) {
    if let Some(Some((s, e))) = caps.get(idx).copied() {
        out.push_str(&subject[s..e]);
    }
}

fn append_replacement_backslash(
    out: &mut String,
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    subject: &str,
    caps: &[Option<(usize, usize)>],
) {
    let Some(&next) = chars.peek() else {
        out.push('\\');
        return;
    };
    if let Some(digit) = next.to_digit(10) {
        chars.next();
        push_capture(out, subject, caps, digit as usize);
        return;
    }
    chars.next();
    match next {
        '\\' => out.push('\\'),
        '&' => out.push('&'),
        'n' => out.push('\n'),
        't' => out.push('\t'),
        'r' => out.push('\r'),
        other => out.push(other),
    }
}

fn append_replacement(
    out: &mut String,
    template: &str,
    subject: &str,
    caps: &[Option<(usize, usize)>],
) {
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => append_replacement_backslash(out, &mut chars, subject, caps),
            '&' => push_capture(out, subject, caps, 0),
            other => out.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_match() {
        let re = Regex::compile_bre("hello").unwrap();
        assert!(re.is_match("say hello world"));
        assert!(!re.is_match("goodbye"));
    }

    // POSIX: the empty regex matches every line, which makes
    // `grep -c "" file` a common idiom for "count lines in file".
    // `posix-regex` returns zero matches for an empty pattern, so
    // the wrapper must short-circuit this case.
    #[test]
    fn empty_pattern_matches_every_line() {
        let re = Regex::compile_bre("").unwrap();
        assert!(re.is_match(""));
        assert!(re.is_match("anything"));
        assert!(re.is_match("line with content"));
    }

    // Regression: the agent harness uncovered that wasmsh's sed could
    // not match `\[section\]`.  POSIX BRE treats `[` as a literal when
    // escaped, so this must succeed.
    #[test]
    fn bre_escaped_brackets() {
        let re = Regex::compile_bre(r"\[section\]").unwrap();
        assert!(re.is_match("[section]"));
        assert!(re.is_match("prefix [section] suffix"));
        assert!(!re.is_match("section"));
    }

    // POSIX BRE: `[[]section]` is technically ambiguous — `[[` looks
    // like the start of a POSIX class expression (`[[:digit:]]` etc.),
    // so `posix-regex` rejects it.  The documented workaround in the
    // agent harness failure was wrong; the canonical form is either
    // `\[section\]` (tested above) or `[\[]section]`.  Document the
    // compile failure so regressions are caught if the upstream
    // behaviour changes.
    #[test]
    fn bre_class_start_bracket_is_ambiguous() {
        assert!(Regex::compile_bre("[[]section]").is_err());
    }

    #[test]
    fn char_class_range() {
        let re = Regex::compile_bre("[a-z][a-z]*").unwrap();
        assert!(re.is_match("XX abc YY"));
        let (s, e) = re.find("XX abc YY").unwrap();
        assert_eq!(&"XX abc YY"[s..e], "abc");
    }

    #[test]
    fn anchors_bre() {
        assert!(Regex::compile_bre("^foo").unwrap().is_match("foobar"));
        assert!(!Regex::compile_bre("^foo").unwrap().is_match("xfoobar"));
        assert!(Regex::compile_bre("foo$").unwrap().is_match("barfoo"));
        assert!(!Regex::compile_bre("foo$").unwrap().is_match("foobar"));
    }

    #[test]
    fn ere_alternation() {
        let re = Regex::compile_ere("foo|bar").unwrap();
        assert!(re.is_match("hello foo"));
        assert!(re.is_match("hello bar"));
        assert!(!re.is_match("hello baz"));
    }

    // POSIX BRE uses `\+`/`\?` as quantifiers; ERE uses bare `+`/`?`.
    #[test]
    fn bre_backslash_quantifiers() {
        let re = Regex::compile_bre(r"a\+").unwrap();
        assert!(re.is_match("baaa"));
        assert!(!re.is_match("b"));
    }

    #[test]
    fn ere_bare_quantifiers() {
        let re = Regex::compile_ere("a+").unwrap();
        assert!(re.is_match("baaa"));
        assert!(!re.is_match("b"));
    }

    // sed substitution semantics.
    #[test]
    fn replace_first_only() {
        let re = Regex::compile_bre("[0-9][0-9]*").unwrap();
        assert_eq!(re.replace("a 1 b 2", "X"), "a X b 2");
    }

    #[test]
    fn replace_all_matches() {
        let re = Regex::compile_bre("[0-9][0-9]*").unwrap();
        assert_eq!(re.replace_all("a 1 b 22", "X"), "a X b X");
    }

    #[test]
    fn replace_with_ampersand_backref() {
        let re = Regex::compile_bre("[0-9][0-9]*").unwrap();
        assert_eq!(re.replace_all("a 1 b 22", "[&]"), "a [1] b [22]");
    }

    // Regression: the exact sed substitution form the agent harness
    // exercised.  Must replace the bracketed literal with a new string.
    #[test]
    fn replace_literal_section_brackets() {
        let re = Regex::compile_bre(r"\[section\]").unwrap();
        assert_eq!(
            re.replace("[section]\nkey=value", "[SECRET]"),
            "[SECRET]\nkey=value"
        );
    }

    #[test]
    fn replace_nth_second_match() {
        let re = Regex::compile_bre("[0-9][0-9]*").unwrap();
        assert_eq!(re.replace_nth("a 1 b 2 c 3", "X", 2), "a 1 b X c 3");
        assert_eq!(re.replace_nth("a 1 b 2 c 3", "X", 3), "a 1 b 2 c X");
        // Out-of-range nth returns original.
        assert_eq!(re.replace_nth("a 1 b 2", "X", 5), "a 1 b 2");
    }

    #[test]
    fn find_iter_offsets_basic() {
        let re = Regex::compile_bre("[0-9][0-9]*").unwrap();
        let offsets = re.find_iter_offsets("a1 b22 c333");
        let matched: Vec<&str> = offsets.iter().map(|&(s, e)| &"a1 b22 c333"[s..e]).collect();
        assert_eq!(matched, vec!["1", "22", "333"]);
    }

    // Invalid patterns must surface as an Err so the caller can
    // decide whether to fall back to literal matching.  `posix-regex`
    // is lenient about unbalanced ERE groups (it accepts `(foo`
    // silently), so we test forms that it does reject.
    #[test]
    fn invalid_pattern_returns_error() {
        assert!(Regex::compile_bre("[").is_err());
        assert!(Regex::compile_bre("[abc").is_err());
        assert!(Regex::compile_bre(r"\").is_err());
        assert!(Regex::compile_bre("[[:nosuch:]]").is_err());
    }
}
