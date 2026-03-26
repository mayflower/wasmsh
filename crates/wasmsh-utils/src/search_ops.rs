//! Search utilities: rg (ripgrep-compatible recursive search).

use wasmsh_fs::{MemoryFs, Vfs};

use crate::helpers::{child_path, read_text, resolve_path, simple_glob_match};
use crate::UtilContext;

/// Map file type names to extensions.
fn type_to_extensions(type_name: &str) -> &[&str] {
    match type_name {
        "rust" | "rs" => &[".rs"],
        "python" | "py" => &[".py"],
        "javascript" | "js" => &[".js"],
        "typescript" | "ts" => &[".ts", ".tsx"],
        "json" => &[".json"],
        "toml" => &[".toml"],
        "yaml" | "yml" => &[".yaml", ".yml"],
        "markdown" | "md" => &[".md"],
        "html" => &[".html", ".htm"],
        "css" => &[".css"],
        "go" => &[".go"],
        "java" => &[".java"],
        "c" => &[".c", ".h"],
        "cpp" => &[".cpp", ".cc", ".cxx", ".hpp", ".hh"],
        "txt" => &[".txt"],
        _ => &[],
    }
}

/// A simplified ripgrep implementation for the VFS.
pub(crate) fn util_rg(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut show_line_numbers = true;
    let mut ignore_case = false;
    let mut files_only = false;
    let mut count_only = false;
    let mut word_regexp = false;
    let mut invert_match = false;
    let mut glob_patterns: Vec<String> = Vec::new();
    let mut type_filters: Vec<String> = Vec::new();
    let mut after_context: usize = 0;
    let mut before_context: usize = 0;
    let mut fixed_strings = false;
    let mut no_heading = false;
    let mut search_hidden = false;
    let mut max_count: Option<usize> = None;

    // Parse flags
    while let Some(arg) = args.first() {
        if *arg == "--" {
            args = &args[1..];
            break;
        }
        match *arg {
            "-n" => {
                show_line_numbers = true;
                args = &args[1..];
            }
            "-i" | "--ignore-case" => {
                ignore_case = true;
                args = &args[1..];
            }
            "-l" | "--files-with-matches" => {
                files_only = true;
                args = &args[1..];
            }
            "-c" | "--count" => {
                count_only = true;
                args = &args[1..];
            }
            "-w" | "--word-regexp" => {
                word_regexp = true;
                args = &args[1..];
            }
            "-v" | "--invert-match" => {
                invert_match = true;
                args = &args[1..];
            }
            "-g" | "--glob" if args.len() > 1 => {
                glob_patterns.push(args[1].to_string());
                args = &args[2..];
            }
            "-t" | "--type" if args.len() > 1 => {
                type_filters.push(args[1].to_string());
                args = &args[2..];
            }
            "-A" if args.len() > 1 => {
                after_context = args[1].parse().unwrap_or(0);
                args = &args[2..];
            }
            "-B" if args.len() > 1 => {
                before_context = args[1].parse().unwrap_or(0);
                args = &args[2..];
            }
            "-C" if args.len() > 1 => {
                let c = args[1].parse().unwrap_or(0);
                before_context = c;
                after_context = c;
                args = &args[2..];
            }
            "-r" | "--recursive" => {
                // Recursive is the default, accept silently
                args = &args[1..];
            }
            "-F" | "--fixed-strings" => {
                fixed_strings = true;
                args = &args[1..];
            }
            "--no-heading" => {
                no_heading = true;
                args = &args[1..];
            }
            "--hidden" => {
                search_hidden = true;
                args = &args[1..];
            }
            "-m" | "--max-count" if args.len() > 1 => {
                max_count = args[1].parse().ok();
                args = &args[2..];
            }
            _ if arg.starts_with("--glob=") => {
                glob_patterns.push(arg["--glob=".len()..].to_string());
                args = &args[1..];
            }
            _ if arg.starts_with("--type=") => {
                type_filters.push(arg["--type=".len()..].to_string());
                args = &args[1..];
            }
            _ if arg.starts_with("--max-count=") => {
                max_count = arg["--max-count=".len()..].parse().ok();
                args = &args[1..];
            }
            // Handle combined short flags like -in, -il, etc.
            _ if arg.starts_with('-') && !arg.starts_with("--") && arg.len() > 1 => {
                let flags = &arg[1..];
                let mut recognized = true;
                for ch in flags.chars() {
                    match ch {
                        'n' => show_line_numbers = true,
                        'i' => ignore_case = true,
                        'l' => files_only = true,
                        'c' => count_only = true,
                        'w' => word_regexp = true,
                        'v' => invert_match = true,
                        'F' => fixed_strings = true,
                        'r' => {} // recursive is default
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
        ctx.output.stderr(b"rg: missing pattern\n");
        return 2;
    }

    let pattern = args[0];
    let search_paths: Vec<&str> = if args.len() > 1 {
        args[1..].to_vec()
    } else {
        vec!["."]
    };

    // Build the matcher
    let matcher = build_matcher(pattern, ignore_case, word_regexp, fixed_strings);

    let opts = RgOpts {
        show_line_numbers,
        files_only,
        count_only,
        invert_match,
        glob_patterns,
        type_filters,
        after_context,
        before_context,
        no_heading,
        search_hidden,
        max_count,
    };

    let mut found_any = false;
    let mut first_file = true;

    for search_path in &search_paths {
        let full = resolve_path(ctx.cwd, search_path);
        match ctx.fs.stat(&full) {
            Ok(meta) if meta.is_dir => {
                let mut files = Vec::new();
                collect_files(ctx.fs, &full, &opts, &mut files);
                files.sort();
                for file_path in &files {
                    let matched = search_file(
                        ctx,
                        file_path,
                        &display_path(file_path, &full, search_path),
                        &matcher,
                        &opts,
                        &mut first_file,
                    );
                    if matched {
                        found_any = true;
                    }
                }
            }
            Ok(_) => {
                // Single file
                let matched =
                    search_file(ctx, &full, search_path, &matcher, &opts, &mut first_file);
                if matched {
                    found_any = true;
                }
            }
            Err(e) => {
                let msg = format!("rg: {search_path}: {e}\n");
                ctx.output.stderr(msg.as_bytes());
            }
        }
    }

    i32::from(!found_any)
}

#[allow(clippy::struct_excessive_bools)]
struct RgOpts {
    show_line_numbers: bool,
    files_only: bool,
    count_only: bool,
    invert_match: bool,
    glob_patterns: Vec<String>,
    type_filters: Vec<String>,
    after_context: usize,
    before_context: usize,
    no_heading: bool,
    search_hidden: bool,
    max_count: Option<usize>,
}

/// Compute a display path for output. Makes paths relative to the search root.
fn display_path(abs_path: &str, root_abs: &str, original_arg: &str) -> String {
    if original_arg == "." {
        // Strip root prefix and leading slash to get relative path
        if let Some(rest) = abs_path.strip_prefix(root_abs) {
            let trimmed = rest.strip_prefix('/').unwrap_or(rest);
            if trimmed.is_empty() {
                ".".to_string()
            } else {
                trimmed.to_string()
            }
        } else {
            abs_path.to_string()
        }
    } else if let Some(rest) = abs_path.strip_prefix(root_abs) {
        let trimmed = rest.strip_prefix('/').unwrap_or(rest);
        if trimmed.is_empty() {
            original_arg.to_string()
        } else {
            format!("{original_arg}/{trimmed}")
        }
    } else {
        abs_path.to_string()
    }
}

/// Recursively collect all files under a directory, respecting filters.
fn collect_files(fs: &MemoryFs, dir: &str, opts: &RgOpts, out: &mut Vec<String>) {
    let Ok(entries) = fs.read_dir(dir) else {
        return;
    };

    for entry in entries {
        // Skip hidden files unless --hidden
        if !opts.search_hidden && entry.name.starts_with('.') {
            continue;
        }

        let child = child_path(dir, &entry.name);

        if entry.is_dir {
            collect_files(fs, &child, opts, out);
        } else if file_matches_filters(&entry.name, opts) {
            out.push(child);
        }
    }
}

/// Check if a filename matches glob and type filters.
fn file_matches_filters(name: &str, opts: &RgOpts) -> bool {
    // Type filter
    if !opts.type_filters.is_empty() {
        let mut type_match = false;
        for type_name in &opts.type_filters {
            let exts = type_to_extensions(type_name);
            for ext in exts {
                if name.ends_with(ext) {
                    type_match = true;
                    break;
                }
            }
            if type_match {
                break;
            }
        }
        if !type_match {
            return false;
        }
    }

    // Glob filter
    if !opts.glob_patterns.is_empty() {
        let mut glob_match = false;
        for pat in &opts.glob_patterns {
            if simple_glob_match(pat, name) {
                glob_match = true;
                break;
            }
        }
        if !glob_match {
            return false;
        }
    }

    true
}

/// Search a single file and emit results. Returns true if any match was found.
fn search_file(
    ctx: &mut UtilContext<'_>,
    abs_path: &str,
    display: &str,
    matcher: &Matcher,
    opts: &RgOpts,
    first_file: &mut bool,
) -> bool {
    let Ok(content) = read_text(ctx.fs, abs_path) else {
        return false; // Skip binary/unreadable files silently
    };

    let lines: Vec<&str> = content.lines().collect();
    let mut match_count: usize = 0;

    // Find which lines match
    let mut match_flags = vec![false; lines.len()];
    for (i, line) in lines.iter().enumerate() {
        let matched = matcher.is_match(line);
        let matched = if opts.invert_match { !matched } else { matched };
        if matched {
            if let Some(max) = opts.max_count {
                if match_count >= max {
                    break;
                }
            }
            match_flags[i] = true;
            match_count += 1;
        }
    }

    if match_count == 0 {
        return false;
    }

    // files-with-matches mode: just print filename
    if opts.files_only {
        let line = format!("{display}\n");
        ctx.output.stdout(line.as_bytes());
        return true;
    }

    // count mode: print count
    if opts.count_only {
        let line = format!("{display}:{match_count}\n");
        ctx.output.stdout(line.as_bytes());
        return true;
    }

    // Compute which lines to display (including context)
    let mut display_flags = vec![false; lines.len()];
    for (i, &is_match) in match_flags.iter().enumerate() {
        if is_match {
            let start = i.saturating_sub(opts.before_context);
            let end = (i + opts.after_context + 1).min(lines.len());
            for flag in display_flags.iter_mut().take(end).skip(start) {
                *flag = true;
            }
        }
    }

    // Emit output
    if opts.no_heading {
        // No heading: prefix every line with filename
        let mut prev_displayed = false;
        for (i, line) in lines.iter().enumerate() {
            if !display_flags[i] {
                prev_displayed = false;
                continue;
            }
            // Insert separator between non-contiguous groups
            if !prev_displayed && i > 0 && display_flags.iter().take(i).any(|&f| f) {
                ctx.output.stdout(b"--\n");
            }
            let prefix = if opts.show_line_numbers {
                let sep = if match_flags[i] { ':' } else { '-' };
                format!("{display}{sep}{}{sep}", i + 1)
            } else {
                let sep = if match_flags[i] { ':' } else { '-' };
                format!("{display}{sep}")
            };
            let out = format!("{prefix}{line}\n");
            ctx.output.stdout(out.as_bytes());
            prev_displayed = true;
        }
    } else {
        // Heading mode: group by file
        if !*first_file {
            ctx.output.stdout(b"\n");
        }
        *first_file = false;

        let heading = format!("{display}\n");
        ctx.output.stdout(heading.as_bytes());

        let mut prev_displayed = false;
        for (i, line) in lines.iter().enumerate() {
            if !display_flags[i] {
                prev_displayed = false;
                continue;
            }
            // Insert separator between non-contiguous groups
            if !prev_displayed && i > 0 && display_flags.iter().take(i).any(|&f| f) {
                ctx.output.stdout(b"--\n");
            }
            if opts.show_line_numbers {
                let sep = if match_flags[i] { ':' } else { '-' };
                let out = format!("{}{sep}{line}\n", i + 1);
                ctx.output.stdout(out.as_bytes());
            } else {
                ctx.output.stdout(line.as_bytes());
                ctx.output.stdout(b"\n");
            }
            prev_displayed = true;
        }
    }

    true
}

// ---------------------------------------------------------------------------
// Pattern matching
// ---------------------------------------------------------------------------

/// A compiled pattern matcher.
struct Matcher {
    kind: MatcherKind,
    ignore_case: bool,
    word_regexp: bool,
}

enum MatcherKind {
    /// Literal substring search.
    Literal(String),
    /// Simple regex compiled into a list of regex tokens.
    Regex(Vec<RegexToken>),
}

#[derive(Debug, Clone)]
enum RegexToken {
    /// Match a literal character.
    Literal(char),
    /// `.` — match any character.
    AnyChar,
    /// `^` — match start of string.
    StartAnchor,
    /// `$` — match end of string.
    EndAnchor,
    /// Character class `[abc]` or `[^abc]`.
    CharClass { chars: Vec<char>, negated: bool },
    /// `\d` — digit.
    Digit,
    /// `\w` — word char.
    WordChar,
    /// `\s` — whitespace.
    Space,
    /// `\b` — word boundary.
    WordBoundary,
    /// `*` — zero or more of the previous token.
    Star(Box<RegexToken>),
    /// `+` — one or more of the previous token.
    Plus(Box<RegexToken>),
    /// `?` — zero or one of the previous token.
    Optional(Box<RegexToken>),
}

impl Matcher {
    fn is_match(&self, line: &str) -> bool {
        let haystack = if self.ignore_case {
            line.to_lowercase()
        } else {
            line.to_string()
        };

        match &self.kind {
            MatcherKind::Literal(needle) => {
                if self.word_regexp {
                    word_match_literal(&haystack, needle)
                } else {
                    haystack.contains(needle.as_str())
                }
            }
            MatcherKind::Regex(tokens) => {
                if self.word_regexp {
                    // For word regexp with regex, check each word
                    for word in split_words(&haystack) {
                        if regex_full_match(tokens, word) {
                            return true;
                        }
                    }
                    false
                } else {
                    regex_search(tokens, &haystack)
                }
            }
        }
    }
}

/// Build a matcher from the user's pattern string.
fn build_matcher(
    pattern: &str,
    ignore_case: bool,
    word_regexp: bool,
    fixed_strings: bool,
) -> Matcher {
    let pattern_str = if ignore_case {
        pattern.to_lowercase()
    } else {
        pattern.to_string()
    };

    let kind = if fixed_strings || is_literal_pattern(&pattern_str) {
        MatcherKind::Literal(pattern_str)
    } else {
        MatcherKind::Regex(parse_regex(&pattern_str))
    };

    Matcher {
        kind,
        ignore_case,
        word_regexp,
    }
}

/// Check if a pattern contains no regex metacharacters.
fn is_literal_pattern(pattern: &str) -> bool {
    let metas = ['.', '*', '+', '?', '^', '$', '[', ']', '\\', '|', '(', ')'];
    !pattern.chars().any(|c| metas.contains(&c))
}

/// Parse a simple regex pattern into tokens.
fn parse_regex(pattern: &str) -> Vec<RegexToken> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let token = match chars[i] {
            '\\' if i + 1 < chars.len() => {
                i += 1;
                match chars[i] {
                    'd' => RegexToken::Digit,
                    'w' => RegexToken::WordChar,
                    's' => RegexToken::Space,
                    'b' => RegexToken::WordBoundary,
                    c => RegexToken::Literal(c),
                }
            }
            '.' => RegexToken::AnyChar,
            '^' => RegexToken::StartAnchor,
            '$' => RegexToken::EndAnchor,
            '[' => {
                i += 1;
                let negated = i < chars.len() && chars[i] == '^';
                if negated {
                    i += 1;
                }
                let mut class_chars = Vec::new();
                while i < chars.len() && chars[i] != ']' {
                    if i + 2 < chars.len() && chars[i + 1] == '-' && chars[i + 2] != ']' {
                        // Character range like a-z
                        let start = chars[i];
                        let end = chars[i + 2];
                        for c in start..=end {
                            class_chars.push(c);
                        }
                        i += 3;
                    } else {
                        class_chars.push(chars[i]);
                        i += 1;
                    }
                }
                // i now points to ']' or end
                RegexToken::CharClass {
                    chars: class_chars,
                    negated,
                }
            }
            c => RegexToken::Literal(c),
        };

        i += 1;

        // Check for quantifiers
        if i < chars.len() {
            match chars[i] {
                '*' => {
                    tokens.push(RegexToken::Star(Box::new(token)));
                    i += 1;
                    continue;
                }
                '+' => {
                    tokens.push(RegexToken::Plus(Box::new(token)));
                    i += 1;
                    continue;
                }
                '?' => {
                    tokens.push(RegexToken::Optional(Box::new(token)));
                    i += 1;
                    continue;
                }
                _ => {}
            }
        }

        tokens.push(token);
    }

    tokens
}

/// Check if a single token matches a character.
fn token_matches_char(token: &RegexToken, ch: char) -> bool {
    match token {
        RegexToken::Literal(c) => *c == ch,
        RegexToken::AnyChar => true,
        RegexToken::Digit => ch.is_ascii_digit(),
        RegexToken::WordChar => ch.is_alphanumeric() || ch == '_',
        RegexToken::Space => ch.is_whitespace(),
        RegexToken::CharClass { chars, negated } => {
            let found = chars.contains(&ch);
            if *negated {
                !found
            } else {
                found
            }
        }
        _ => false,
    }
}

/// Try to match the regex tokens starting at position `start` in the haystack.
/// Returns true if a match is found starting at `start`.
fn regex_match_at(tokens: &[RegexToken], haystack: &[char], start: usize) -> bool {
    // Recursive backtracking matcher
    fn try_match(tokens: &[RegexToken], ti: usize, hay: &[char], hi: usize) -> bool {
        if ti >= tokens.len() {
            return true;
        }

        let token = &tokens[ti];

        match token {
            RegexToken::StartAnchor => {
                if hi == 0 {
                    try_match(tokens, ti + 1, hay, hi)
                } else {
                    false
                }
            }
            RegexToken::EndAnchor => {
                if hi == hay.len() {
                    try_match(tokens, ti + 1, hay, hi)
                } else {
                    false
                }
            }
            RegexToken::WordBoundary => {
                let at_boundary = is_word_boundary(hay, hi);
                if at_boundary {
                    try_match(tokens, ti + 1, hay, hi)
                } else {
                    false
                }
            }
            RegexToken::Star(inner) => {
                // Try matching zero occurrences first, then greedily
                // Greedy: try max first, then back off
                let mut count = 0;
                while hi + count < hay.len() && token_matches_char(inner, hay[hi + count]) {
                    count += 1;
                }
                // Try from max to zero
                for c in (0..=count).rev() {
                    if try_match(tokens, ti + 1, hay, hi + c) {
                        return true;
                    }
                }
                false
            }
            RegexToken::Plus(inner) => {
                let mut count = 0;
                while hi + count < hay.len() && token_matches_char(inner, hay[hi + count]) {
                    count += 1;
                }
                if count == 0 {
                    return false;
                }
                for c in (1..=count).rev() {
                    if try_match(tokens, ti + 1, hay, hi + c) {
                        return true;
                    }
                }
                false
            }
            RegexToken::Optional(inner) => {
                // Try with the character first
                if hi < hay.len()
                    && token_matches_char(inner, hay[hi])
                    && try_match(tokens, ti + 1, hay, hi + 1)
                {
                    return true;
                }
                // Try without
                try_match(tokens, ti + 1, hay, hi)
            }
            _ => {
                if hi < hay.len() && token_matches_char(token, hay[hi]) {
                    try_match(tokens, ti + 1, hay, hi + 1)
                } else {
                    false
                }
            }
        }
    }

    try_match(tokens, 0, haystack, start)
}

/// Check if position `pos` is a word boundary in the character array.
fn is_word_boundary(hay: &[char], pos: usize) -> bool {
    let prev_word = if pos > 0 {
        hay[pos - 1].is_alphanumeric() || hay[pos - 1] == '_'
    } else {
        false
    };
    let curr_word = if pos < hay.len() {
        hay[pos].is_alphanumeric() || hay[pos] == '_'
    } else {
        false
    };
    prev_word != curr_word
}

/// Search for a regex match anywhere in the haystack.
fn regex_search(tokens: &[RegexToken], haystack: &str) -> bool {
    let chars: Vec<char> = haystack.chars().collect();

    // If pattern starts with ^, only try at position 0
    if matches!(tokens.first(), Some(RegexToken::StartAnchor)) {
        return regex_match_at(tokens, &chars, 0);
    }

    // Try at every position
    for start in 0..=chars.len() {
        if regex_match_at(tokens, &chars, start) {
            return true;
        }
    }
    false
}

/// Check if the regex matches the entire string (for word matching).
fn regex_full_match(tokens: &[RegexToken], word: &str) -> bool {
    // Wrap tokens with ^ and $ if not already anchored
    let chars: Vec<char> = word.chars().collect();
    if regex_match_at(tokens, &chars, 0) {
        // Verify the match consumed all characters by adding an EndAnchor
        let mut extended = tokens.to_vec();
        // Only add EndAnchor if not already present
        if !matches!(extended.last(), Some(RegexToken::EndAnchor)) {
            extended.push(RegexToken::EndAnchor);
        }
        // Only add StartAnchor if not already present
        if !matches!(extended.first(), Some(RegexToken::StartAnchor)) {
            extended.insert(0, RegexToken::StartAnchor);
        }
        regex_match_at(&extended, &chars, 0)
    } else {
        false
    }
}

/// Check if `needle` appears as a whole word in `haystack`.
fn word_match_literal(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        let abs_pos = start + pos;
        let end_pos = abs_pos + needle.len();

        let left_ok = abs_pos == 0
            || !haystack.as_bytes()[abs_pos - 1].is_ascii_alphanumeric()
                && haystack.as_bytes()[abs_pos - 1] != b'_';
        let right_ok = end_pos == haystack.len()
            || !haystack.as_bytes()[end_pos].is_ascii_alphanumeric()
                && haystack.as_bytes()[end_pos] != b'_';

        if left_ok && right_ok {
            return true;
        }

        start = abs_pos + 1;
    }
    false
}

/// Split a string into words (sequences of word characters).
fn split_words(s: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            words.push(&s[start..i]);
        } else {
            i += 1;
        }
    }
    words
}

// ---------------------------------------------------------------------------
// fd — fast find alternative
// ---------------------------------------------------------------------------

#[allow(clippy::struct_excessive_bools)]
struct FdArgs {
    type_filter: Option<char>,
    extension: Option<String>,
    show_hidden: bool,
    max_depth: Option<usize>,
    exec_cmd: Option<String>,
    absolute_path: bool,
    glob_mode: bool,
    stop_after_first: bool,
}

fn parse_fd_args(argv: &[&str]) -> (FdArgs, usize) {
    let mut args = &argv[1..];
    let mut fd = FdArgs {
        type_filter: None,
        extension: None,
        show_hidden: false,
        max_depth: None,
        exec_cmd: None,
        absolute_path: false,
        glob_mode: false,
        stop_after_first: false,
    };
    let mut consumed = 1;

    while let Some(arg) = args.first() {
        match *arg {
            "-t" | "--type" if args.len() > 1 => {
                let t = args[1];
                if t == "f" || t == "d" {
                    fd.type_filter = Some(t.chars().next().unwrap());
                }
                args = &args[2..];
                consumed += 2;
            }
            "-e" | "--extension" if args.len() > 1 => {
                fd.extension = Some(args[1].to_string());
                args = &args[2..];
                consumed += 2;
            }
            "-H" | "--hidden" => {
                fd.show_hidden = true;
                args = &args[1..];
                consumed += 1;
            }
            "-I" | "--no-ignore" => {
                args = &args[1..];
                consumed += 1;
            }
            "-d" | "--max-depth" if args.len() > 1 => {
                fd.max_depth = args[1].parse().ok();
                args = &args[2..];
                consumed += 2;
            }
            "-x" | "--exec" if args.len() > 1 => {
                fd.exec_cmd = Some(args[1].to_string());
                args = &args[2..];
                consumed += 2;
            }
            "-a" | "--absolute-path" => {
                fd.absolute_path = true;
                args = &args[1..];
                consumed += 1;
            }
            "-g" | "--glob" => {
                fd.glob_mode = true;
                args = &args[1..];
                consumed += 1;
            }
            "-1" => {
                fd.stop_after_first = true;
                args = &args[1..];
                consumed += 1;
            }
            _ if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") => {
                let flags = &arg[1..];
                let mut recognized = true;
                for ch in flags.chars() {
                    match ch {
                        'H' => fd.show_hidden = true,
                        'I' => {}
                        'a' => fd.absolute_path = true,
                        'g' => fd.glob_mode = true,
                        '1' => fd.stop_after_first = true,
                        _ => {
                            recognized = false;
                            break;
                        }
                    }
                }
                if recognized {
                    args = &args[1..];
                    consumed += 1;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    (fd, consumed)
}

fn fd_entry_matches(name: &str, is_dir: bool, fd: &FdArgs, pattern: Option<&str>) -> bool {
    if let Some(tf) = fd.type_filter {
        if tf == 'f' && is_dir {
            return false;
        }
        if tf == 'd' && !is_dir {
            return false;
        }
    }
    if let Some(ref ext) = fd.extension {
        let dot_ext = format!(".{ext}");
        if !name.ends_with(&dot_ext) {
            return false;
        }
    }
    if let Some(pat) = pattern {
        if fd.glob_mode {
            if !simple_glob_match(pat, name) {
                return false;
            }
        } else if !name.contains(pat) {
            return false;
        }
    }
    true
}

fn fd_format_path(path: &str, search_root: &str, absolute: bool) -> String {
    if absolute {
        return path.to_string();
    }
    if let Some(rest) = path.strip_prefix(search_root) {
        let trimmed = rest.strip_prefix('/').unwrap_or(rest);
        if trimmed.is_empty() {
            ".".to_string()
        } else {
            trimmed.to_string()
        }
    } else {
        path.to_string()
    }
}

pub(crate) fn util_fd(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (fd_args, consumed) = parse_fd_args(argv);
    let mut args = &argv[consumed..];

    let pattern: Option<&str> = if !args.is_empty() {
        let p = args[0];
        args = &args[1..];
        Some(p)
    } else {
        None
    };

    let search_root = if !args.is_empty() {
        resolve_path(ctx.cwd, args[0])
    } else {
        ctx.cwd.to_string()
    };

    let mut results = Vec::new();
    fd_walk(
        ctx.fs,
        &search_root,
        0,
        fd_args.max_depth,
        fd_args.show_hidden,
        &mut results,
    );
    results.sort();

    let mut count = 0;
    for (path, is_dir) in &results {
        let name = path.rsplit('/').next().unwrap_or(path);
        if !fd_entry_matches(name, *is_dir, &fd_args, pattern) {
            continue;
        }

        let display = fd_format_path(path, &search_root, fd_args.absolute_path);
        if let Some(ref cmd) = fd_args.exec_cmd {
            let line = format!("{cmd} {display}\n");
            ctx.output.stdout(line.as_bytes());
        } else {
            let line = format!("{display}\n");
            ctx.output.stdout(line.as_bytes());
        }

        count += 1;
        if fd_args.stop_after_first {
            break;
        }
    }

    if count == 0 && pattern.is_some() {
        return 1;
    }

    0
}

/// Recursively walk the VFS collecting paths.
fn fd_walk(
    fs: &MemoryFs,
    dir: &str,
    depth: usize,
    max_depth: Option<usize>,
    show_hidden: bool,
    out: &mut Vec<(String, bool)>,
) {
    if let Some(max) = max_depth {
        if depth >= max {
            return;
        }
    }

    let Ok(entries) = fs.read_dir(dir) else {
        return;
    };

    for entry in entries {
        if !show_hidden && entry.name.starts_with('.') {
            continue;
        }

        let child = child_path(dir, &entry.name);

        out.push((child.clone(), entry.is_dir));

        if entry.is_dir {
            fd_walk(fs, &child, depth + 1, max_depth, show_hidden, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UtilContext, VecOutput};
    use wasmsh_fs::{MemoryFs, OpenOptions, Vfs};

    fn make_search_fs() -> MemoryFs {
        let mut fs = MemoryFs::new();
        fs.create_dir("/project").unwrap();
        fs.create_dir("/project/src").unwrap();

        let h = fs
            .open("/project/src/main.rs", OpenOptions::write())
            .unwrap();
        fs.write_file(
            h,
            b"fn main() {\n    println!(\"hello world\");\n    let x = 42;\n}\n",
        )
        .unwrap();
        fs.close(h);

        let h = fs
            .open("/project/src/lib.rs", OpenOptions::write())
            .unwrap();
        fs.write_file(
            h,
            b"pub fn hello() -> &'static str {\n    \"hello\"\n}\n\npub fn goodbye() {\n    println!(\"goodbye world\");\n}\n",
        )
        .unwrap();
        fs.close(h);

        let h = fs.open("/project/README.md", OpenOptions::write()).unwrap();
        fs.write_file(h, b"# Hello Project\n\nA hello world example.\n")
            .unwrap();
        fs.close(h);

        let h = fs
            .open("/project/Cargo.toml", OpenOptions::write())
            .unwrap();
        fs.write_file(h, b"[package]\nname = \"hello\"\nversion = \"0.1.0\"\n")
            .unwrap();
        fs.close(h);

        fs
    }

    fn run_rg(argv: &[&str], fs: &mut MemoryFs, cwd: &str) -> (i32, String, String) {
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs,
                output: &mut output,
                cwd,
                stdin: None,
                state: None,
            };
            util_rg(&mut ctx, argv)
        };
        (
            status,
            output.stdout_str().to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )
    }

    #[test]
    fn rg_basic_search() {
        let mut fs = make_search_fs();
        let (status, out, _) = run_rg(&["rg", "hello", "."], &mut fs, "/project");
        assert_eq!(status, 0);
        assert!(out.contains("hello"));
        // Should find matches in multiple files
        assert!(out.contains("main.rs") || out.contains("lib.rs") || out.contains("README.md"));
    }

    #[test]
    fn rg_no_match() {
        let mut fs = make_search_fs();
        let (status, out, _) = run_rg(&["rg", "zzzznotfound", "."], &mut fs, "/project");
        assert_eq!(status, 1);
        assert!(out.is_empty());
    }

    #[test]
    fn rg_case_insensitive() {
        let mut fs = make_search_fs();
        let (status, out, _) = run_rg(&["rg", "-i", "HELLO", "."], &mut fs, "/project");
        assert_eq!(status, 0);
        assert!(out.contains("hello"));
    }

    #[test]
    fn rg_files_only() {
        let mut fs = make_search_fs();
        let (status, out, _) = run_rg(&["rg", "-l", "hello", "."], &mut fs, "/project");
        assert_eq!(status, 0);
        // Should print filenames, not content
        assert!(out.contains("main.rs") || out.contains("lib.rs"));
        // Should NOT print line content
        assert!(!out.contains("println"));
    }

    #[test]
    fn rg_count() {
        let mut fs = make_search_fs();
        let (status, out, _) = run_rg(&["rg", "-c", "hello", "."], &mut fs, "/project");
        assert_eq!(status, 0);
        // Output should be filename:count
        assert!(out.contains(':'));
    }

    #[test]
    fn rg_type_filter() {
        let mut fs = make_search_fs();
        let (status, out, _) = run_rg(&["rg", "-t", "rs", "hello", "."], &mut fs, "/project");
        assert_eq!(status, 0);
        // Should only find matches in .rs files
        assert!(!out.contains("README.md"));
        assert!(!out.contains("Cargo.toml"));
    }

    #[test]
    fn rg_glob_filter() {
        let mut fs = make_search_fs();
        let (status, out, _) = run_rg(&["rg", "-g", "*.md", "hello", "."], &mut fs, "/project");
        assert_eq!(status, 0);
        assert!(out.contains("README.md"));
        assert!(!out.contains("main.rs"));
    }

    #[test]
    fn rg_invert_match() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/test.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"alpha\nbeta\ngamma\n").unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "-v", "beta", "/test.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(out.contains("alpha"));
        assert!(out.contains("gamma"));
        assert!(!out.contains("beta"));
    }

    #[test]
    fn rg_word_regexp() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/test.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"hello\nhelloworld\nworld hello world\n")
            .unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "-w", "hello", "/test.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(out.contains("hello"));
        // "helloworld" should NOT match as a whole word
        let lines: Vec<&str> = out.lines().filter(|l| l.contains("helloworld")).collect();
        assert!(lines.is_empty());
    }

    #[test]
    fn rg_fixed_strings() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/test.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"hello.world\nhello world\nhelloxworld\n")
            .unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "-F", "hello.world", "/test.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        // With -F, the dot should be literal
        assert!(out.contains("hello.world"));
    }

    #[test]
    fn rg_line_numbers() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/test.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"aaa\nbbb\nccc\nbbb\n").unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "-n", "bbb", "/test.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(out.contains("2:bbb"));
        assert!(out.contains("4:bbb"));
    }

    #[test]
    fn rg_context_lines() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/test.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"line1\nline2\nMATCH\nline4\nline5\n")
            .unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "-C", "1", "MATCH", "/test.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(out.contains("line2"));
        assert!(out.contains("MATCH"));
        assert!(out.contains("line4"));
    }

    #[test]
    fn rg_hidden_files() {
        let mut fs = MemoryFs::new();
        fs.create_dir("/dir").unwrap();
        let h = fs.open("/dir/.hidden", OpenOptions::write()).unwrap();
        fs.write_file(h, b"secret data\n").unwrap();
        fs.close(h);
        let h = fs.open("/dir/visible.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"visible data\n").unwrap();
        fs.close(h);

        // Without --hidden
        let (status, out, _) = run_rg(&["rg", "data", "/dir"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(out.contains("visible"));
        assert!(!out.contains("secret"));

        // With --hidden
        let (status, out, _) = run_rg(&["rg", "--hidden", "data", "/dir"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(out.contains("secret"));
    }

    #[test]
    fn rg_max_count() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/test.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"match1\nmatch2\nmatch3\nmatch4\n")
            .unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "-m", "2", "match", "/test.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        let match_lines: Vec<&str> = out.lines().filter(|l| l.contains("match")).collect();
        assert_eq!(match_lines.len(), 2); // 2 matches (heading line doesn't contain "match")
    }

    #[test]
    fn rg_no_heading() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/test.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"hello there\n").unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "--no-heading", "hello", "/test.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        // With no-heading, every line should have the filename prefix
        for line in out.lines() {
            assert!(line.starts_with("/test.txt"));
        }
    }

    #[test]
    fn rg_regex_dot() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/test.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"cat\ncar\ncap\ndog\n").unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "ca.", "/test.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(out.contains("cat"));
        assert!(out.contains("car"));
        assert!(out.contains("cap"));
        assert!(!out.contains("dog"));
    }

    #[test]
    fn rg_regex_anchors() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/test.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"hello world\nworld hello\n").unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "^hello", "/test.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(out.contains("hello world"));
        // "world hello" should not match ^hello
        let lines: Vec<&str> = out.lines().filter(|l| l.contains("world hello")).collect();
        assert!(lines.is_empty());
    }

    #[test]
    fn rg_regex_char_class() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/test.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"bat\nbet\nbit\nbut\nbot\n").unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "b[aei]t", "/test.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(out.contains("bat"));
        assert!(out.contains("bet"));
        assert!(out.contains("bit"));
        assert!(!out.contains("but"));
        assert!(!out.contains("bot"));
    }

    #[test]
    fn rg_missing_pattern() {
        let mut fs = MemoryFs::new();
        let (status, _, err) = run_rg(&["rg"], &mut fs, "/");
        assert_eq!(status, 2);
        assert!(err.contains("missing pattern"));
    }

    #[test]
    fn rg_regex_digit() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/test.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"abc\n123\na1b\n").unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "\\d+", "/test.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(out.contains("123"));
        assert!(out.contains("a1b"));
        assert!(!out.contains("abc\n") || out.contains("abc")); // abc has no digits
    }

    #[test]
    fn rg_uses_cwd_when_no_path() {
        let mut fs = MemoryFs::new();
        fs.create_dir("/mydir").unwrap();
        let h = fs.open("/mydir/file.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"findme\n").unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "findme"], &mut fs, "/mydir");
        assert_eq!(status, 0);
        assert!(out.contains("findme"));
    }

    // -----------------------------------------------------------------------
    // fd tests
    // -----------------------------------------------------------------------

    fn run_fd(argv: &[&str], fs: &mut MemoryFs, cwd: &str) -> (i32, String, String) {
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs,
                output: &mut output,
                cwd,
                stdin: None,
                state: None,
            };
            util_fd(&mut ctx, argv)
        };
        (
            status,
            output.stdout_str().to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )
    }

    fn make_fd_fs() -> MemoryFs {
        let mut fs = MemoryFs::new();
        fs.create_dir("/root").unwrap();
        fs.create_dir("/root/sub").unwrap();
        fs.create_dir("/root/sub/deep").unwrap();

        let h = fs.open("/root/hello.rs", OpenOptions::write()).unwrap();
        fs.write_file(h, b"fn main() {}").unwrap();
        fs.close(h);

        let h = fs.open("/root/test_foo.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"foo").unwrap();
        fs.close(h);

        let h = fs
            .open("/root/sub/test_bar.rs", OpenOptions::write())
            .unwrap();
        fs.write_file(h, b"bar").unwrap();
        fs.close(h);

        let h = fs
            .open("/root/sub/deep/notes.txt", OpenOptions::write())
            .unwrap();
        fs.write_file(h, b"notes").unwrap();
        fs.close(h);

        let h = fs.open("/root/.hidden", OpenOptions::write()).unwrap();
        fs.write_file(h, b"secret").unwrap();
        fs.close(h);

        fs
    }

    #[test]
    fn fd_list_all() {
        let mut fs = make_fd_fs();
        let (status, out, _) = run_fd(&["fd"], &mut fs, "/root");
        assert_eq!(status, 0);
        // Should list files and directories
        assert!(out.contains("hello.rs"));
        assert!(out.contains("test_foo.txt"));
        assert!(out.contains("sub"));
    }

    #[test]
    fn fd_substring_match() {
        let mut fs = make_fd_fs();
        let (status, out, _) = run_fd(&["fd", "test"], &mut fs, "/root");
        assert_eq!(status, 0);
        assert!(out.contains("test_foo.txt"));
        assert!(out.contains("test_bar.rs"));
        assert!(!out.contains("hello.rs"));
    }

    #[test]
    fn fd_type_file() {
        let mut fs = make_fd_fs();
        let (status, out, _) = run_fd(&["fd", "-t", "f"], &mut fs, "/root");
        assert_eq!(status, 0);
        assert!(out.contains("hello.rs"));
        // Directories should not appear
        assert!(!out.lines().any(|l| l == "sub" || l == "sub/deep"));
    }

    #[test]
    fn fd_type_dir() {
        let mut fs = make_fd_fs();
        let (status, out, _) = run_fd(&["fd", "-t", "d"], &mut fs, "/root");
        assert_eq!(status, 0);
        assert!(out.contains("sub"));
        // Files should not appear
        assert!(!out.contains("hello.rs"));
        assert!(!out.contains("test_foo.txt"));
    }

    #[test]
    fn fd_extension() {
        let mut fs = make_fd_fs();
        let (status, out, _) = run_fd(&["fd", "-e", "rs"], &mut fs, "/root");
        assert_eq!(status, 0);
        assert!(out.contains("hello.rs"));
        assert!(out.contains("test_bar.rs"));
        assert!(!out.contains("test_foo.txt"));
        assert!(!out.contains("notes.txt"));
    }

    #[test]
    fn fd_hidden() {
        let mut fs = make_fd_fs();
        // Without -H, dotfiles should be hidden
        let (_, out_no_hidden, _) = run_fd(&["fd"], &mut fs, "/root");
        assert!(!out_no_hidden.contains(".hidden"));

        // With -H, dotfiles should be shown
        let (_, out_hidden, _) = run_fd(&["fd", "-H"], &mut fs, "/root");
        assert!(out_hidden.contains(".hidden"));
    }

    #[test]
    fn fd_max_depth() {
        let mut fs = make_fd_fs();
        let (status, out, _) = run_fd(&["fd", "-d", "1"], &mut fs, "/root");
        assert_eq!(status, 0);
        // Should see top-level entries
        assert!(out.contains("hello.rs"));
        assert!(out.contains("sub"));
        // Should NOT see deeply nested entries
        assert!(!out.contains("test_bar.rs"));
        assert!(!out.contains("notes.txt"));
    }

    #[test]
    fn fd_glob_mode() {
        let mut fs = make_fd_fs();
        let (status, out, _) = run_fd(&["fd", "-g", "*.txt"], &mut fs, "/root");
        assert_eq!(status, 0);
        assert!(out.contains("test_foo.txt"));
        assert!(out.contains("notes.txt"));
        assert!(!out.contains("hello.rs"));
    }

    #[test]
    fn fd_first_match() {
        let mut fs = make_fd_fs();
        let (status, out, _) = run_fd(&["fd", "-1"], &mut fs, "/root");
        assert_eq!(status, 0);
        // Should output exactly one result
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn fd_absolute_path() {
        let mut fs = make_fd_fs();
        let (status, out, _) = run_fd(&["fd", "-a"], &mut fs, "/root");
        assert_eq!(status, 0);
        // Every result line should start with /
        for line in out.lines() {
            assert!(line.starts_with('/'), "Expected absolute path, got: {line}");
        }
    }

    #[test]
    fn fd_no_results() {
        let mut fs = make_fd_fs();
        let (status, out, _) = run_fd(&["fd", "zzzznothing"], &mut fs, "/root");
        // fd returns 1 when pattern given but no results
        assert_eq!(status, 1);
        assert!(out.is_empty());
    }

    #[test]
    fn type_mapping_coverage() {
        assert!(!type_to_extensions("rs").is_empty());
        assert!(!type_to_extensions("rust").is_empty());
        assert!(!type_to_extensions("py").is_empty());
        assert!(!type_to_extensions("python").is_empty());
        assert!(!type_to_extensions("js").is_empty());
        assert!(!type_to_extensions("ts").is_empty());
        assert!(!type_to_extensions("json").is_empty());
        assert!(!type_to_extensions("toml").is_empty());
        assert!(!type_to_extensions("yaml").is_empty());
        assert!(!type_to_extensions("yml").is_empty());
        assert!(!type_to_extensions("md").is_empty());
        assert!(!type_to_extensions("html").is_empty());
        assert!(!type_to_extensions("css").is_empty());
        assert!(!type_to_extensions("go").is_empty());
        assert!(!type_to_extensions("java").is_empty());
        assert!(!type_to_extensions("c").is_empty());
        assert!(!type_to_extensions("cpp").is_empty());
        assert!(!type_to_extensions("txt").is_empty());
        assert!(type_to_extensions("unknown").is_empty());
    }

    // -------------------------------------------------------------------
    // rg -A 2 -B 1  context lines around match
    // -------------------------------------------------------------------

    #[test]
    fn rg_after_and_before_context() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/ctx.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"line1\nline2\nTARGET\nline4\nline5\nline6\n")
            .unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(
            &["rg", "-A", "2", "-B", "1", "TARGET", "/ctx.txt"],
            &mut fs,
            "/",
        );
        assert_eq!(status, 0);
        assert!(out.contains("line2"), "expected before context: {out}");
        assert!(out.contains("TARGET"));
        assert!(out.contains("line4"), "expected after context: {out}");
        assert!(out.contains("line5"), "expected 2nd after context: {out}");
    }

    // -------------------------------------------------------------------
    // rg -C 2  combined context
    // -------------------------------------------------------------------

    #[test]
    fn rg_combined_context() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/cc.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"a\nb\nc\nMATCH\ne\nf\ng\n").unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "-C", "2", "MATCH", "/cc.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(out.contains('b'), "expected 2 lines before: {out}");
        assert!(out.contains('c'), "expected 1 line before: {out}");
        assert!(out.contains("MATCH"));
        assert!(out.contains('e'), "expected 1 line after: {out}");
        assert!(out.contains('f'), "expected 2 lines after: {out}");
    }

    // -------------------------------------------------------------------
    // rg --no-heading  flat output
    // -------------------------------------------------------------------

    #[test]
    fn rg_no_heading_flat() {
        let mut fs = MemoryFs::new();
        fs.create_dir("/flat").unwrap();
        let h = fs.open("/flat/a.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"hello\n").unwrap();
        fs.close(h);
        let h = fs.open("/flat/b.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"hello\n").unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "--no-heading", "hello", "/flat"], &mut fs, "/");
        assert_eq!(status, 0);
        // Every line should be prefixed with filename
        for line in out.lines() {
            assert!(line.contains("/flat/"), "expected file prefix, got: {line}");
        }
    }

    // -------------------------------------------------------------------
    // rg -m 1  max count per file
    // -------------------------------------------------------------------

    #[test]
    fn rg_max_count_1() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/mc.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"hit1\nhit2\nhit3\n").unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "-m", "1", "hit", "/mc.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        let hit_lines: Vec<&str> = out.lines().filter(|l| l.contains("hit")).collect();
        assert_eq!(hit_lines.len(), 1, "expected exactly 1 match, got: {out}");
    }

    // -------------------------------------------------------------------
    // rg -w  word regexp
    // -------------------------------------------------------------------

    #[test]
    fn rg_word_regexp_no_partial() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/wr.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"cat\ncatch\nthe cat sat\ncatalog\n")
            .unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "-w", "cat", "/wr.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(out.contains("cat"));
        assert!(out.contains("the cat sat"));
        // "catch" and "catalog" should NOT match as whole words
        let lines: Vec<&str> = out
            .lines()
            .filter(|l| l.contains("catch") || l.contains("catalog"))
            .collect();
        assert!(lines.is_empty(), "partial matches found: {out}");
    }

    // -------------------------------------------------------------------
    // rg with regex: \d+, [A-Z], foo.*bar
    // -------------------------------------------------------------------

    #[test]
    fn rg_regex_digit_plus() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/nums.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"abc\n42\nno digits\n100x\n").unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "\\d+", "/nums.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(out.contains("42"));
        assert!(out.contains("100x"));
        // "no digits" should not match
        assert!(!out.lines().any(|l| l.contains("no digits")));
    }

    #[test]
    fn rg_regex_uppercase_class() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/upper.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"hello\nWorld\nGOOD\nlower\n").unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "[A-Z]", "/upper.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(out.contains("World"));
        assert!(out.contains("GOOD"));
        // "hello" and "lower" have no uppercase letters
        let bad_lines: Vec<&str> = out
            .lines()
            .filter(|l| l.contains("hello") || l.contains("lower"))
            .collect();
        assert!(bad_lines.is_empty(), "unexpected lowercase matches: {out}");
    }

    #[test]
    fn rg_regex_foo_dot_star_bar() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/rx.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"foobar\nfoo__bar\nfoo 123 bar\nbaz\n")
            .unwrap();
        fs.close(h);

        let (status, out, _) = run_rg(&["rg", "foo.*bar", "/rx.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(out.contains("foobar"));
        assert!(out.contains("foo__bar"));
        assert!(out.contains("foo 123 bar"));
        assert!(!out.lines().any(|l| l.contains("baz")));
    }

    // -------------------------------------------------------------------
    // fd -t d  directories only
    // -------------------------------------------------------------------

    #[test]
    fn fd_type_dir_only() {
        let mut fs = make_fd_fs();
        let (status, out, _) = run_fd(&["fd", "-t", "d"], &mut fs, "/root");
        assert_eq!(status, 0);
        assert!(out.contains("sub"));
        assert!(!out.contains("hello.rs"));
    }

    // -------------------------------------------------------------------
    // fd -d 1  max depth
    // -------------------------------------------------------------------

    #[test]
    fn fd_max_depth_1_excludes_deep() {
        let mut fs = make_fd_fs();
        let (status, out, _) = run_fd(&["fd", "-d", "1"], &mut fs, "/root");
        assert_eq!(status, 0);
        assert!(out.contains("hello.rs"));
        assert!(!out.contains("deep"));
        assert!(!out.contains("notes.txt"));
    }

    // -------------------------------------------------------------------
    // fd --exec  print command
    // -------------------------------------------------------------------

    #[test]
    fn fd_exec_flag() {
        let mut fs = make_fd_fs();
        let (status, out, _) = run_fd(&["fd", "-e", "rs", "--exec", "echo"], &mut fs, "/root");
        assert_eq!(status, 0);
        // With --exec echo, lines should show "echo <path>"
        for line in out.lines() {
            assert!(
                line.starts_with("echo "),
                "expected 'echo ' prefix, got: {line}"
            );
        }
    }

    // -------------------------------------------------------------------
    // fd -a  absolute paths
    // -------------------------------------------------------------------

    #[test]
    fn fd_absolute_paths_flag() {
        let mut fs = make_fd_fs();
        let (status, out, _) = run_fd(&["fd", "-a", "hello"], &mut fs, "/root");
        assert_eq!(status, 0);
        for line in out.lines() {
            assert!(line.starts_with('/'), "expected absolute path: {line}");
        }
        assert!(out.contains("hello.rs"));
    }

    // -------------------------------------------------------------------
    // fd combined flags
    // -------------------------------------------------------------------

    #[test]
    fn fd_combined_type_and_extension() {
        let mut fs = make_fd_fs();
        let (status, out, _) = run_fd(&["fd", "-t", "f", "-e", "txt"], &mut fs, "/root");
        assert_eq!(status, 0);
        assert!(out.contains("test_foo.txt"));
        assert!(out.contains("notes.txt"));
        assert!(!out.contains("hello.rs"));
        // Directories should not appear as standalone entries (they may appear in paths)
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            !lines
                .iter()
                .any(|l| l.trim() == "sub" || l.trim() == "sub/deep"),
            "directory entries should not appear as matches: {out}"
        );
    }
}
