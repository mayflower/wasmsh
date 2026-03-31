//! Text utilities: head, tail, wc, grep, sed, sort, uniq, cut, tr, tee, paste, rev, column.

use std::fmt::Write;

use wasmsh_fs::{OpenOptions, Vfs};

use crate::helpers::{emit_error, get_input_text, grep_matches, read_text, resolve_path};
use crate::{UtilContext, UtilOutput};

enum HeadMode {
    Lines(usize),
    Bytes(usize),
}

fn parse_head_args<'a>(argv: &'a [&'a str]) -> (HeadMode, bool, bool, Vec<&'a str>) {
    let mut mode = HeadMode::Lines(10);
    let mut quiet = false;
    let mut verbose = false;
    let mut files = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i];
        if arg == "-c" && i + 1 < argv.len() {
            mode = HeadMode::Bytes(argv[i + 1].parse().unwrap_or(0));
            i += 2;
        } else if arg == "-n" && i + 1 < argv.len() {
            mode = HeadMode::Lines(argv[i + 1].parse().unwrap_or(10));
            i += 2;
        } else if arg == "-q" {
            quiet = true;
            i += 1;
        } else if arg == "-v" {
            verbose = true;
            i += 1;
        } else if arg.starts_with('-') && arg.len() > 1 && arg != "--" {
            if let Ok(n) = arg[1..].parse::<usize>() {
                mode = HeadMode::Lines(n);
            }
            i += 1;
        } else {
            if arg == "--" {
                i += 1;
            }
            files.extend(argv[i..].iter().filter(|a| !a.starts_with('-')));
            break;
        }
    }
    (mode, quiet, verbose, files)
}

fn head_emit(output: &mut dyn UtilOutput, data: &[u8], mode: &HeadMode) {
    match mode {
        HeadMode::Bytes(n) => {
            let end = (*n).min(data.len());
            output.stdout(&data[..end]);
        }
        HeadMode::Lines(n) => {
            let text = String::from_utf8_lossy(data);
            for line in text.lines().take(*n) {
                output.stdout(line.as_bytes());
                output.stdout(b"\n");
            }
        }
    }
}

pub(crate) fn util_head(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (mode, quiet, verbose, files) = parse_head_args(argv);
    if files.is_empty() {
        if let Some(data) = ctx.stdin {
            head_emit(ctx.output, data, &mode);
            return 0;
        }
        ctx.output.stderr(b"head: missing operand\n");
        return 1;
    }
    let multi = files.len() > 1;
    let mut status = 0;
    for (idx, path) in files.iter().enumerate() {
        if (multi && !quiet) || verbose {
            if idx > 0 {
                ctx.output.stdout(b"\n");
            }
            let hdr = format!("==> {path} <==\n");
            ctx.output.stdout(hdr.as_bytes());
        }
        let full = resolve_path(ctx.cwd, path);
        match read_text(ctx.fs, &full) {
            Ok(text) => head_emit(ctx.output, text.as_bytes(), &mode),
            Err(e) => {
                emit_error(ctx.output, "head", path, &e);
                status = 1;
            }
        }
    }
    status
}

pub(crate) fn tail_output(text: &str, n: usize, from_start: bool, output: &mut dyn UtilOutput) {
    let lines: Vec<&str> = text.lines().collect();
    let start = if from_start {
        (n.saturating_sub(1)).min(lines.len())
    } else {
        lines.len().saturating_sub(n)
    };
    for line in &lines[start..] {
        output.stdout(line.as_bytes());
        output.stdout(b"\n");
    }
}

enum TailMode {
    Lines(usize, bool), // (count, from_start)
    Bytes(usize),
}

fn parse_tail_args<'a>(argv: &'a [&'a str]) -> (TailMode, bool, bool, Vec<&'a str>) {
    let mut quiet = false;
    let mut verbose = false;
    let mut files = Vec::new();
    let mut mode: Option<TailMode> = None;
    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i];
        if arg == "-c" && i + 1 < argv.len() {
            mode = Some(TailMode::Bytes(argv[i + 1].parse().unwrap_or(0)));
            i += 2;
        } else if arg == "-n" && i + 1 < argv.len() {
            let val = argv[i + 1];
            if let Some(rest) = val.strip_prefix('+') {
                let n = rest.parse().unwrap_or(1);
                mode = Some(TailMode::Lines(n, true));
            } else {
                let n = val.parse().unwrap_or(10);
                mode = Some(TailMode::Lines(n, false));
            }
            i += 2;
        } else if arg == "-f" {
            // accept, no-op in VFS
            i += 1;
        } else if arg == "-q" {
            quiet = true;
            i += 1;
        } else if arg == "-v" {
            verbose = true;
            i += 1;
        } else if arg.starts_with('-') && arg.len() > 1 && arg != "--" {
            if let Ok(n) = arg[1..].parse::<usize>() {
                mode = Some(TailMode::Lines(n, false));
            } else if let Some(rest) = arg.strip_prefix('+') {
                if let Ok(n) = rest.parse::<usize>() {
                    mode = Some(TailMode::Lines(n, true));
                }
            }
            i += 1;
        } else {
            if arg == "--" {
                i += 1;
            }
            files.extend(argv[i..].iter().filter(|a| !a.starts_with('-')));
            break;
        }
    }
    (
        mode.unwrap_or(TailMode::Lines(10, false)),
        quiet,
        verbose,
        files,
    )
}

fn tail_emit(output: &mut dyn UtilOutput, data: &[u8], mode: &TailMode) {
    match mode {
        TailMode::Bytes(n) => {
            let start = data.len().saturating_sub(*n);
            output.stdout(&data[start..]);
        }
        TailMode::Lines(n, from_start) => {
            let text = String::from_utf8_lossy(data);
            tail_output(&text, *n, *from_start, output);
        }
    }
}

pub(crate) fn util_tail(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (mode, quiet, verbose, files) = parse_tail_args(argv);
    if files.is_empty() {
        if let Some(data) = ctx.stdin {
            tail_emit(ctx.output, data, &mode);
            return 0;
        }
        ctx.output.stderr(b"tail: missing operand\n");
        return 1;
    }
    let multi = files.len() > 1;
    let mut status = 0;
    for (idx, path) in files.iter().enumerate() {
        if (multi && !quiet) || verbose {
            if idx > 0 {
                ctx.output.stdout(b"\n");
            }
            let hdr = format!("==> {path} <==\n");
            ctx.output.stdout(hdr.as_bytes());
        }
        let full = resolve_path(ctx.cwd, path);
        match read_text(ctx.fs, &full) {
            Ok(text) => tail_emit(ctx.output, text.as_bytes(), &mode),
            Err(e) => {
                emit_error(ctx.output, "tail", path, &e);
                status = 1;
            }
        }
    }
    status
}

pub(crate) fn util_wc(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, file_args) = parse_wc_flags(&argv[1..]);

    if file_args.is_empty() {
        if let Some(data) = ctx.stdin {
            let text = String::from_utf8_lossy(data);
            wc_emit(ctx, &text, data.len(), None, &flags);
            return 0;
        }
        ctx.output.stderr(b"wc: missing operand\n");
        return 1;
    }
    let mut status = 0;
    let mut total_lines: usize = 0;
    let mut total_words: usize = 0;
    let mut total_bytes: usize = 0;
    for path in &file_args {
        let full = resolve_path(ctx.cwd, path);
        match read_text(ctx.fs, &full) {
            Ok(text) => {
                let bytes = text.len();
                total_lines += text.lines().count();
                total_words += text.split_whitespace().count();
                total_bytes += bytes;
                wc_emit(ctx, &text, bytes, Some(path), &flags);
            }
            Err(e) => {
                emit_error(ctx.output, "wc", path, &e);
                status = 1;
            }
        }
    }
    if file_args.len() > 1 {
        wc_emit_totals(ctx, total_lines, total_words, total_bytes, &flags);
    }
    status
}

#[allow(clippy::struct_excessive_bools)]
struct WcFlags {
    lines: bool,
    words: bool,
    bytes: bool,
    max_line_length: bool,
}

fn parse_wc_flags<'a>(args: &[&'a str]) -> (WcFlags, Vec<&'a str>) {
    let mut show_lines = false;
    let mut show_words = false;
    let mut show_bytes = false;
    let mut show_max_line = false;
    let mut file_args = Vec::new();
    let mut parsing_flags = true;

    for arg in args {
        if parsing_flags && arg.starts_with('-') && arg.len() > 1 && *arg != "--" {
            for ch in arg[1..].chars() {
                match ch {
                    'l' => show_lines = true,
                    'w' => show_words = true,
                    'c' | 'm' => show_bytes = true,
                    'L' => show_max_line = true,
                    _ => {}
                }
            }
        } else {
            if *arg == "--" {
                parsing_flags = false;
                continue;
            }
            file_args.push(*arg);
        }
    }

    // If no flags specified, show all
    if !show_lines && !show_words && !show_bytes && !show_max_line {
        show_lines = true;
        show_words = true;
        show_bytes = true;
    }

    (
        WcFlags {
            lines: show_lines,
            words: show_words,
            bytes: show_bytes,
            max_line_length: show_max_line,
        },
        file_args,
    )
}

fn wc_emit(
    ctx: &mut UtilContext<'_>,
    text: &str,
    bytes: usize,
    path: Option<&str>,
    flags: &WcFlags,
) {
    let mut parts = Vec::new();
    if flags.lines {
        parts.push(format!("{:>7}", text.lines().count()));
    }
    if flags.words {
        parts.push(format!("{:>7}", text.split_whitespace().count()));
    }
    if flags.bytes {
        parts.push(format!("{bytes:>7}"));
    }
    if flags.max_line_length {
        let max_len = text.lines().map(str::len).max().unwrap_or(0);
        parts.push(format!("{max_len:>7}"));
    }
    let mut out = parts.join("");
    if let Some(p) = path {
        out.push(' ');
        out.push_str(p);
    }
    out.push('\n');
    ctx.output.stdout(out.as_bytes());
}

fn wc_emit_totals(
    ctx: &mut UtilContext<'_>,
    lines: usize,
    words: usize,
    bytes: usize,
    flags: &WcFlags,
) {
    let mut parts = Vec::new();
    if flags.lines {
        parts.push(format!("{lines:>7}"));
    }
    if flags.words {
        parts.push(format!("{words:>7}"));
    }
    if flags.bytes {
        parts.push(format!("{bytes:>7}"));
    }
    if flags.max_line_length {
        parts.push(format!("{:>7}", 0)); // total max-line not meaningful
    }
    let mut out = parts.join("");
    out.push_str(" total\n");
    ctx.output.stdout(out.as_bytes());
}

#[allow(clippy::struct_excessive_bools)]
struct GrepFlags {
    ignore_case: bool,
    invert: bool,
    count_only: bool,
    show_line_numbers: bool,
    recursive: bool,
    files_only: bool,
    word_match: bool,
    only_matching: bool,
    quiet: bool,
    extended: bool,
    fixed: bool,
    after_context: usize,
    before_context: usize,
    max_count: Option<usize>,
    show_filename: Option<bool>, // None=auto, Some(true)=always, Some(false)=never
    patterns: Vec<String>,
    include_glob: Option<String>,
    exclude_glob: Option<String>,
}

fn parse_grep_flags<'a>(argv: &'a [&'a str]) -> (GrepFlags, Vec<&'a str>) {
    let mut flags = GrepFlags {
        ignore_case: false,
        invert: false,
        count_only: false,
        show_line_numbers: false,
        recursive: false,
        files_only: false,
        word_match: false,
        only_matching: false,
        quiet: false,
        extended: false,
        fixed: false,
        after_context: 0,
        before_context: 0,
        max_count: None,
        show_filename: None,
        patterns: Vec::new(),
        include_glob: None,
        exclude_glob: None,
    };
    let mut rest = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i];
        if arg == "--" {
            rest.extend(argv[i + 1..].iter().copied());
            break;
        }
        if let Some(g) = arg.strip_prefix("--include=") {
            flags.include_glob = Some(g.to_string());
            i += 1;
            continue;
        }
        if let Some(g) = arg.strip_prefix("--exclude=") {
            flags.exclude_glob = Some(g.to_string());
            i += 1;
            continue;
        }
        if arg == "--color" || arg.starts_with("--color=") {
            i += 1;
            continue;
        }
        if arg == "-e" && i + 1 < argv.len() {
            flags.patterns.push(argv[i + 1].to_string());
            i += 2;
            continue;
        }
        if arg == "-f" && i + 1 < argv.len() {
            // pattern file - not handled here, would need fs access
            i += 2;
            continue;
        }
        if arg == "-A" && i + 1 < argv.len() {
            flags.after_context = argv[i + 1].parse().unwrap_or(0);
            i += 2;
            continue;
        }
        if arg == "-B" && i + 1 < argv.len() {
            flags.before_context = argv[i + 1].parse().unwrap_or(0);
            i += 2;
            continue;
        }
        if arg == "-C" && i + 1 < argv.len() {
            let n = argv[i + 1].parse().unwrap_or(0);
            flags.before_context = n;
            flags.after_context = n;
            i += 2;
            continue;
        }
        if arg == "-m" && i + 1 < argv.len() {
            flags.max_count = argv[i + 1].parse().ok();
            i += 2;
            continue;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            for c in arg[1..].chars() {
                match c {
                    'i' => flags.ignore_case = true,
                    'v' => flags.invert = true,
                    'c' => flags.count_only = true,
                    'n' => flags.show_line_numbers = true,
                    'r' | 'R' => flags.recursive = true,
                    'l' => flags.files_only = true,
                    'E' | 'P' => flags.extended = true,
                    'F' => flags.fixed = true,
                    'w' => flags.word_match = true,
                    'o' => flags.only_matching = true,
                    'q' => flags.quiet = true,
                    'h' => flags.show_filename = Some(false),
                    'H' => flags.show_filename = Some(true),
                    // 'z' etc. — accepted, no-op
                    _ => {}
                }
            }
            i += 1;
        } else {
            rest.push(arg);
            i += 1;
        }
    }
    (flags, rest)
}

fn grep_match_pattern(line: &str, pattern: &str, flags: &GrepFlags) -> bool {
    let (l, p) = if flags.ignore_case {
        (line.to_lowercase(), pattern.to_lowercase())
    } else {
        (line.to_string(), pattern.to_string())
    };

    if flags.extended && p.contains('|') {
        return p
            .split('|')
            .any(|alt| grep_match_single(&l, alt.trim(), flags));
    }
    grep_match_single(&l, &p, flags)
}

fn grep_match_single(line: &str, pattern: &str, flags: &GrepFlags) -> bool {
    if flags.word_match {
        for word in line.split(|c: char| !c.is_alphanumeric() && c != '_') {
            if word == pattern {
                return true;
            }
        }
        return false;
    }
    grep_matches(line, pattern, false) // already lowercased if needed
}

fn grep_find_match<'a>(line: &'a str, pattern: &str, flags: &GrepFlags) -> Option<&'a str> {
    let (l, p) = if flags.ignore_case {
        (line.to_lowercase(), pattern.to_lowercase())
    } else {
        (line.to_string(), pattern.to_string())
    };
    if flags.word_match {
        let start = l.find(&p)?;
        // Check word boundaries
        if start > 0 && l.as_bytes()[start - 1].is_ascii_alphanumeric() {
            return None;
        }
        let end = start + p.len();
        if end < l.len() && l.as_bytes()[end].is_ascii_alphanumeric() {
            return None;
        }
        Some(&line[start..start + p.len()])
    } else {
        let idx = l.find(&p)?;
        Some(&line[idx..idx + p.len()])
    }
}

fn grep_line_matches(line: &str, flags: &GrepFlags, patterns: &[&str]) -> bool {
    let matched = patterns.iter().any(|p| grep_match_pattern(line, p, flags));
    matched != flags.invert
}

fn grep_process_file(
    output: &mut dyn UtilOutput,
    text: &str,
    filename: Option<&str>,
    flags: &GrepFlags,
    patterns: &[&str],
) -> (bool, u64) {
    let lines: Vec<&str> = text.lines().collect();
    let mut match_count = 0u64;
    let mut found = false;
    let mut remaining_after = 0usize;
    let mut printed_separator = false;

    // For -B context, track which lines to print before a match
    let mut before_buf: Vec<(usize, &str)> = Vec::new();

    for (i, &line) in lines.iter().enumerate() {
        if grep_line_matches(line, flags, patterns) {
            found = true;
            match_count += 1;

            if flags.quiet || flags.files_only {
                // Don't emit lines
                if let Some(max) = flags.max_count {
                    if match_count >= max as u64 {
                        break;
                    }
                }
                continue;
            }
            if !flags.count_only {
                // Print before-context lines
                if flags.before_context > 0 && !before_buf.is_empty() {
                    if printed_separator && flags.before_context > 0 {
                        output.stdout(b"--\n");
                    }
                    for (bi, bline) in &before_buf {
                        grep_emit_one(output, bline, *bi + 1, filename, flags, patterns);
                    }
                }
                before_buf.clear();
                grep_emit_one(output, line, i + 1, filename, flags, patterns);
                remaining_after = flags.after_context;
                printed_separator = true;
            }
            if let Some(max) = flags.max_count {
                if match_count >= max as u64 {
                    break;
                }
            }
        } else if remaining_after > 0 && !flags.count_only {
            grep_emit_one(output, line, i + 1, filename, flags, patterns);
            remaining_after -= 1;
        } else if flags.before_context > 0 {
            before_buf.push((i, line));
            if before_buf.len() > flags.before_context {
                before_buf.remove(0);
            }
        }
    }

    if flags.count_only && !flags.quiet {
        if let Some(f) = filename {
            if flags.show_filename != Some(false) {
                let s = format!("{f}:{match_count}\n");
                output.stdout(s.as_bytes());
            } else {
                let s = format!("{match_count}\n");
                output.stdout(s.as_bytes());
            }
        } else {
            let s = format!("{match_count}\n");
            output.stdout(s.as_bytes());
        }
    }
    (found, match_count)
}

fn grep_emit_one(
    output: &mut dyn UtilOutput,
    line: &str,
    line_num: usize,
    filename: Option<&str>,
    flags: &GrepFlags,
    patterns: &[&str],
) {
    let mut prefix = String::new();
    if let Some(f) = filename {
        if flags.show_filename != Some(false) {
            prefix.push_str(f);
            prefix.push(':');
        }
    }
    if flags.show_line_numbers {
        let _ = write!(prefix, "{line_num}:");
    }
    if flags.only_matching {
        for pat in patterns {
            if let Some(m) = grep_find_match(line, pat, flags) {
                output.stdout(prefix.as_bytes());
                output.stdout(m.as_bytes());
                output.stdout(b"\n");
            }
        }
    } else {
        output.stdout(prefix.as_bytes());
        output.stdout(line.as_bytes());
        output.stdout(b"\n");
    }
}

fn grep_walk_recursive(
    ctx: &mut UtilContext<'_>,
    dir: &str,
    flags: &GrepFlags,
    patterns: &[&str],
    found_any: &mut bool,
) {
    let Ok(entries) = ctx.fs.read_dir(dir) else {
        return;
    };
    for entry in entries {
        let child = crate::helpers::child_path(dir, &entry.name);
        if entry.is_dir {
            grep_walk_recursive(ctx, &child, flags, patterns, found_any);
        } else {
            if let Some(ref glob) = flags.include_glob {
                if !crate::helpers::simple_glob_match(glob, &entry.name) {
                    continue;
                }
            }
            if let Some(ref glob) = flags.exclude_glob {
                if crate::helpers::simple_glob_match(glob, &entry.name) {
                    continue;
                }
            }
            if let Ok(text) = read_text(ctx.fs, &child) {
                let (found, _) =
                    grep_process_file(ctx.output, &text, Some(&child), flags, patterns);
                if found {
                    *found_any = true;
                    if flags.files_only {
                        let out = format!("{child}\n");
                        ctx.output.stdout(out.as_bytes());
                    }
                }
            }
        }
    }
}

pub(crate) fn util_grep(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, args) = parse_grep_flags(argv);

    let (pattern_strs, file_args): (Vec<String>, Vec<&str>) = if flags.patterns.is_empty() {
        if args.is_empty() {
            ctx.output.stderr(b"grep: missing pattern\n");
            return 2;
        }
        (vec![args[0].to_string()], args[1..].to_vec())
    } else {
        (flags.patterns.clone(), args)
    };
    let patterns: Vec<&str> = pattern_strs.iter().map(String::as_str).collect();

    if flags.recursive {
        let dir = if file_args.is_empty() {
            "."
        } else {
            file_args[0]
        };
        let full = resolve_path(ctx.cwd, dir);
        let mut found_any = false;
        grep_walk_recursive(ctx, &full, &flags, &patterns, &mut found_any);
        return i32::from(!found_any);
    }

    if file_args.is_empty() {
        let Some(text) = ctx
            .stdin
            .map(|data| String::from_utf8_lossy(data).to_string())
        else {
            ctx.output.stderr(b"grep: missing file operand\n");
            return 2;
        };
        let (found, _) = grep_process_file(ctx.output, &text, None, &flags, &patterns);
        return i32::from(!found);
    }

    let multi = file_args.len() > 1;
    let show_fn = flags.show_filename.unwrap_or(multi);
    let adj_flags = GrepFlags {
        show_filename: Some(show_fn),
        ..flags
    };

    let mut found_any = false;
    for path in &file_args {
        let full = resolve_path(ctx.cwd, path);
        match read_text(ctx.fs, &full) {
            Ok(text) => {
                let fname = if show_fn { Some(*path) } else { None };
                let (found, _) = grep_process_file(ctx.output, &text, fname, &adj_flags, &patterns);
                if found {
                    found_any = true;
                    if adj_flags.files_only {
                        let out = format!("{path}\n");
                        ctx.output.stdout(out.as_bytes());
                    }
                }
            }
            Err(e) => {
                emit_error(ctx.output, "grep", path, &e);
            }
        }
    }
    i32::from(!found_any)
}

pub(crate) struct SedSubstitute {
    pub(crate) pattern: String,
    pub(crate) replacement: String,
    pub(crate) global: bool,
}

pub(crate) fn parse_sed_substitute(expr: &str) -> Option<SedSubstitute> {
    if !expr.starts_with('s') || expr.len() < 4 {
        return None;
    }
    let delim = expr.as_bytes()[1];
    let rest = &expr[2..];
    let parts: Vec<&str> = rest.split(delim as char).collect();
    if parts.len() < 2 {
        return None;
    }
    let global = parts.get(2).is_some_and(|f| f.contains('g'));
    Some(SedSubstitute {
        pattern: parts[0].to_string(),
        replacement: parts[1].to_string(),
        global,
    })
}

enum SedAddr {
    None,
    Line(usize),
    Last,
    Regex(String),
    Range(Box<SedAddr>, Box<SedAddr>),
}

enum SedCmd {
    Substitute(SedSubstitute),
    Delete,
    Print,
    Transliterate(Vec<char>, Vec<char>),
    AppendText(String),
    InsertText(String),
    ChangeText(String),
    Quit,
}

struct SedInstruction {
    addr: SedAddr,
    cmd: SedCmd,
}

fn parse_sed_addr(s: &str) -> (SedAddr, &str) {
    if let Some(stripped) = s.strip_prefix('/') {
        if let Some(end) = stripped.find('/') {
            let pat = &stripped[..end];
            let rest = &stripped[end + 1..];
            if let Some(after_comma) = rest.strip_prefix(',') {
                let (addr2, rest2) = parse_sed_addr(after_comma);
                return (
                    SedAddr::Range(Box::new(SedAddr::Regex(pat.to_string())), Box::new(addr2)),
                    rest2,
                );
            }
            return (SedAddr::Regex(pat.to_string()), rest);
        }
    }
    if let Some(rest) = s.strip_prefix('$') {
        if let Some(after_comma) = rest.strip_prefix(',') {
            let (addr2, rest2) = parse_sed_addr(after_comma);
            return (
                SedAddr::Range(Box::new(SedAddr::Last), Box::new(addr2)),
                rest2,
            );
        }
        return (SedAddr::Last, rest);
    }
    let num_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if num_end > 0 {
        if let Ok(n) = s[..num_end].parse::<usize>() {
            let rest = &s[num_end..];
            if let Some(after_comma) = rest.strip_prefix(',') {
                let (addr2, rest2) = parse_sed_addr(after_comma);
                return (
                    SedAddr::Range(Box::new(SedAddr::Line(n)), Box::new(addr2)),
                    rest2,
                );
            }
            return (SedAddr::Line(n), rest);
        }
    }
    (SedAddr::None, s)
}

fn parse_sed_script(script: &str) -> Vec<SedInstruction> {
    let mut instructions = Vec::new();
    for part in script.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (addr, rest) = parse_sed_addr(part);
        let rest = rest.trim();
        let cmd = if rest.starts_with('s') {
            if let Some(sub) = parse_sed_substitute(rest) {
                SedCmd::Substitute(sub)
            } else {
                continue;
            }
        } else if rest == "d" {
            SedCmd::Delete
        } else if rest == "p" {
            SedCmd::Print
        } else if rest == "q" {
            SedCmd::Quit
        } else if rest.starts_with("y/") || rest.starts_with("y|") {
            let delim = rest.as_bytes()[1] as char;
            let inner = &rest[2..];
            let parts: Vec<&str> = inner.split(delim).collect();
            if parts.len() >= 2 {
                SedCmd::Transliterate(parts[0].chars().collect(), parts[1].chars().collect())
            } else {
                continue;
            }
        } else if let Some(text) = rest.strip_prefix("a\\") {
            SedCmd::AppendText(text.trim_start().to_string())
        } else if let Some(text) = rest.strip_prefix("i\\") {
            SedCmd::InsertText(text.trim_start().to_string())
        } else if let Some(text) = rest.strip_prefix("c\\") {
            SedCmd::ChangeText(text.trim_start().to_string())
        } else {
            continue;
        };
        instructions.push(SedInstruction { addr, cmd });
    }
    instructions
}

fn sed_addr_matches(
    addr: &SedAddr,
    line_num: usize,
    total_lines: usize,
    line: &str,
    in_range: &mut bool,
) -> bool {
    match addr {
        SedAddr::None => true,
        SedAddr::Line(n) => line_num == *n,
        SedAddr::Last => line_num == total_lines,
        SedAddr::Regex(pat) => grep_matches(line, pat, false),
        SedAddr::Range(start, end) => {
            if *in_range {
                if sed_addr_matches(end, line_num, total_lines, line, &mut false) {
                    *in_range = false;
                }
                true
            } else if sed_addr_matches(start, line_num, total_lines, line, &mut false) {
                *in_range = true;
                true
            } else {
                false
            }
        }
    }
}

pub(crate) fn util_sed(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut suppress_print = false;
    let mut in_place = false;
    let mut in_place_suffix: Option<String> = None;
    let mut expressions: Vec<String> = Vec::new();
    let mut file_args = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i];
        if arg == "-n" {
            suppress_print = true;
            i += 1;
        } else if arg == "-i" {
            in_place = true;
            i += 1;
        } else if let Some(suffix) = arg.strip_prefix("-i") {
            in_place = true;
            in_place_suffix = Some(suffix.to_string());
            i += 1;
        } else if arg == "-e" && i + 1 < argv.len() {
            expressions.push(argv[i + 1].to_string());
            i += 2;
        } else if arg == "-E" || arg == "-r" {
            i += 1; // extended regex, accept
        } else if arg == "-f" && i + 1 < argv.len() {
            // script file - read it
            let full = resolve_path(ctx.cwd, argv[i + 1]);
            if let Ok(script) = read_text(ctx.fs, &full) {
                expressions.push(script);
            }
            i += 2;
        } else if arg.starts_with('-') && arg.len() > 1 && arg != "--" {
            i += 1;
        } else {
            if arg == "--" {
                i += 1;
                file_args.extend(argv[i..].iter().copied());
                break;
            }
            if expressions.is_empty() {
                expressions.push(arg.to_string());
            } else {
                file_args.push(arg);
            }
            i += 1;
        }
    }

    if expressions.is_empty() {
        ctx.output.stderr(b"sed: missing script\n");
        return 1;
    }

    let script = expressions.join(";");
    let instructions = parse_sed_script(&script);

    let process = |text: &str, output: &mut dyn UtilOutput| -> String {
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        let mut result = String::new();
        let mut range_states: Vec<bool> = vec![false; instructions.len()];

        for (idx, &line) in lines.iter().enumerate() {
            let line_num = idx + 1;
            let mut current = line.to_string();
            let mut deleted = false;
            let mut printed = false;
            let mut quit = false;

            for (ci, instr) in instructions.iter().enumerate() {
                if !sed_addr_matches(
                    &instr.addr,
                    line_num,
                    total,
                    &current,
                    &mut range_states[ci],
                ) {
                    continue;
                }
                match &instr.cmd {
                    SedCmd::Substitute(sub) => {
                        current = if sub.global {
                            current.replace(&sub.pattern, &sub.replacement)
                        } else {
                            current.replacen(&sub.pattern, &sub.replacement, 1)
                        };
                    }
                    SedCmd::Delete => {
                        deleted = true;
                        break;
                    }
                    SedCmd::Print => {
                        output.stdout(current.as_bytes());
                        output.stdout(b"\n");
                        printed = true;
                    }
                    SedCmd::Transliterate(from, to) => {
                        current = current
                            .chars()
                            .map(|c| {
                                if let Some(pos) = from.iter().position(|&fc| fc == c) {
                                    to.get(pos).or(to.last()).copied().unwrap_or(c)
                                } else {
                                    c
                                }
                            })
                            .collect();
                    }
                    SedCmd::AppendText(text) => {
                        if !suppress_print {
                            output.stdout(current.as_bytes());
                            output.stdout(b"\n");
                        }
                        output.stdout(text.as_bytes());
                        output.stdout(b"\n");
                        printed = true;
                    }
                    SedCmd::InsertText(text) => {
                        output.stdout(text.as_bytes());
                        output.stdout(b"\n");
                    }
                    SedCmd::ChangeText(text) => {
                        output.stdout(text.as_bytes());
                        output.stdout(b"\n");
                        deleted = true;
                        break;
                    }
                    SedCmd::Quit => {
                        if !suppress_print && !printed {
                            output.stdout(current.as_bytes());
                            output.stdout(b"\n");
                        }
                        result.push_str(&current);
                        result.push('\n');
                        quit = true;
                        break;
                    }
                }
            }
            if quit {
                break;
            }
            if !deleted {
                if !suppress_print && !printed {
                    output.stdout(current.as_bytes());
                    output.stdout(b"\n");
                }
                result.push_str(&current);
                result.push('\n');
            }
        }
        result
    };

    if in_place && !file_args.is_empty() {
        for path in &file_args {
            let full = resolve_path(ctx.cwd, path);
            let text = match read_text(ctx.fs, &full) {
                Ok(t) => t,
                Err(e) => {
                    emit_error(ctx.output, "sed", path, &e);
                    return 1;
                }
            };
            if let Some(ref suffix) = in_place_suffix {
                let backup = format!("{full}{suffix}");
                let _ = crate::helpers::copy_file_contents(ctx.fs, &full, &backup);
            }
            // process without output (in-place)
            let mut dummy = SedDummyOutput;
            let result = process(&text, &mut dummy);
            // Write back
            if let Ok(h) = ctx.fs.open(&full, OpenOptions::write()) {
                let _ = ctx.fs.write_file(h, result.as_bytes());
                ctx.fs.close(h);
            }
        }
        return 0;
    }

    let text = get_input_text(ctx, &file_args);
    process(&text, ctx.output);
    0
}

struct SedDummyOutput;
impl UtilOutput for SedDummyOutput {
    fn stdout(&mut self, _data: &[u8]) {}
    fn stderr(&mut self, _data: &[u8]) {}
}

#[allow(clippy::struct_excessive_bools)]
struct SortFlags {
    numeric: bool,
    reverse: bool,
    unique: bool,
    ignore_case: bool,
    stable: bool,
    ignore_leading_blanks: bool,
    check: bool,
    human_numeric: bool,
    version_sort: bool,
    key_field: Option<usize>,
    separator: Option<char>,
    output_file: Option<String>,
}

fn parse_sort_flags<'a>(argv: &'a [&'a str]) -> (SortFlags, Vec<&'a str>) {
    let mut flags = SortFlags {
        numeric: false,
        reverse: false,
        unique: false,
        ignore_case: false,
        stable: false,
        ignore_leading_blanks: false,
        check: false,
        human_numeric: false,
        version_sort: false,
        key_field: None,
        separator: None,
        output_file: None,
    };
    let mut file_args = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i];
        if arg == "-k" && i + 1 < argv.len() {
            // Parse key spec: "-k 2" or "-k 2,3"
            let spec = argv[i + 1];
            let field: &str = spec.split(',').next().unwrap_or(spec);
            let field: &str = field.split('.').next().unwrap_or(field);
            flags.key_field = field.parse().ok();
            i += 2;
        } else if arg == "-t" && i + 1 < argv.len() {
            flags.separator = argv[i + 1].chars().next();
            i += 2;
        } else if arg == "-o" && i + 1 < argv.len() {
            flags.output_file = Some(argv[i + 1].to_string());
            i += 2;
        } else if arg.starts_with('-') && arg.len() > 1 && arg != "--" {
            for c in arg[1..].chars() {
                match c {
                    'n' => flags.numeric = true,
                    'r' => flags.reverse = true,
                    'u' => flags.unique = true,
                    'f' => flags.ignore_case = true,
                    's' => flags.stable = true,
                    'b' => flags.ignore_leading_blanks = true,
                    'c' => flags.check = true,
                    'h' => flags.human_numeric = true,
                    'V' => flags.version_sort = true,
                    // 'm', 'z' etc. — accept, no special handling
                    _ => {}
                }
            }
            i += 1;
        } else {
            if arg == "--" {
                i += 1;
            }
            file_args.extend(argv[i..].iter().filter(|a| !a.starts_with('-')).copied());
            break;
        }
    }
    (flags, file_args)
}

fn sort_extract_key<'a>(line: &'a str, flags: &SortFlags) -> &'a str {
    if let Some(field_num) = flags.key_field {
        if field_num == 0 {
            return line;
        }
        let parts: Vec<&str> = if let Some(sep) = flags.separator {
            line.split(sep).collect()
        } else {
            line.split_whitespace().collect()
        };
        if field_num <= parts.len() {
            return parts[field_num - 1];
        }
        ""
    } else {
        line
    }
}

fn sort_compare(a: &str, b: &str, flags: &SortFlags) -> std::cmp::Ordering {
    let ka = sort_extract_key(a, flags);
    let kb = sort_extract_key(b, flags);
    let ka = if flags.ignore_leading_blanks {
        ka.trim_start()
    } else {
        ka
    };
    let kb = if flags.ignore_leading_blanks {
        kb.trim_start()
    } else {
        kb
    };

    if flags.numeric || flags.human_numeric {
        let na = ka.trim().parse::<f64>().unwrap_or(0.0);
        let nb = kb.trim().parse::<f64>().unwrap_or(0.0);
        na.partial_cmp(&nb).unwrap_or(std::cmp::Ordering::Equal)
    } else if flags.version_sort {
        version_compare(ka, kb)
    } else if flags.ignore_case {
        ka.to_lowercase().cmp(&kb.to_lowercase())
    } else {
        ka.cmp(kb)
    }
}

fn version_compare(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek(), bi.peek()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(&ac), Some(&bc)) => {
                if ac.is_ascii_digit() && bc.is_ascii_digit() {
                    let na: String = ai.by_ref().take_while(char::is_ascii_digit).collect();
                    let nb: String = bi.by_ref().take_while(char::is_ascii_digit).collect();
                    let cmp = na
                        .parse::<u64>()
                        .unwrap_or(0)
                        .cmp(&nb.parse::<u64>().unwrap_or(0));
                    if cmp != std::cmp::Ordering::Equal {
                        return cmp;
                    }
                } else {
                    let cmp = ac.cmp(&bc);
                    if cmp != std::cmp::Ordering::Equal {
                        return cmp;
                    }
                    ai.next();
                    bi.next();
                }
            }
        }
    }
}

pub(crate) fn util_sort(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, file_args) = parse_sort_flags(argv);
    let text = get_input_text(ctx, &file_args);
    let mut lines: Vec<&str> = text.lines().collect();

    if flags.check {
        for w in lines.windows(2) {
            let ord = sort_compare(w[0], w[1], &flags);
            if (flags.reverse && ord == std::cmp::Ordering::Less)
                || (!flags.reverse && ord == std::cmp::Ordering::Greater)
            {
                let msg = format!("sort: disorder: {}\n", w[1]);
                ctx.output.stderr(msg.as_bytes());
                return 1;
            }
        }
        return 0;
    }

    if flags.stable {
        lines.sort_by(|a, b| sort_compare(a, b, &flags));
    } else {
        lines.sort_unstable_by(|a, b| sort_compare(a, b, &flags));
    }
    if flags.reverse {
        lines.reverse();
    }
    if flags.unique {
        lines.dedup_by(|a, b| {
            if flags.ignore_case {
                a.to_lowercase() == b.to_lowercase()
            } else {
                a == b
            }
        });
    }

    if let Some(ref path) = flags.output_file {
        let full = resolve_path(ctx.cwd, path);
        let mut out = String::new();
        for line in &lines {
            out.push_str(line);
            out.push('\n');
        }
        if let Ok(h) = ctx.fs.open(&full, OpenOptions::write()) {
            let _ = ctx.fs.write_file(h, out.as_bytes());
            ctx.fs.close(h);
        }
    } else {
        for line in &lines {
            ctx.output.stdout(line.as_bytes());
            ctx.output.stdout(b"\n");
        }
    }
    0
}

#[allow(clippy::struct_excessive_bools)]
struct UniqFlags {
    count: bool,
    duplicates_only: bool,
    unique_only: bool,
    ignore_case: bool,
    skip_fields: usize,
    skip_chars: usize,
    compare_chars: Option<usize>,
}

fn parse_uniq_flags<'a>(argv: &'a [&'a str]) -> (UniqFlags, Vec<&'a str>) {
    let mut flags = UniqFlags {
        count: false,
        duplicates_only: false,
        unique_only: false,
        ignore_case: false,
        skip_fields: 0,
        skip_chars: 0,
        compare_chars: None,
    };
    let mut file_args = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i];
        if arg == "-f" && i + 1 < argv.len() {
            flags.skip_fields = argv[i + 1].parse().unwrap_or(0);
            i += 2;
        } else if arg == "-s" && i + 1 < argv.len() {
            flags.skip_chars = argv[i + 1].parse().unwrap_or(0);
            i += 2;
        } else if arg == "-w" && i + 1 < argv.len() {
            flags.compare_chars = argv[i + 1].parse().ok();
            i += 2;
        } else if arg.starts_with('-') && arg.len() > 1 && arg != "--" {
            for c in arg[1..].chars() {
                match c {
                    'c' => flags.count = true,
                    'd' => flags.duplicates_only = true,
                    'u' => flags.unique_only = true,
                    'i' => flags.ignore_case = true,
                    // 'z' etc. — accept, no-op
                    _ => {}
                }
            }
            i += 1;
        } else {
            file_args.push(arg);
            i += 1;
        }
    }
    (flags, file_args)
}

fn uniq_compare_key<'a>(line: &'a str, flags: &UniqFlags) -> String {
    let mut s = line;
    // Skip fields
    for _ in 0..flags.skip_fields {
        s = s.trim_start();
        if let Some(pos) = s.find(char::is_whitespace) {
            s = &s[pos..];
        } else {
            s = "";
            break;
        }
    }
    // Skip chars
    if flags.skip_chars > 0 {
        let chars: Vec<char> = s.chars().collect();
        s = if flags.skip_chars < chars.len() {
            &s[chars[..flags.skip_chars]
                .iter()
                .map(|c| c.len_utf8())
                .sum::<usize>()..]
        } else {
            ""
        };
    }
    let mut key = s.to_string();
    // Limit compare chars
    if let Some(w) = flags.compare_chars {
        let truncated: String = key.chars().take(w).collect();
        key = truncated;
    }
    if flags.ignore_case {
        key = key.to_lowercase();
    }
    key
}

pub(crate) fn util_uniq(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, file_args) = parse_uniq_flags(argv);
    let text = get_input_text(ctx, &file_args);

    let mut prev: Option<(String, String)> = None; // (original_line, compare_key)
    let mut cnt: usize = 0;

    let emit = |output: &mut dyn UtilOutput, line: &str, n: usize, flags: &UniqFlags| {
        if flags.duplicates_only && n < 2 {
            return;
        }
        if flags.unique_only && n > 1 {
            return;
        }
        if flags.count {
            let s = format!("{n:>7} {line}\n");
            output.stdout(s.as_bytes());
        } else {
            output.stdout(line.as_bytes());
            output.stdout(b"\n");
        }
    };

    for line in text.lines() {
        let key = uniq_compare_key(line, &flags);
        if prev.as_ref().is_some_and(|(_, k)| *k == key) {
            cnt += 1;
        } else {
            if let Some((ref p, _)) = prev {
                emit(ctx.output, p, cnt, &flags);
            }
            prev = Some((line.to_string(), key));
            cnt = 1;
        }
    }
    if let Some((ref p, _)) = prev {
        emit(ctx.output, p, cnt, &flags);
    }
    0
}

enum CutMode {
    Fields(Vec<CutRange>),
    Chars(Vec<CutRange>),
    Bytes(Vec<CutRange>),
}

#[derive(Clone)]
struct CutRange {
    start: Option<usize>, // None = from beginning
    end: Option<usize>,   // None = to end
}

fn parse_cut_ranges(spec: &str) -> Vec<CutRange> {
    spec.split(',')
        .filter_map(|s| {
            if let Some((a, b)) = s.split_once('-') {
                Some(CutRange {
                    start: if a.is_empty() { None } else { a.parse().ok() },
                    end: if b.is_empty() { None } else { b.parse().ok() },
                })
            } else {
                let n: usize = s.parse().ok()?;
                Some(CutRange {
                    start: Some(n),
                    end: Some(n),
                })
            }
        })
        .collect()
}

fn cut_range_includes(ranges: &[CutRange], idx: usize) -> bool {
    ranges.iter().any(|r| {
        let start = r.start.unwrap_or(1);
        let end = r.end.unwrap_or(usize::MAX);
        idx >= start && idx <= end
    })
}

pub(crate) fn util_cut(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut delim = '\t';
    let mut mode: Option<CutMode> = None;
    let mut complement = false;
    let mut only_delimited = false;
    let mut output_delim: Option<String> = None;
    let mut file_args = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i];
        if arg == "-d" && i + 1 < argv.len() {
            delim = argv[i + 1].chars().next().unwrap_or('\t');
            i += 2;
        } else if arg == "-f" && i + 1 < argv.len() {
            mode = Some(CutMode::Fields(parse_cut_ranges(argv[i + 1])));
            i += 2;
        } else if arg == "-c" && i + 1 < argv.len() {
            mode = Some(CutMode::Chars(parse_cut_ranges(argv[i + 1])));
            i += 2;
        } else if arg == "-b" && i + 1 < argv.len() {
            mode = Some(CutMode::Bytes(parse_cut_ranges(argv[i + 1])));
            i += 2;
        } else if arg == "--complement" {
            complement = true;
            i += 1;
        } else if arg == "-s" {
            only_delimited = true;
            i += 1;
        } else if let Some(od) = arg.strip_prefix("--output-delimiter=") {
            output_delim = Some(od.to_string());
            i += 1;
        } else if arg == "-z" {
            i += 1; // accept, no-op
        } else {
            file_args.push(arg);
            i += 1;
        }
    }

    let Some(mode) = mode else {
        ctx.output
            .stderr(b"cut: you must specify a list of bytes, characters, or fields\n");
        return 1;
    };
    let out_sep = output_delim.unwrap_or_else(|| delim.to_string());
    let text = get_input_text(ctx, &file_args);

    for line in text.lines() {
        match &mode {
            CutMode::Fields(ranges) => {
                if only_delimited && !line.contains(delim) {
                    continue;
                }
                let parts: Vec<&str> = line.split(delim).collect();
                let selected: Vec<&str> = parts
                    .iter()
                    .enumerate()
                    .filter(|(idx, _)| {
                        let included = cut_range_includes(ranges, idx + 1);
                        if complement {
                            !included
                        } else {
                            included
                        }
                    })
                    .map(|(_, s)| *s)
                    .collect();
                ctx.output.stdout(selected.join(&out_sep).as_bytes());
                ctx.output.stdout(b"\n");
            }
            CutMode::Chars(ranges) | CutMode::Bytes(ranges) => {
                let chars: Vec<char> = line.chars().collect();
                let selected: String = chars
                    .iter()
                    .enumerate()
                    .filter(|(idx, _)| {
                        let included = cut_range_includes(ranges, idx + 1);
                        if complement {
                            !included
                        } else {
                            included
                        }
                    })
                    .map(|(_, c)| *c)
                    .collect();
                ctx.output.stdout(selected.as_bytes());
                ctx.output.stdout(b"\n");
            }
        }
    }
    0
}

fn tr_expand_set(s: &str) -> Vec<char> {
    let mut chars = Vec::new();
    let mut iter = s.chars().peekable();
    while let Some(ch) = iter.next() {
        if ch == '[' && iter.peek() == Some(&':') {
            // Character class [:name:]
            iter.next(); // consume ':'
            let class_name: String = iter.by_ref().take_while(|&c| c != ':').collect();
            let _ = iter.next(); // consume ']'
            match class_name.as_str() {
                "upper" => chars.extend('A'..='Z'),
                "lower" => chars.extend('a'..='z'),
                "digit" => chars.extend('0'..='9'),
                "alpha" => {
                    chars.extend('A'..='Z');
                    chars.extend('a'..='z');
                }
                "alnum" => {
                    chars.extend('0'..='9');
                    chars.extend('A'..='Z');
                    chars.extend('a'..='z');
                }
                "space" => chars.extend([' ', '\t', '\n', '\r', '\x0b', '\x0c']),
                "blank" => chars.extend([' ', '\t']),
                "punct" => {
                    chars.extend("!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~".chars());
                }
                _ => {}
            }
        } else if iter.peek() == Some(&'-') {
            // Character range: a-z
            let saved = iter.clone();
            iter.next(); // consume '-'
            if let Some(&end_ch) = iter.peek() {
                if end_ch > ch {
                    chars.extend(ch..=end_ch);
                    iter.next(); // consume end char
                } else {
                    chars.push(ch);
                    // put back '-'
                    iter = saved;
                    iter.next(); // skip '-'
                    chars.push('-');
                }
            } else {
                chars.push(ch);
                chars.push('-');
            }
        } else if ch == '\\' {
            match iter.next() {
                Some('n') => chars.push('\n'),
                Some('t') => chars.push('\t'),
                Some('r') => chars.push('\r'),
                Some('\\') | None => chars.push('\\'),
                Some(c) => chars.push(c),
            }
        } else {
            chars.push(ch);
        }
    }
    chars
}

pub(crate) fn util_tr(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut delete = false;
    let mut squeeze = false;
    let mut complement = false;
    let mut set_args: Vec<&str> = Vec::new();

    for arg in &argv[1..] {
        if arg.starts_with('-') && arg.len() > 1 {
            for ch in arg[1..].chars() {
                match ch {
                    'd' => delete = true,
                    's' => squeeze = true,
                    'c' | 'C' => complement = true,
                    // 't' (truncate) etc. — accepted
                    _ => {}
                }
            }
        } else {
            set_args.push(arg);
        }
    }

    let text = if let Some(data) = ctx.stdin {
        String::from_utf8_lossy(data).to_string()
    } else {
        ctx.output.stderr(b"tr: missing operand\n");
        return 1;
    };

    if set_args.is_empty() {
        ctx.output.stderr(b"tr: missing operand\n");
        return 1;
    }

    let from_chars = tr_expand_set(set_args[0]);

    if delete && squeeze && set_args.len() >= 2 {
        // -ds: delete chars in SET1, then squeeze chars in SET2
        let squeeze_chars = tr_expand_set(set_args[1]);
        let after_delete: String = text
            .chars()
            .filter(|c| {
                let in_set = from_chars.contains(c);
                if complement {
                    in_set
                } else {
                    !in_set
                }
            })
            .collect();
        let mut result = String::new();
        let mut prev: Option<char> = None;
        for c in after_delete.chars() {
            if squeeze_chars.contains(&c) && prev == Some(c) {
                continue;
            }
            result.push(c);
            prev = Some(c);
        }
        ctx.output.stdout(result.as_bytes());
        return 0;
    }

    if delete {
        let result: String = text
            .chars()
            .filter(|c| {
                let in_set = from_chars.contains(c);
                if complement {
                    in_set
                } else {
                    !in_set
                }
            })
            .collect();
        ctx.output.stdout(result.as_bytes());
        return 0;
    }

    if squeeze && set_args.len() < 2 {
        // Squeeze only — squeeze repeated chars from SET1
        let mut result = String::new();
        let mut prev: Option<char> = None;
        for c in text.chars() {
            if from_chars.contains(&c) && prev == Some(c) {
                continue;
            }
            result.push(c);
            prev = Some(c);
        }
        ctx.output.stdout(result.as_bytes());
        return 0;
    }

    if set_args.len() < 2 {
        ctx.output.stderr(b"tr: missing operand\n");
        return 1;
    }

    let to_chars = tr_expand_set(set_args[1]);
    let from_set = if complement {
        // Build complement: all ASCII chars not in from_chars
        (0u8..=127)
            .map(|b| b as char)
            .filter(|c| !from_chars.contains(c))
            .collect()
    } else {
        from_chars.clone()
    };

    let mut result = String::new();
    let mut prev: Option<char> = None;
    for c in text.chars() {
        let translated = if let Some(pos) = from_set.iter().position(|&fc| fc == c) {
            to_chars.get(pos).or(to_chars.last()).copied().unwrap_or(c)
        } else {
            c
        };
        if squeeze && to_chars.contains(&translated) && prev == Some(translated) {
            continue;
        }
        result.push(translated);
        prev = Some(translated);
    }
    ctx.output.stdout(result.as_bytes());
    0
}

pub(crate) fn util_tee(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut append = false;
    if args.first() == Some(&"-a") {
        append = true;
        args = &args[1..];
    }
    let data = if let Some(d) = ctx.stdin {
        d.to_vec()
    } else {
        Vec::new()
    };
    ctx.output.stdout(&data);
    let mut status = 0;
    for path in args {
        let full = resolve_path(ctx.cwd, path);
        let opts = if append {
            OpenOptions::append()
        } else {
            OpenOptions::write()
        };
        match ctx.fs.open(&full, opts) {
            Ok(h) => {
                if let Err(e) = ctx.fs.write_file(h, &data) {
                    emit_error(ctx.output, "tee", path, &e);
                    status = 1;
                }
                ctx.fs.close(h);
            }
            Err(e) => {
                emit_error(ctx.output, "tee", path, &e);
                status = 1;
            }
        }
    }
    status
}

struct PasteFlags {
    delimiter: String,
    serial: bool,
}

fn parse_paste_flags<'a>(argv: &'a [&'a str]) -> (PasteFlags, &'a [&'a str]) {
    let mut args = &argv[1..];
    let mut flags = PasteFlags {
        delimiter: "\t".to_string(),
        serial: false,
    };

    while let Some(arg) = args.first() {
        if *arg == "-d" && args.len() > 1 {
            flags.delimiter = args[1].to_string();
            args = &args[2..];
        } else if *arg == "-s" {
            flags.serial = true;
            args = &args[1..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            if !parse_paste_bundled(arg, args, &mut flags) {
                break;
            }
            args = &args[1..];
        } else {
            break;
        }
    }
    (flags, args)
}

/// Parse bundled paste flags like `-sd`. Returns `false` if an unknown flag was found.
fn parse_paste_bundled<'a>(arg: &str, args: &[&'a str], flags: &mut PasteFlags) -> bool {
    for c in arg[1..].chars() {
        match c {
            's' => flags.serial = true,
            'd' => {
                if args.len() > 1 {
                    flags.delimiter = args[1].to_string();
                }
            }
            _ => return false,
        }
    }
    true
}

fn paste_read_files(ctx: &mut UtilContext<'_>, args: &[&str]) -> Result<Vec<Vec<String>>, i32> {
    let mut file_lines: Vec<Vec<String>> = Vec::new();
    for path in args {
        let lines = if *path == "-" {
            let text = ctx
                .stdin
                .map(|d| String::from_utf8_lossy(d).to_string())
                .unwrap_or_default();
            text.lines().map(String::from).collect()
        } else {
            let full = resolve_path(ctx.cwd, path);
            match read_text(ctx.fs, &full) {
                Ok(text) => text.lines().map(String::from).collect(),
                Err(e) => {
                    emit_error(ctx.output, "paste", path, &e);
                    return Err(1);
                }
            }
        };
        file_lines.push(lines);
    }
    Ok(file_lines)
}

fn paste_serial(ctx: &mut UtilContext<'_>, file_lines: &[Vec<String>], delimiter: &str) {
    for lines in file_lines {
        let joined = lines.join(delimiter);
        ctx.output.stdout(joined.as_bytes());
        ctx.output.stdout(b"\n");
    }
}

fn paste_merge(ctx: &mut UtilContext<'_>, file_lines: &[Vec<String>], delimiter: &str) {
    let max_lines = file_lines.iter().map(Vec::len).max().unwrap_or(0);
    for i in 0..max_lines {
        for (fi, lines) in file_lines.iter().enumerate() {
            if fi > 0 {
                ctx.output.stdout(delimiter.as_bytes());
            }
            if let Some(line) = lines.get(i) {
                ctx.output.stdout(line.as_bytes());
            }
        }
        ctx.output.stdout(b"\n");
    }
}

pub(crate) fn util_paste(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, args) = parse_paste_flags(argv);

    if args.is_empty() {
        let Some(data) = ctx.stdin else {
            ctx.output.stderr(b"paste: missing operand\n");
            return 1;
        };
        let text = String::from_utf8_lossy(data);
        ctx.output.stdout(text.as_bytes());
        if !text.ends_with('\n') {
            ctx.output.stdout(b"\n");
        }
        return 0;
    }

    let file_lines = match paste_read_files(ctx, args) {
        Ok(fl) => fl,
        Err(status) => return status,
    };

    if flags.serial {
        paste_serial(ctx, &file_lines, &flags.delimiter);
    } else {
        paste_merge(ctx, &file_lines, &flags.delimiter);
    }
    0
}

pub(crate) fn util_rev(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let file_args = &argv[1..];
    let text = get_input_text(ctx, file_args);
    if text.is_empty() && file_args.is_empty() && ctx.stdin.is_none() {
        ctx.output.stderr(b"rev: missing operand\n");
        return 1;
    }
    for line in text.lines() {
        let reversed: String = line.chars().rev().collect();
        ctx.output.stdout(reversed.as_bytes());
        ctx.output.stdout(b"\n");
    }
    0
}

// ---------------------------------------------------------------------------
// bat — cat with line numbers and file header
// ---------------------------------------------------------------------------

struct BatFlags {
    show_numbers: bool,
    show_header: bool,
    line_range: Option<(Option<usize>, Option<usize>)>,
    show_all: bool,
}

fn parse_bat_flags(argv: &[&str]) -> (BatFlags, usize) {
    let mut args = &argv[1..];
    let mut flags = BatFlags {
        show_numbers: true,
        show_header: true,
        line_range: None,
        show_all: false,
    };
    let mut consumed = 1;

    while let Some(arg) = args.first() {
        match *arg {
            "-n" | "--number" => flags.show_numbers = true,
            "-p" | "--plain" => {
                flags.show_numbers = false;
                flags.show_header = false;
            }
            "-A" | "--show-all" => flags.show_all = true,
            "-r" | "--line-range" if args.len() > 1 => {
                flags.line_range = parse_bat_range(args[1]);
                args = &args[2..];
                consumed += 2;
                continue;
            }
            "-l" | "--language" | "--paging" if args.len() > 1 => {
                args = &args[2..];
                consumed += 2;
                continue;
            }
            _ if arg.starts_with("--style=") => {
                apply_bat_style(&mut flags, &arg["--style=".len()..]);
            }
            _ if arg.starts_with("--line-range=") => {
                flags.line_range = parse_bat_range(&arg["--line-range=".len()..]);
            }
            _ if arg.starts_with("--paging=") | arg.starts_with("--language=") => {}
            _ if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") => {
                if !apply_bat_bundled_flags(&mut flags, arg) {
                    break;
                }
            }
            _ => break,
        }
        args = &args[1..];
        consumed += 1;
    }
    (flags, consumed)
}

/// Try to apply bundled single-char flags like `-npA`. Returns false if an
/// unrecognized character is encountered (caller should break out of parsing).
fn apply_bat_bundled_flags(flags: &mut BatFlags, arg: &str) -> bool {
    for ch in arg[1..].chars() {
        match ch {
            'n' => flags.show_numbers = true,
            'p' => {
                flags.show_numbers = false;
                flags.show_header = false;
            }
            'A' => flags.show_all = true,
            _ => return false,
        }
    }
    true
}

fn apply_bat_style(flags: &mut BatFlags, style: &str) {
    match style {
        "plain" => {
            flags.show_numbers = false;
            flags.show_header = false;
        }
        "numbers" => {
            flags.show_numbers = true;
            flags.show_header = false;
        }
        "header" => {
            flags.show_numbers = false;
            flags.show_header = true;
        }
        _ => {
            flags.show_numbers = true;
            flags.show_header = true;
        }
    }
}

pub(crate) fn util_bat(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, consumed) = parse_bat_flags(argv);
    let file_args: Vec<&str> = argv[consumed..].to_vec();

    if file_args.is_empty() {
        if let Some(data) = ctx.stdin {
            let text = String::from_utf8_lossy(data).to_string();
            bat_output(
                ctx,
                None,
                &text,
                flags.show_numbers,
                flags.show_header,
                flags.line_range,
                flags.show_all,
            );
            return 0;
        }
        ctx.output.stderr(b"bat: missing operand\n");
        return 1;
    }

    let mut status = 0;
    for path in &file_args {
        let full = resolve_path(ctx.cwd, path);
        match read_text(ctx.fs, &full) {
            Ok(text) => {
                bat_output(
                    ctx,
                    Some(path),
                    &text,
                    flags.show_numbers,
                    flags.show_header,
                    flags.line_range,
                    flags.show_all,
                );
            }
            Err(e) => {
                emit_error(ctx.output, "bat", path, &e);
                status = 1;
            }
        }
    }

    status
}

fn parse_bat_range(s: &str) -> Option<(Option<usize>, Option<usize>)> {
    if let Some((start, end)) = s.split_once(':') {
        let s = if start.is_empty() {
            None
        } else {
            start.parse().ok()
        };
        let e = if end.is_empty() {
            None
        } else {
            end.parse().ok()
        };
        Some((s, e))
    } else {
        // Single line
        let n: usize = s.parse().ok()?;
        Some((Some(n), Some(n)))
    }
}

fn bat_in_range(line_num: usize, range: Option<(Option<usize>, Option<usize>)>) -> bool {
    let Some((start, end)) = range else {
        return true;
    };
    if start.is_some_and(|s| line_num < s) {
        return false;
    }
    end.is_none_or(|e| line_num <= e)
}

fn bat_emit_chrome(
    ctx: &mut UtilContext<'_>,
    filename: Option<&str>,
    rule_left: &str,
    rule_right: &str,
) {
    let top_corner = "\u{252C}";
    let mid_corner = "\u{253C}";
    let vert = "\u{2502}";

    let header_line = format!("{rule_left}{top_corner}{rule_right}\n");
    ctx.output.stdout(header_line.as_bytes());
    if let Some(name) = filename {
        let file_line = format!("       {vert} File: {name}\n");
        ctx.output.stdout(file_line.as_bytes());
    }
    let sep_line = format!("{rule_left}{mid_corner}{rule_right}\n");
    ctx.output.stdout(sep_line.as_bytes());
}

fn bat_output(
    ctx: &mut UtilContext<'_>,
    filename: Option<&str>,
    text: &str,
    show_numbers: bool,
    show_header: bool,
    line_range: Option<(Option<usize>, Option<usize>)>,
    show_all: bool,
) {
    let separator = "\u{2500}";
    let vert = "\u{2502}";
    let rule_left: String = separator.repeat(7);
    let rule_right: String = separator.repeat(20);

    if show_header {
        bat_emit_chrome(ctx, filename, &rule_left, &rule_right);
    }

    for (i, line) in text.lines().enumerate() {
        let line_num = i + 1;
        if !bat_in_range(line_num, line_range) {
            continue;
        }

        let display_line = if show_all {
            make_visible(line)
        } else {
            line.to_string()
        };

        let out = if show_numbers {
            format!("{line_num:>5}   {vert} {display_line}\n")
        } else {
            format!("{display_line}\n")
        };
        ctx.output.stdout(out.as_bytes());
    }

    if show_header {
        let bot_corner = "\u{2534}";
        let footer = format!("{rule_left}{bot_corner}{rule_right}\n");
        ctx.output.stdout(footer.as_bytes());
    }
}

/// Replace non-printable characters with visible representations.
fn make_visible(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch == '\t' {
            out.push_str("\\t");
        } else if ch == '\r' {
            out.push_str("\\r");
        } else if ch.is_control() {
            let _ = write!(out, "\\x{:02x}", ch as u32);
        } else {
            out.push(ch);
        }
    }
    out
}

struct ColumnFlags {
    table_mode: bool,
    input_delim: Option<String>,
}

fn parse_column_flags<'a>(argv: &'a [&'a str]) -> (ColumnFlags, &'a [&'a str]) {
    let mut args = &argv[1..];
    let mut flags = ColumnFlags {
        table_mode: false,
        input_delim: None,
    };

    while let Some(arg) = args.first() {
        if *arg == "-t" {
            flags.table_mode = true;
            args = &args[1..];
        } else if *arg == "-s" && args.len() > 1 {
            flags.input_delim = Some(args[1].to_string());
            args = &args[2..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            args = &args[1..];
        } else {
            break;
        }
    }
    (flags, args)
}

fn column_table_output(ctx: &mut UtilContext<'_>, text: &str, input_delim: Option<&String>) {
    let rows = column_rows(text, input_delim);

    if rows.is_empty() {
        return;
    }

    let col_widths = column_widths(&rows);

    for row in &rows {
        let mut line = column_format_row(row, &col_widths);
        line.push('\n');
        ctx.output.stdout(line.as_bytes());
    }
}

fn column_rows<'a>(text: &'a str, input_delim: Option<&String>) -> Vec<Vec<&'a str>> {
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| match input_delim {
            Some(delim) => line.split(delim.as_str()).collect(),
            None => line.split_whitespace().collect(),
        })
        .collect()
}

fn column_widths(rows: &[Vec<&str>]) -> Vec<usize> {
    let max_cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut col_widths = vec![0usize; max_cols];
    for row in rows {
        for (i, field) in row.iter().enumerate() {
            col_widths[i] = col_widths[i].max(field.len());
        }
    }
    col_widths
}

fn column_format_row(row: &[&str], col_widths: &[usize]) -> String {
    let mut line = String::new();
    for (i, field) in row.iter().enumerate() {
        if i > 0 {
            line.push_str("  ");
        }
        line.push_str(field);
        if i + 1 < row.len() {
            line.push_str(&" ".repeat(col_widths[i].saturating_sub(field.len())));
        }
    }
    line
}

pub(crate) fn util_column(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, args) = parse_column_flags(argv);
    let text = get_input_text(ctx, args);
    if text.is_empty() {
        return 0;
    }

    if flags.table_mode {
        column_table_output(ctx, &text, flags.input_delim.as_ref());
    } else {
        ctx.output.stdout(text.as_bytes());
        if !text.ends_with('\n') {
            ctx.output.stdout(b"\n");
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UtilContext, VecOutput};
    use wasmsh_fs::{MemoryFs, OpenOptions, Vfs};

    fn make_fs() -> MemoryFs {
        MemoryFs::new()
    }

    fn make_fs_with_file(path: &str, content: &[u8]) -> MemoryFs {
        let mut fs = MemoryFs::new();
        let h = fs.open(path, OpenOptions::write()).unwrap();
        fs.write_file(h, content).unwrap();
        fs.close(h);
        fs
    }

    fn run(
        f: fn(&mut UtilContext<'_>, &[&str]) -> i32,
        argv: &[&str],
        fs: &mut MemoryFs,
    ) -> (i32, String, String) {
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs,
                output: &mut output,
                cwd: "/",
                stdin: None,
                state: None,
                network: None,
            };
            f(&mut ctx, argv)
        };
        (
            status,
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )
    }

    fn run_stdin(
        f: fn(&mut UtilContext<'_>, &[&str]) -> i32,
        argv: &[&str],
        stdin: &[u8],
        fs: &mut MemoryFs,
    ) -> (i32, String, String) {
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs,
                output: &mut output,
                cwd: "/",
                stdin: Some(stdin),
                state: None,
                network: None,
            };
            f(&mut ctx, argv)
        };
        (
            status,
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )
    }

    #[test]
    fn bat_full_style() {
        let mut fs = make_fs_with_file("/code.rs", b"fn main() {\n    println!(\"hi\");\n}\n");
        let (status, out, _) = run(util_bat, &["bat", "/code.rs"], &mut fs);
        assert_eq!(status, 0);
        // Should contain box-drawing characters for header
        assert!(out.contains('\u{2500}'), "expected horizontal rule char");
        assert!(out.contains('\u{252C}'), "expected top corner char");
        assert!(out.contains("File: /code.rs"), "expected file header");
        // Should contain line numbers
        assert!(out.contains('1'), "expected line number 1");
    }

    #[test]
    fn bat_plain() {
        let mut fs = make_fs_with_file("/plain.txt", b"line one\nline two\n");
        let (status, out, _) = run(util_bat, &["bat", "-p", "/plain.txt"], &mut fs);
        assert_eq!(status, 0);
        // Plain mode: no box-drawing, no header
        assert!(!out.contains('\u{2500}'), "should not contain decoration");
        assert!(!out.contains("File:"), "should not contain file header");
        assert!(out.contains("line one"));
        assert!(out.contains("line two"));
    }

    #[test]
    fn bat_line_numbers() {
        let mut fs = make_fs_with_file("/nums.txt", b"aaa\nbbb\nccc\n");
        let (status, out, _) = run(util_bat, &["bat", "--style=numbers", "/nums.txt"], &mut fs);
        assert_eq!(status, 0);
        // Should show line numbers
        assert!(out.contains('1'));
        assert!(out.contains('2'));
        assert!(out.contains('3'));
        // Should NOT show file header
        assert!(!out.contains("File:"));
    }

    #[test]
    fn bat_header_only() {
        let mut fs = make_fs_with_file("/hdr.txt", b"content\n");
        let (status, out, _) = run(util_bat, &["bat", "--style=header", "/hdr.txt"], &mut fs);
        assert_eq!(status, 0);
        // Should show file header
        assert!(out.contains("File: /hdr.txt"));
        // Should contain the content
        assert!(out.contains("content"));
    }

    #[test]
    fn bat_line_range() {
        let mut fs = make_fs_with_file("/range.txt", b"one\ntwo\nthree\nfour\nfive\n");
        let (status, out, _) = run(util_bat, &["bat", "-p", "-r", "2:3", "/range.txt"], &mut fs);
        assert_eq!(status, 0);
        assert!(out.contains("two"));
        assert!(out.contains("three"));
        assert!(!out.contains("one"));
        assert!(!out.contains("four"));
        assert!(!out.contains("five"));
    }

    #[test]
    fn bat_line_range_open_end() {
        let mut fs = make_fs_with_file("/range2.txt", b"a\nb\nc\nd\ne\n");
        let (status, out, _) = run(util_bat, &["bat", "-p", "-r", "3:", "/range2.txt"], &mut fs);
        assert_eq!(status, 0);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(out.contains('c'));
        assert!(out.contains('d'));
        assert!(out.contains('e'));
    }

    #[test]
    fn bat_line_range_open_start() {
        let mut fs = make_fs_with_file("/range3.txt", b"alpha\nbeta\ngamma\ndelta\n");
        let (status, out, _) = run(util_bat, &["bat", "-p", "-r", ":2", "/range3.txt"], &mut fs);
        assert_eq!(status, 0);
        assert!(out.contains("alpha"));
        assert!(out.contains("beta"));
        assert!(!out.contains("gamma"));
        assert!(!out.contains("delta"));
    }

    #[test]
    fn bat_stdin() {
        let mut fs = make_fs();
        let (status, out, _) = run_stdin(util_bat, &["bat", "-p"], b"from stdin\n", &mut fs);
        assert_eq!(status, 0);
        assert!(out.contains("from stdin"));
    }

    #[test]
    fn bat_missing_file() {
        let mut fs = make_fs();
        let (status, _out, err) = run(util_bat, &["bat", "/no_such_file"], &mut fs);
        assert_eq!(status, 1);
        assert!(!err.is_empty(), "expected error on stderr");
    }

    #[test]
    fn bat_show_all() {
        let mut fs = make_fs_with_file("/tabs.txt", b"col1\tcol2\n");
        let (status, out, _) = run(util_bat, &["bat", "-A", "-p", "/tabs.txt"], &mut fs);
        assert_eq!(status, 0);
        // -A should convert tab to visible representation
        assert!(out.contains("\\t"), "expected \\t for tab, got: {out}");
    }

    // -------------------------------------------------------------------
    // bat --style=numbers  just line numbers
    // -------------------------------------------------------------------

    #[test]
    fn bat_style_numbers_only() {
        let mut fs = make_fs_with_file("/sn.txt", b"alpha\nbeta\n");
        let (status, out, _) = run(util_bat, &["bat", "--style=numbers", "/sn.txt"], &mut fs);
        assert_eq!(status, 0);
        // Line numbers should be present
        assert!(out.contains('1'));
        assert!(out.contains('2'));
        // Header/footer should NOT be present
        assert!(!out.contains("File:"));
    }

    // -------------------------------------------------------------------
    // bat --style=header  just header
    // -------------------------------------------------------------------

    #[test]
    fn bat_style_header_only() {
        let mut fs = make_fs_with_file("/sh.txt", b"content\n");
        let (status, out, _) = run(util_bat, &["bat", "--style=header", "/sh.txt"], &mut fs);
        assert_eq!(status, 0);
        assert!(out.contains("File: /sh.txt"), "expected file header: {out}");
        assert!(out.contains("content"));
    }

    // -------------------------------------------------------------------
    // bat -r 2:4  specific line range
    // -------------------------------------------------------------------

    #[test]
    fn bat_range_2_to_4() {
        let mut fs = make_fs_with_file("/r24.txt", b"line1\nline2\nline3\nline4\nline5\n");
        let (status, out, _) = run(util_bat, &["bat", "-p", "-r", "2:4", "/r24.txt"], &mut fs);
        assert_eq!(status, 0);
        assert!(!out.contains("line1"));
        assert!(out.contains("line2"));
        assert!(out.contains("line3"));
        assert!(out.contains("line4"));
        assert!(!out.contains("line5"));
    }

    // -------------------------------------------------------------------
    // bat -A  show-all non-printable
    // -------------------------------------------------------------------

    #[test]
    fn bat_show_all_non_printable() {
        let mut fs = make_fs_with_file("/np.txt", b"a\tb\rc\x01d\n");
        let (status, out, _) = run(util_bat, &["bat", "-A", "-p", "/np.txt"], &mut fs);
        assert_eq!(status, 0);
        assert!(out.contains("\\t"), "expected \\t: {out}");
        assert!(out.contains("\\r"), "expected \\r: {out}");
        assert!(out.contains("\\x01"), "expected \\x01: {out}");
    }

    // -------------------------------------------------------------------
    // grep -n  line numbers
    // -------------------------------------------------------------------

    #[test]
    fn grep_line_numbers() {
        let mut fs = make_fs_with_file("/gn.txt", b"aaa\nbbb\nccc\nbbb\n");
        let (status, out, _) = run(util_grep, &["grep", "-n", "bbb", "/gn.txt"], &mut fs);
        assert_eq!(status, 0);
        assert!(out.contains("2:bbb"), "expected 2:bbb in: {out}");
        assert!(out.contains("4:bbb"), "expected 4:bbb in: {out}");
    }

    // -------------------------------------------------------------------
    // grep -v  invert match
    // -------------------------------------------------------------------

    #[test]
    fn grep_invert_match() {
        let mut fs = make_fs_with_file("/gv.txt", b"alpha\nbeta\ngamma\n");
        let (status, out, _) = run(util_grep, &["grep", "-v", "beta", "/gv.txt"], &mut fs);
        assert_eq!(status, 0);
        assert!(out.contains("alpha"));
        assert!(out.contains("gamma"));
        assert!(!out.contains("beta"));
    }

    // -------------------------------------------------------------------
    // grep -c  count only
    // -------------------------------------------------------------------

    #[test]
    fn grep_count_only() {
        let mut fs = make_fs_with_file("/gc.txt", b"a\nb\na\nc\na\n");
        let (status, out, _) = run(util_grep, &["grep", "-c", "a", "/gc.txt"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(out.trim(), "3");
    }

    // -------------------------------------------------------------------
    // grep -i  case insensitive
    // -------------------------------------------------------------------

    #[test]
    fn grep_case_insensitive() {
        let mut fs = make_fs_with_file("/gi.txt", b"Hello\nhello\nHELLO\nworld\n");
        let (status, out, _) = run(util_grep, &["grep", "-i", "hello", "/gi.txt"], &mut fs);
        assert_eq!(status, 0);
        assert!(out.contains("Hello"));
        assert!(out.contains("hello"));
        assert!(out.contains("HELLO"));
        assert!(!out.contains("world"));
    }

    // -------------------------------------------------------------------
    // tee  to multiple files
    // -------------------------------------------------------------------

    #[test]
    fn tee_multiple_files() {
        let mut fs = make_fs();
        let (status, out, _) = run_stdin(
            util_tee,
            &["tee", "/f1.txt", "/f2.txt"],
            b"tee data",
            &mut fs,
        );
        assert_eq!(status, 0);
        // stdout should echo the input
        assert_eq!(out, "tee data");
        // Both files should be written
        let h = fs.open("/f1.txt", OpenOptions::read()).unwrap();
        let d1 = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&d1, b"tee data");

        let h = fs.open("/f2.txt", OpenOptions::read()).unwrap();
        let d2 = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&d2, b"tee data");
    }

    // -------------------------------------------------------------------
    // tee -a  append mode
    // -------------------------------------------------------------------

    #[test]
    fn tee_append_mode() {
        let mut fs = make_fs_with_file("/app.txt", b"old ");
        let (status, _, _) = run_stdin(util_tee, &["tee", "-a", "/app.txt"], b"new", &mut fs);
        assert_eq!(status, 0);
        let h = fs.open("/app.txt", OpenOptions::read()).unwrap();
        let d = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&d, b"old new");
    }

    // -------------------------------------------------------------------
    // sed 's/old/new/g'  global replace
    // -------------------------------------------------------------------

    #[test]
    fn sed_global_replace() {
        let mut fs = make_fs_with_file("/sg.txt", b"old old old\nold new old\n");
        let (status, out, _) = run(util_sed, &["sed", "s/old/new/g", "/sg.txt"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(out, "new new new\nnew new new\n");
    }

    // -------------------------------------------------------------------
    // sed non-global replace (first only)
    // -------------------------------------------------------------------

    #[test]
    fn sed_first_only_replace() {
        let mut fs = make_fs_with_file("/sf.txt", b"aXbXc\n");
        let (status, out, _) = run(util_sed, &["sed", "s/X/Y/", "/sf.txt"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(out, "aYbXc\n");
    }

    // -------------------------------------------------------------------
    // cut -d: -f1  with custom delimiter
    // -------------------------------------------------------------------

    #[test]
    fn cut_custom_delimiter() {
        let mut fs = make_fs_with_file("/cd.txt", b"user:x:1000:1000\nroot:x:0:0\n");
        let (status, out, _) = run(util_cut, &["cut", "-d", ":", "-f", "1", "/cd.txt"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(out, "user\nroot\n");
    }

    // -------------------------------------------------------------------
    // tr 'a-z' 'A-Z'  range translation
    // -------------------------------------------------------------------

    #[test]
    fn tr_lowercase_to_uppercase() {
        let mut fs = make_fs();
        let (status, out, _) = run_stdin(
            util_tr,
            &[
                "tr",
                "abcdefghijklmnopqrstuvwxyz",
                "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
            ],
            b"hello world",
            &mut fs,
        );
        assert_eq!(status, 0);
        assert_eq!(out, "HELLO WORLD");
    }

    // -------------------------------------------------------------------
    // column -t  tabular format
    // -------------------------------------------------------------------

    #[test]
    fn column_tabular() {
        let mut fs = make_fs_with_file("/ct.txt", b"aa bb cc\nd eee f\n");
        let (status, out, _) = run(util_column, &["column", "-t", "/ct.txt"], &mut fs);
        assert_eq!(status, 0);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        // Columns should be aligned (second column starts at same position)
        let col2_pos_0 = lines[0].find("bb").unwrap();
        let col2_pos_1 = lines[1].find("eee").unwrap();
        assert_eq!(col2_pos_0, col2_pos_1, "columns not aligned: {lines:?}");
    }
}
