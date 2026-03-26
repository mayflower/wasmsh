//! Word parser: splits raw word text (from the lexer) into structured `WordPart` nodes.
//!
//! The lexer produces word tokens whose text includes quotes and dollar signs.
//! This module splits that text into `Literal`, `SingleQuoted`, `DoubleQuoted`,
//! `Parameter`, `CommandSubstitution`, and `Arithmetic` parts.

use wasmsh_ast::WordPart;

/// Parse the raw text of a word token into structured parts.
pub(crate) fn parse_word_parts(text: &str) -> Vec<WordPart> {
    let bytes = text.as_bytes();
    let mut pos = 0;
    let mut parts = Vec::new();
    let mut lit = String::new();

    while pos < bytes.len() {
        parse_word_part(text, bytes, &mut pos, &mut lit, &mut parts);
    }

    flush(&mut lit, &mut parts);
    parts
}

fn parse_word_part(
    text: &str,
    bytes: &[u8],
    pos: &mut usize,
    lit: &mut String,
    parts: &mut Vec<WordPart>,
) {
    match bytes[*pos] {
        b'\'' => parse_single_quoted_part(text, bytes, pos, lit, parts),
        b'"' => parse_double_quoted_part(text, pos, lit, parts),
        b'\\' => parse_escaped_literal(bytes, pos, lit),
        b'$' => parse_dollar_part(text, bytes, pos, lit, parts),
        _ => {
            lit.push(bytes[*pos] as char);
            *pos += 1;
        }
    }
}

fn parse_single_quoted_part(
    text: &str,
    bytes: &[u8],
    pos: &mut usize,
    lit: &mut String,
    parts: &mut Vec<WordPart>,
) {
    flush(lit, parts);
    *pos += 1;
    let start = *pos;
    while *pos < bytes.len() && bytes[*pos] != b'\'' {
        *pos += 1;
    }
    parts.push(WordPart::SingleQuoted(text[start..*pos].into()));
    if *pos < bytes.len() {
        *pos += 1;
    }
}

fn parse_double_quoted_part(
    text: &str,
    pos: &mut usize,
    lit: &mut String,
    parts: &mut Vec<WordPart>,
) {
    flush(lit, parts);
    *pos += 1;
    parts.push(WordPart::DoubleQuoted(parse_double_quoted(text, pos)));
}

fn parse_escaped_literal(bytes: &[u8], pos: &mut usize, lit: &mut String) {
    *pos += 1;
    if *pos < bytes.len() {
        lit.push(bytes[*pos] as char);
        *pos += 1;
    }
}

fn parse_dollar_part(
    text: &str,
    bytes: &[u8],
    pos: &mut usize,
    lit: &mut String,
    parts: &mut Vec<WordPart>,
) {
    if pos_peek(bytes, *pos + 1) == Some(b'\'') {
        flush(lit, parts);
        *pos += 2;
        parts.push(WordPart::Literal(parse_ansi_c_quoted(text, pos).into()));
        return;
    }
    if pos_peek(bytes, *pos + 1) == Some(b'"') {
        flush(lit, parts);
        *pos += 2;
        parts.push(WordPart::DoubleQuoted(parse_double_quoted(text, pos)));
        return;
    }

    flush(lit, parts);
    *pos += 1;
    if let Some(part) = parse_dollar(text, pos) {
        parts.push(part);
    } else {
        lit.push('$');
    }
}

/// Parse the interior of a double-quoted string, stopping at closing `"`.
fn parse_double_quoted(text: &str, pos: &mut usize) -> Vec<WordPart> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut lit = String::new();

    while *pos < bytes.len() {
        match bytes[*pos] {
            b'"' => {
                *pos += 1; // closing "
                break;
            }
            b'\\' => {
                *pos += 1;
                if *pos < bytes.len() {
                    let c = bytes[*pos];
                    // In double quotes, backslash only escapes $, `, ", \, and newline
                    if matches!(c, b'$' | b'`' | b'"' | b'\\' | b'\n') {
                        lit.push(c as char);
                    } else {
                        lit.push('\\');
                        lit.push(c as char);
                    }
                    *pos += 1;
                }
            }
            b'$' => {
                flush(&mut lit, &mut parts);
                *pos += 1;
                if let Some(part) = parse_dollar(text, pos) {
                    parts.push(part);
                } else {
                    lit.push('$');
                }
            }
            _ => {
                lit.push(bytes[*pos] as char);
                *pos += 1;
            }
        }
    }

    flush(&mut lit, &mut parts);
    parts
}

/// Parse a dollar-expansion starting after the `$`. Returns `None` for a lone `$`.
fn parse_dollar(text: &str, pos: &mut usize) -> Option<WordPart> {
    let bytes = text.as_bytes();
    if *pos >= bytes.len() {
        return None;
    }

    match bytes[*pos] {
        b'(' => parse_dollar_paren(text, bytes, pos),
        b'{' => parse_dollar_brace(text, pos),
        b if b.is_ascii_alphabetic() || b == b'_' => parse_named_parameter(text, bytes, pos),
        b if is_special_param(b) => parse_special_parameter(text, pos),
        _ => None,
    }
}

fn parse_dollar_paren(text: &str, bytes: &[u8], pos: &mut usize) -> Option<WordPart> {
    *pos += 1;
    if *pos < bytes.len() && bytes[*pos] == b'(' {
        parse_dollar_arith(text, pos)
    } else {
        parse_dollar_cmd_subst(text, pos)
    }
}

fn parse_named_parameter(text: &str, bytes: &[u8], pos: &mut usize) -> Option<WordPart> {
    let start = *pos;
    while *pos < bytes.len() && (bytes[*pos].is_ascii_alphanumeric() || bytes[*pos] == b'_') {
        *pos += 1;
    }
    Some(WordPart::Parameter(text[start..*pos].into()))
}

fn parse_special_parameter(text: &str, pos: &mut usize) -> Option<WordPart> {
    let start = *pos;
    *pos += 1;
    Some(WordPart::Parameter(text[start..*pos].into()))
}

/// Check if a byte is a special parameter character (`?`, `!`, `#`, `$`, `@`, `*`, `-`, digit).
fn is_special_param(b: u8) -> bool {
    matches!(b, b'?' | b'!' | b'#' | b'$' | b'@' | b'*' | b'-') || b.is_ascii_digit()
}

/// Parse `$(( ... ))` arithmetic expansion (pos is just past the second `(`).
fn parse_dollar_arith(text: &str, pos: &mut usize) -> Option<WordPart> {
    let bytes = text.as_bytes();
    *pos += 1; // skip second '('
    let start = *pos;
    let mut depth: u32 = 1;
    while *pos < bytes.len() && depth > 0 {
        if bytes[*pos] == b'(' && pos_peek(bytes, *pos + 1) == Some(b'(') {
            depth += 1;
            *pos += 2;
        } else if bytes[*pos] == b')' && pos_peek(bytes, *pos + 1) == Some(b')') {
            depth -= 1;
            if depth == 0 {
                let expr = &text[start..*pos];
                *pos += 2; // skip ))
                return Some(WordPart::Arithmetic(expr.into()));
            }
            *pos += 2;
        } else {
            *pos += 1;
        }
    }
    // Fallback: unterminated (shouldn't happen -- lexer validates)
    Some(WordPart::Arithmetic(text[start..*pos].into()))
}

/// Parse `$( ... )` command substitution (pos is at first byte inside parens).
fn parse_dollar_cmd_subst(text: &str, pos: &mut usize) -> Option<WordPart> {
    let bytes = text.as_bytes();
    let start = *pos;
    let mut depth: u32 = 1;
    while *pos < bytes.len() && depth > 0 {
        match bytes[*pos] {
            b'(' => {
                depth += 1;
                *pos += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    let inner = &text[start..*pos];
                    *pos += 1; // skip )
                    return Some(WordPart::CommandSubstitution(inner.into()));
                }
                *pos += 1;
            }
            b'\'' => skip_single_quoted(bytes, pos),
            b'"' => skip_double_quoted(bytes, pos),
            _ => *pos += 1,
        }
    }
    Some(WordPart::CommandSubstitution(text[start..*pos].into()))
}

/// Parse `${...}` parameter expansion (pos is at `{`).
fn parse_dollar_brace(text: &str, pos: &mut usize) -> Option<WordPart> {
    let bytes = text.as_bytes();
    *pos += 1; // skip '{'
    let start = *pos;
    let mut depth: u32 = 1;
    while *pos < bytes.len() && depth > 0 {
        match bytes[*pos] {
            b'{' => {
                depth += 1;
                *pos += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    let name = &text[start..*pos];
                    *pos += 1;
                    return Some(WordPart::Parameter(name.into()));
                }
                *pos += 1;
            }
            b'\\' => {
                *pos += 1;
                if *pos < bytes.len() {
                    *pos += 1;
                }
            }
            _ => *pos += 1,
        }
    }
    Some(WordPart::Parameter(text[start..*pos].into()))
}

/// Skip past a single-quoted string (pos is at the opening `'`).
fn skip_single_quoted(bytes: &[u8], pos: &mut usize) {
    *pos += 1;
    while *pos < bytes.len() && bytes[*pos] != b'\'' {
        *pos += 1;
    }
    if *pos < bytes.len() {
        *pos += 1;
    }
}

/// Skip past a double-quoted string (pos is at the opening `"`).
fn skip_double_quoted(bytes: &[u8], pos: &mut usize) {
    *pos += 1;
    while *pos < bytes.len() && bytes[*pos] != b'"' {
        if bytes[*pos] == b'\\' {
            *pos += 1;
            if *pos >= bytes.len() {
                break;
            }
        }
        *pos += 1;
    }
    if *pos < bytes.len() {
        *pos += 1;
    }
}

/// Parse the interior of a `$'...'` ANSI-C quoted string, stopping at closing `'`.
/// Handles `\n`, `\t`, `\\`, `\0`, `\a`, `\b`, `\e`, `\f`, `\r`, `\v`, `\xNN`, `\0NNN`.
fn parse_ansi_c_quoted(text: &str, pos: &mut usize) -> String {
    let bytes = text.as_bytes();
    let mut result = String::new();

    while *pos < bytes.len() {
        if bytes[*pos] == b'\'' {
            *pos += 1;
            break;
        }
        if bytes[*pos] == b'\\' {
            *pos += 1;
            if *pos < bytes.len() {
                ansi_c_escape(bytes, pos, &mut result);
            }
            continue;
        }
        result.push(bytes[*pos] as char);
        *pos += 1;
    }

    result
}

/// Process a single ANSI-C escape sequence (pos is at the character after `\`).
fn ansi_c_escape(bytes: &[u8], pos: &mut usize, result: &mut String) {
    match bytes[*pos] {
        b'n' => {
            result.push('\n');
            *pos += 1;
        }
        b't' => {
            result.push('\t');
            *pos += 1;
        }
        b'r' => {
            result.push('\r');
            *pos += 1;
        }
        b'a' => {
            result.push('\x07');
            *pos += 1;
        }
        b'b' => {
            result.push('\x08');
            *pos += 1;
        }
        b'e' | b'E' => {
            result.push('\x1b');
            *pos += 1;
        }
        b'f' => {
            result.push('\x0c');
            *pos += 1;
        }
        b'v' => {
            result.push('\x0b');
            *pos += 1;
        }
        b'\\' => {
            result.push('\\');
            *pos += 1;
        }
        b'\'' => {
            result.push('\'');
            *pos += 1;
        }
        b'"' => {
            result.push('"');
            *pos += 1;
        }
        b'0' => {
            *pos += 1;
            result.push(parse_octal_digits(bytes, pos, 3) as char);
        }
        b'x' => {
            *pos += 1;
            result.push(parse_hex_digits(bytes, pos, 2) as char);
        }
        other => {
            result.push('\\');
            result.push(other as char);
            *pos += 1;
        }
    }
}

/// Parse up to `max_digits` octal digits, advancing `pos`. Returns the accumulated value.
fn parse_octal_digits(bytes: &[u8], pos: &mut usize, max_digits: usize) -> u8 {
    let mut val: u8 = 0;
    let mut count = 0;
    while *pos < bytes.len() && count < max_digits && bytes[*pos] >= b'0' && bytes[*pos] <= b'7' {
        val = val * 8 + (bytes[*pos] - b'0');
        *pos += 1;
        count += 1;
    }
    val
}

/// Parse up to `max_digits` hex digits, advancing `pos`. Returns the accumulated value.
fn parse_hex_digits(bytes: &[u8], pos: &mut usize, max_digits: usize) -> u8 {
    let mut val: u8 = 0;
    let mut count = 0;
    while *pos < bytes.len() && count < max_digits {
        let digit = match bytes[*pos] {
            b'0'..=b'9' => bytes[*pos] - b'0',
            b'a'..=b'f' => bytes[*pos] - b'a' + 10,
            b'A'..=b'F' => bytes[*pos] - b'A' + 10,
            _ => break,
        };
        val = val * 16 + digit;
        *pos += 1;
        count += 1;
    }
    val
}

fn pos_peek(bytes: &[u8], pos: usize) -> Option<u8> {
    bytes.get(pos).copied()
}

fn flush(buf: &mut String, parts: &mut Vec<WordPart>) {
    if !buf.is_empty() {
        parts.push(WordPart::Literal(std::mem::take(buf).into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(s: &str) -> WordPart {
        WordPart::Literal(s.into())
    }
    fn sq(s: &str) -> WordPart {
        WordPart::SingleQuoted(s.into())
    }
    fn dq(parts: Vec<WordPart>) -> WordPart {
        WordPart::DoubleQuoted(parts)
    }
    fn param(s: &str) -> WordPart {
        WordPart::Parameter(s.into())
    }
    fn cmd_subst(s: &str) -> WordPart {
        WordPart::CommandSubstitution(s.into())
    }
    fn arith(s: &str) -> WordPart {
        WordPart::Arithmetic(s.into())
    }

    #[test]
    fn plain_literal() {
        assert_eq!(parse_word_parts("hello"), vec![lit("hello")]);
    }

    #[test]
    fn single_quoted() {
        assert_eq!(parse_word_parts("'hello world'"), vec![sq("hello world")]);
    }

    #[test]
    fn double_quoted_literal() {
        assert_eq!(
            parse_word_parts("\"hello world\""),
            vec![dq(vec![lit("hello world")])]
        );
    }

    #[test]
    fn double_quoted_with_param() {
        assert_eq!(
            parse_word_parts("\"hello $USER\""),
            vec![dq(vec![lit("hello "), param("USER")])]
        );
    }

    #[test]
    fn double_quoted_with_brace_param() {
        assert_eq!(
            parse_word_parts("\"${HOME}/bin\""),
            vec![dq(vec![param("HOME"), lit("/bin")])]
        );
    }

    #[test]
    fn bare_param() {
        assert_eq!(
            parse_word_parts("$HOME/bin"),
            vec![param("HOME"), lit("/bin")]
        );
    }

    #[test]
    fn brace_param_with_default() {
        assert_eq!(parse_word_parts("${FOO:-bar}"), vec![param("FOO:-bar")]);
    }

    #[test]
    fn command_substitution() {
        assert_eq!(parse_word_parts("$(ls -la)"), vec![cmd_subst("ls -la")]);
    }

    #[test]
    fn arithmetic_expansion() {
        assert_eq!(parse_word_parts("$((1+2))"), vec![arith("1+2")]);
    }

    #[test]
    fn mixed_literal_and_expansion() {
        assert_eq!(
            parse_word_parts("hello$USER"),
            vec![lit("hello"), param("USER")]
        );
    }

    #[test]
    fn mixed_quoting_styles() {
        assert_eq!(
            parse_word_parts("hello'world'\"!\""),
            vec![lit("hello"), sq("world"), dq(vec![lit("!")])]
        );
    }

    #[test]
    fn backslash_escape() {
        assert_eq!(parse_word_parts("hello\\ world"), vec![lit("hello world")]);
    }

    #[test]
    fn special_param() {
        assert_eq!(parse_word_parts("$?"), vec![param("?")]);
        assert_eq!(parse_word_parts("$#"), vec![param("#")]);
        assert_eq!(parse_word_parts("$@"), vec![param("@")]);
        assert_eq!(parse_word_parts("$1"), vec![param("1")]);
    }

    #[test]
    fn lone_dollar() {
        assert_eq!(parse_word_parts("$"), vec![lit("$")]);
    }

    #[test]
    fn nested_command_in_double_quote() {
        assert_eq!(
            parse_word_parts("\"$(echo hi)\""),
            vec![dq(vec![cmd_subst("echo hi")])]
        );
    }

    #[test]
    fn double_quoted_backslash_escapes() {
        // In double quotes, \$ becomes $
        assert_eq!(
            parse_word_parts("\"\\$HOME\""),
            vec![dq(vec![lit("$HOME")])]
        );
    }

    #[test]
    fn assignment_like_word() {
        // FOO=bar is just a literal word at the word-parser level
        assert_eq!(parse_word_parts("FOO=bar"), vec![lit("FOO=bar")]);
    }

    // ---- ANSI-C quoting ----

    #[test]
    fn ansi_c_quote_newline() {
        assert_eq!(
            parse_word_parts("$'hello\\nworld'"),
            vec![lit("hello\nworld")]
        );
    }

    #[test]
    fn ansi_c_quote_tab() {
        assert_eq!(parse_word_parts("$'a\\tb'"), vec![lit("a\tb")]);
    }

    #[test]
    fn ansi_c_quote_backslash() {
        assert_eq!(parse_word_parts("$'a\\\\b'"), vec![lit("a\\b")]);
    }

    #[test]
    fn ansi_c_quote_hex() {
        assert_eq!(parse_word_parts("$'\\x41'"), vec![lit("A")]);
    }

    #[test]
    fn ansi_c_quote_octal() {
        // \0101 = 'A' (65 in octal is 101)
        assert_eq!(parse_word_parts("$'\\0101'"), vec![lit("A")]);
    }

    #[test]
    fn ansi_c_quote_escape() {
        assert_eq!(parse_word_parts("$'\\e'"), vec![lit("\x1b")]);
    }

    #[test]
    fn ansi_c_quote_mixed_with_literal() {
        assert_eq!(
            parse_word_parts("prefix$'\\n'suffix"),
            vec![lit("prefix"), lit("\n"), lit("suffix")]
        );
    }
}
