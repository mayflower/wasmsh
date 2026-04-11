//! Search utilities: rg (ripgrep-compatible recursive search).

use wasmsh_fs::{BackendFs, Vfs};

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

/// Mutable state accumulated while parsing rg flags.
#[allow(clippy::struct_excessive_bools)]
struct RgFlagState {
    show_line_numbers: bool,
    ignore_case: bool,
    files_only: bool,
    count_only: bool,
    word_regexp: bool,
    invert_match: bool,
    glob_patterns: Vec<String>,
    type_filters: Vec<String>,
    after_context: usize,
    before_context: usize,
    fixed_strings: bool,
    no_heading: bool,
    search_hidden: bool,
    max_count: Option<usize>,
}

impl Default for RgFlagState {
    fn default() -> Self {
        Self {
            show_line_numbers: true,
            ignore_case: false,
            files_only: false,
            count_only: false,
            word_regexp: false,
            invert_match: false,
            glob_patterns: Vec::new(),
            type_filters: Vec::new(),
            after_context: 0,
            before_context: 0,
            fixed_strings: false,
            no_heading: false,
            search_hidden: false,
            max_count: None,
        }
    }
}

/// Parse one rg flag. Returns the number of argv slots consumed, or 0 to stop.
fn parse_rg_single_flag(arg: &str, args: &[&str], st: &mut RgFlagState) -> usize {
    match arg {
        "-n" => {
            st.show_line_numbers = true;
            1
        }
        "-i" | "--ignore-case" => {
            st.ignore_case = true;
            1
        }
        "-l" | "--files-with-matches" => {
            st.files_only = true;
            1
        }
        "-c" | "--count" => {
            st.count_only = true;
            1
        }
        "-w" | "--word-regexp" => {
            st.word_regexp = true;
            1
        }
        "-v" | "--invert-match" => {
            st.invert_match = true;
            1
        }
        "-g" | "--glob" if args.len() > 1 => {
            st.glob_patterns.push(args[1].to_string());
            2
        }
        "-t" | "--type" if args.len() > 1 => {
            st.type_filters.push(args[1].to_string());
            2
        }
        "-A" if args.len() > 1 => {
            st.after_context = args[1].parse().unwrap_or(0);
            2
        }
        "-B" if args.len() > 1 => {
            st.before_context = args[1].parse().unwrap_or(0);
            2
        }
        "-C" if args.len() > 1 => {
            let c = args[1].parse().unwrap_or(0);
            st.before_context = c;
            st.after_context = c;
            2
        }
        "-r" | "--recursive" => 1, // recursive is the default
        "-F" | "--fixed-strings" => {
            st.fixed_strings = true;
            1
        }
        "--no-heading" => {
            st.no_heading = true;
            1
        }
        "--hidden" => {
            st.search_hidden = true;
            1
        }
        "-m" | "--max-count" if args.len() > 1 => {
            st.max_count = args[1].parse().ok();
            2
        }
        _ => parse_rg_long_equals_or_bundled(arg, st),
    }
}

/// Handle `--glob=VAL`, `--type=VAL`, `--max-count=VAL`, and bundled short flags.
fn parse_rg_long_equals_or_bundled(arg: &str, st: &mut RgFlagState) -> usize {
    if let Some(val) = arg.strip_prefix("--glob=") {
        st.glob_patterns.push(val.to_string());
        return 1;
    }
    if let Some(val) = arg.strip_prefix("--type=") {
        st.type_filters.push(val.to_string());
        return 1;
    }
    if let Some(val) = arg.strip_prefix("--max-count=") {
        st.max_count = val.parse().ok();
        return 1;
    }
    if arg.starts_with('-') && !arg.starts_with("--") && arg.len() > 1 {
        return usize::from(parse_rg_bundled_flags(&arg[1..], st));
    }
    0
}

/// Apply each character in a bundled short-flag group (e.g. `-inl`).
fn parse_rg_bundled_flags(flags: &str, st: &mut RgFlagState) -> bool {
    for ch in flags.chars() {
        match ch {
            'n' => st.show_line_numbers = true,
            'i' => st.ignore_case = true,
            'l' => st.files_only = true,
            'c' => st.count_only = true,
            'w' => st.word_regexp = true,
            'v' => st.invert_match = true,
            'F' => st.fixed_strings = true,
            'r' => {} // recursive is default
            _ => return false,
        }
    }
    true
}

/// Parse the full rg argv, returning the flags, pattern index and consumed count.
fn parse_rg_args(argv: &[&str]) -> (RgFlagState, usize) {
    let mut st = RgFlagState::default();
    let mut pos = 1; // skip argv[0]

    while pos < argv.len() {
        if argv[pos] == "--" {
            pos += 1;
            break;
        }
        let advance = parse_rg_single_flag(argv[pos], &argv[pos..], &mut st);
        if advance == 0 {
            break;
        }
        pos += advance;
    }

    (st, pos)
}

/// A simplified ripgrep implementation for the VFS.
pub(crate) fn util_rg(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (st, pos) = parse_rg_args(argv);
    let args = &argv[pos..];

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

    let matcher = build_matcher(pattern, st.ignore_case, st.word_regexp, st.fixed_strings);

    let opts = RgOpts {
        show_line_numbers: st.show_line_numbers,
        files_only: st.files_only,
        count_only: st.count_only,
        invert_match: st.invert_match,
        glob_patterns: st.glob_patterns,
        type_filters: st.type_filters,
        after_context: st.after_context,
        before_context: st.before_context,
        no_heading: st.no_heading,
        search_hidden: st.search_hidden,
        max_count: st.max_count,
    };

    let mut found_any = false;
    let mut first_file = true;

    for search_path in &search_paths {
        let matched = rg_search_path(ctx, search_path, &matcher, &opts, &mut first_file);
        if matched {
            found_any = true;
        }
    }

    i32::from(!found_any)
}

/// Search a single path argument (file or directory) for rg matches.
fn rg_search_path(
    ctx: &mut UtilContext<'_>,
    search_path: &str,
    matcher: &Matcher,
    opts: &RgOpts,
    first_file: &mut bool,
) -> bool {
    let full = resolve_path(ctx.cwd, search_path);
    match ctx.fs.stat(&full) {
        Ok(meta) if meta.is_dir => {
            rg_search_dir(ctx, &full, search_path, matcher, opts, first_file)
        }
        Ok(_) => search_file(ctx, &full, search_path, matcher, opts, first_file),
        Err(e) => {
            let msg = format!("rg: {search_path}: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            false
        }
    }
}

/// Search all files in a directory recursively.
fn rg_search_dir(
    ctx: &mut UtilContext<'_>,
    full: &str,
    search_path: &str,
    matcher: &Matcher,
    opts: &RgOpts,
    first_file: &mut bool,
) -> bool {
    let mut files = Vec::new();
    collect_files(ctx.fs, full, opts, &mut files);
    files.sort();
    let mut found = false;
    for file_path in &files {
        let display = display_path(file_path, full, search_path);
        if search_file(ctx, file_path, &display, matcher, opts, first_file) {
            found = true;
        }
    }
    found
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
fn collect_files(fs: &BackendFs, dir: &str, opts: &RgOpts, out: &mut Vec<String>) {
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
    let type_ok = opts.type_filters.is_empty()
        || opts
            .type_filters
            .iter()
            .any(|t| type_to_extensions(t).iter().any(|ext| name.ends_with(ext)));

    let glob_ok = opts.glob_patterns.is_empty()
        || opts
            .glob_patterns
            .iter()
            .any(|p| simple_glob_match(p, name));

    type_ok && glob_ok
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
    let (match_flags, match_count) = compute_match_flags(&lines, matcher, opts);

    if match_count == 0 {
        return false;
    }

    if opts.files_only {
        let line = format!("{display}\n");
        ctx.output.stdout(line.as_bytes());
        return true;
    }

    if opts.count_only {
        let line = format!("{display}:{match_count}\n");
        ctx.output.stdout(line.as_bytes());
        return true;
    }

    let display_flags = compute_display_flags(&match_flags, opts, lines.len());

    if opts.no_heading {
        emit_no_heading(ctx, &lines, &match_flags, &display_flags, display, opts);
    } else {
        emit_heading(
            ctx,
            &lines,
            &match_flags,
            &display_flags,
            display,
            opts,
            first_file,
        );
    }

    true
}

/// Determine which lines match the pattern, respecting invert and max-count.
fn compute_match_flags(lines: &[&str], matcher: &Matcher, opts: &RgOpts) -> (Vec<bool>, usize) {
    let mut flags = vec![false; lines.len()];
    let mut count: usize = 0;
    for (i, line) in lines.iter().enumerate() {
        let hit = matcher.is_match(line) != opts.invert_match;
        if hit {
            if opts.max_count.is_some_and(|max| count >= max) {
                break;
            }
            flags[i] = true;
            count += 1;
        }
    }
    (flags, count)
}

/// Expand match flags to include before/after context lines.
fn compute_display_flags(match_flags: &[bool], opts: &RgOpts, len: usize) -> Vec<bool> {
    let mut display = vec![false; len];
    for (i, &is_match) in match_flags.iter().enumerate() {
        if is_match {
            let start = i.saturating_sub(opts.before_context);
            let end = (i + opts.after_context + 1).min(len);
            for flag in display.iter_mut().take(end).skip(start) {
                *flag = true;
            }
        }
    }
    display
}

/// Returns true when this is a non-contiguous group boundary (needs `--` separator).
fn is_group_gap(display_flags: &[bool], i: usize, prev_displayed: bool) -> bool {
    !prev_displayed && i > 0 && display_flags.iter().take(i).any(|&f| f)
}

/// Emit output lines in no-heading mode (every line prefixed with filename).
fn emit_no_heading(
    ctx: &mut UtilContext<'_>,
    lines: &[&str],
    match_flags: &[bool],
    display_flags: &[bool],
    display: &str,
    opts: &RgOpts,
) {
    let mut prev_displayed = false;
    for (i, line) in lines.iter().enumerate() {
        if !display_flags[i] {
            prev_displayed = false;
            continue;
        }
        if is_group_gap(display_flags, i, prev_displayed) {
            ctx.output.stdout(b"--\n");
        }
        let sep = if match_flags[i] { ':' } else { '-' };
        let prefix = if opts.show_line_numbers {
            format!("{display}{sep}{}{sep}", i + 1)
        } else {
            format!("{display}{sep}")
        };
        let out = format!("{prefix}{line}\n");
        ctx.output.stdout(out.as_bytes());
        prev_displayed = true;
    }
}

/// Emit output lines in heading mode (file header, then lines).
fn emit_heading(
    ctx: &mut UtilContext<'_>,
    lines: &[&str],
    match_flags: &[bool],
    display_flags: &[bool],
    display: &str,
    opts: &RgOpts,
    first_file: &mut bool,
) {
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
        if is_group_gap(display_flags, i, prev_displayed) {
            ctx.output.stdout(b"--\n");
        }
        emit_heading_line(ctx, line, i, match_flags[i], opts);
        prev_displayed = true;
    }
}

/// Emit a single line in heading mode.
fn emit_heading_line(
    ctx: &mut UtilContext<'_>,
    line: &str,
    idx: usize,
    is_match: bool,
    opts: &RgOpts,
) {
    if opts.show_line_numbers {
        let sep = if is_match { ':' } else { '-' };
        let out = format!("{}{sep}{line}\n", idx + 1);
        ctx.output.stdout(out.as_bytes());
    } else {
        ctx.output.stdout(line.as_bytes());
        ctx.output.stdout(b"\n");
    }
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
    /// Literal substring search (used for `-F` fixed-string mode).
    Literal(String),
    /// POSIX ERE regex compiled via the shared `posix-regex` engine.
    /// See ADR-0022 and ADR-0023.
    Regex(crate::regex_posix::Regex),
}

impl Matcher {
    fn is_match(&self, line: &str) -> bool {
        let haystack_owned;
        let haystack: &str = if self.ignore_case {
            haystack_owned = line.to_lowercase();
            &haystack_owned
        } else {
            line
        };

        match &self.kind {
            MatcherKind::Literal(needle) if self.word_regexp => {
                word_match_literal(haystack, needle)
            }
            MatcherKind::Literal(needle) => haystack.contains(needle.as_str()),
            MatcherKind::Regex(re) if self.word_regexp => split_words(haystack)
                .iter()
                .any(|w| regex_full_match(re, w)),
            MatcherKind::Regex(re) => re.is_match(haystack),
        }
    }
}

/// Build a matcher from the user's pattern string.
///
/// Patterns with no regex metacharacters are compiled to a literal
/// substring matcher (faster, and also the only thing that works for
/// `-F` fixed-string mode).  Otherwise we compile to POSIX ERE via
/// `posix-regex`, falling back to literal matching on pathological
/// patterns so a malformed `rg`/`fd` invocation never panics.
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
        match crate::regex_posix::Regex::compile_ere(&pattern_str) {
            Ok(re) => MatcherKind::Regex(re),
            Err(_) => MatcherKind::Literal(pattern_str),
        }
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

/// Verify that the regex matches as a full-word occurrence inside
/// `word` (which is itself already word-bounded by `split_words`).
/// A word-regex match requires the regex to accept the whole word.
fn regex_full_match(re: &crate::regex_posix::Regex, word: &str) -> bool {
    match re.find(word) {
        Some((start, end)) => start == 0 && end == word.len(),
        None => false,
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
        let advance = parse_fd_single_flag(arg, args, &mut fd);
        if advance == 0 {
            break;
        }
        args = &args[advance..];
        consumed += advance;
    }
    (fd, consumed)
}

/// Parse one fd flag. Returns number of args consumed, or 0 to stop.
fn parse_fd_single_flag(arg: &str, args: &[&str], fd: &mut FdArgs) -> usize {
    match arg {
        "-t" | "--type" if args.len() > 1 => {
            let t = args[1];
            if t == "f" || t == "d" {
                fd.type_filter = Some(t.chars().next().unwrap());
            }
            2
        }
        "-e" | "--extension" if args.len() > 1 => {
            fd.extension = Some(args[1].to_string());
            2
        }
        "-H" | "--hidden" => {
            fd.show_hidden = true;
            1
        }
        "-I" | "--no-ignore" => 1,
        "-d" | "--max-depth" if args.len() > 1 => {
            fd.max_depth = args[1].parse().ok();
            2
        }
        "-x" | "--exec" if args.len() > 1 => {
            fd.exec_cmd = Some(args[1].to_string());
            2
        }
        "-a" | "--absolute-path" => {
            fd.absolute_path = true;
            1
        }
        "-g" | "--glob" => {
            fd.glob_mode = true;
            1
        }
        "-1" => {
            fd.stop_after_first = true;
            1
        }
        _ if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") => {
            usize::from(parse_fd_bundled_flags(&arg[1..], fd))
        }
        _ => 0,
    }
}

fn parse_fd_bundled_flags(flags: &str, fd: &mut FdArgs) -> bool {
    for ch in flags.chars() {
        match ch {
            'H' => fd.show_hidden = true,
            'I' => {}
            'a' => fd.absolute_path = true,
            'g' => fd.glob_mode = true,
            '1' => fd.stop_after_first = true,
            _ => return false,
        }
    }
    true
}

fn fd_entry_matches(name: &str, is_dir: bool, fd: &FdArgs, pattern: Option<&str>) -> bool {
    if !fd_type_matches(fd.type_filter, is_dir) {
        return false;
    }
    if !fd_extension_matches(name, fd.extension.as_deref()) {
        return false;
    }
    fd_pattern_matches(name, pattern, fd.glob_mode)
}

fn fd_type_matches(type_filter: Option<char>, is_dir: bool) -> bool {
    match type_filter {
        Some('f') if is_dir => false,
        Some('d') if !is_dir => false,
        _ => true,
    }
}

fn fd_extension_matches(name: &str, extension: Option<&str>) -> bool {
    match extension {
        Some(ext) => {
            let dot_ext = format!(".{ext}");
            name.ends_with(&dot_ext)
        }
        None => true,
    }
}

fn fd_pattern_matches(name: &str, pattern: Option<&str>, glob_mode: bool) -> bool {
    match pattern {
        Some(pat) if glob_mode => simple_glob_match(pat, name),
        Some(pat) => name.contains(pat),
        None => true,
    }
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

    let count = fd_emit_results(ctx, &results, &fd_args, pattern, &search_root);

    if count == 0 && pattern.is_some() {
        return 1;
    }

    0
}

/// Emit matching fd results and return the number of matches printed.
fn fd_emit_results(
    ctx: &mut UtilContext<'_>,
    results: &[(String, bool)],
    fd_args: &FdArgs,
    pattern: Option<&str>,
    search_root: &str,
) -> usize {
    let mut count = 0;
    for (path, is_dir) in results {
        let name = path.rsplit('/').next().unwrap_or(path);
        if !fd_entry_matches(name, *is_dir, fd_args, pattern) {
            continue;
        }

        let display = fd_format_path(path, search_root, fd_args.absolute_path);
        let line = match fd_args.exec_cmd {
            Some(ref cmd) => format!("{cmd} {display}\n"),
            None => format!("{display}\n"),
        };
        ctx.output.stdout(line.as_bytes());

        count += 1;
        if fd_args.stop_after_first {
            break;
        }
    }
    count
}

/// Recursively walk the VFS collecting paths.
fn fd_walk(
    fs: &BackendFs,
    dir: &str,
    depth: usize,
    max_depth: Option<usize>,
    show_hidden: bool,
    out: &mut Vec<(String, bool)>,
) {
    if max_depth.is_some_and(|max| depth >= max) {
        return;
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
                network: None,
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
                network: None,
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
