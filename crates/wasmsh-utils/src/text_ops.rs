//! Text utilities: head, tail, wc, grep, sed, sort, uniq, cut, tr, tee, paste, rev, column.

use std::collections::VecDeque;
use std::fmt::Write;
use std::io::Read;

use wasmsh_fs::{OpenOptions, Vfs};

use crate::helpers::{
    collect_input_lines, collect_input_text, collect_path_text, emit_error, grep_matches,
    open_reader_for_path, read_next_line_from_reader, resolve_path, stream_input_chunks,
    stream_input_lines,
};
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

fn head_emit_reader(
    output: &mut dyn UtilOutput,
    reader: &mut dyn Read,
    mode: &HeadMode,
    cmd: &str,
) -> i32 {
    match mode {
        HeadMode::Bytes(limit) => {
            let mut remaining = *limit;
            let mut buffer = [0u8; 4096];
            while remaining > 0 {
                let to_read = remaining.min(buffer.len());
                match reader.read(&mut buffer[..to_read]) {
                    Ok(0) => break,
                    Ok(read) => {
                        output.stdout(&buffer[..read]);
                        remaining -= read;
                    }
                    Err(err) => {
                        let msg = format!("{cmd}: stdin read error: {err}\n");
                        output.stderr(msg.as_bytes());
                        return 1;
                    }
                }
            }
            0
        }
        HeadMode::Lines(limit) => {
            let mut lines_seen = 0usize;
            let mut buffer = [0u8; 4096];
            while lines_seen < *limit {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        let mut consumed = 0usize;
                        while consumed < read && lines_seen < *limit {
                            let byte = buffer[consumed];
                            output.stdout(&buffer[consumed..consumed + 1]);
                            consumed += 1;
                            if byte == b'\n' {
                                lines_seen += 1;
                            }
                        }
                        if lines_seen >= *limit {
                            break;
                        }
                    }
                    Err(err) => {
                        let msg = format!("{cmd}: stdin read error: {err}\n");
                        output.stderr(msg.as_bytes());
                        return 1;
                    }
                }
            }
            0
        }
    }
}

pub(crate) fn util_head(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (mode, quiet, verbose, files) = parse_head_args(argv);
    if files.is_empty() {
        if let Some(mut stdin) = ctx.stdin.take() {
            return head_emit_reader(ctx.output, &mut stdin, &mode, "head");
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
        match open_reader_for_path(ctx, &full, path, "head") {
            Ok(mut reader) => {
                status |= head_emit_reader(ctx.output, reader.as_mut(), &mode, "head");
            }
            Err(_) => status = 1,
        }
    }
    status
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

fn tail_emit_reader(
    output: &mut dyn UtilOutput,
    reader: &mut dyn Read,
    mode: &TailMode,
    cmd: &str,
) -> i32 {
    match mode {
        TailMode::Bytes(limit) => {
            let mut ring = VecDeque::with_capacity(*limit);
            let mut buffer = [0u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        for &byte in &buffer[..read] {
                            if ring.len() == *limit && *limit > 0 {
                                ring.pop_front();
                            }
                            if *limit > 0 {
                                ring.push_back(byte);
                            }
                        }
                    }
                    Err(err) => {
                        let msg = format!("{cmd}: stdin read error: {err}\n");
                        output.stderr(msg.as_bytes());
                        return 1;
                    }
                }
            }
            let bytes: Vec<u8> = ring.into_iter().collect();
            output.stdout(&bytes);
            0
        }
        TailMode::Lines(limit, from_start) => {
            let mut buffer = [0u8; 4096];
            let mut current = Vec::new();
            if *from_start {
                let mut lines_seen = 0usize;
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => {
                            for &byte in &buffer[..read] {
                                if byte == b'\n' {
                                    if lines_seen + 1 >= *limit {
                                        output.stdout(&current);
                                        output.stdout(b"\n");
                                    }
                                    current.clear();
                                    lines_seen += 1;
                                } else {
                                    current.push(byte);
                                }
                            }
                        }
                        Err(err) => {
                            let msg = format!("{cmd}: stdin read error: {err}\n");
                            output.stderr(msg.as_bytes());
                            return 1;
                        }
                    }
                }
                if !current.is_empty() && lines_seen + 1 >= *limit {
                    output.stdout(&current);
                    output.stdout(b"\n");
                }
                return 0;
            }

            let mut ring: VecDeque<Vec<u8>> = VecDeque::with_capacity(*limit);
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        for &byte in &buffer[..read] {
                            if byte == b'\n' {
                                if ring.len() == *limit && *limit > 0 {
                                    ring.pop_front();
                                }
                                if *limit > 0 {
                                    ring.push_back(std::mem::take(&mut current));
                                } else {
                                    current.clear();
                                }
                            } else {
                                current.push(byte);
                            }
                        }
                    }
                    Err(err) => {
                        let msg = format!("{cmd}: stdin read error: {err}\n");
                        output.stderr(msg.as_bytes());
                        return 1;
                    }
                }
            }
            if !current.is_empty() {
                if ring.len() == *limit && *limit > 0 {
                    ring.pop_front();
                }
                if *limit > 0 {
                    ring.push_back(current);
                }
            }
            for line in ring {
                output.stdout(&line);
                output.stdout(b"\n");
            }
            0
        }
    }
}

pub(crate) fn util_tail(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (mode, quiet, verbose, files) = parse_tail_args(argv);
    if files.is_empty() {
        if let Some(mut stdin) = ctx.stdin.take() {
            return tail_emit_reader(ctx.output, &mut stdin, &mode, "tail");
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
        match open_reader_for_path(ctx, &full, path, "tail") {
            Ok(mut reader) => {
                status |= tail_emit_reader(ctx.output, reader.as_mut(), &mode, "tail");
            }
            Err(_) => status = 1,
        }
    }
    status
}

pub(crate) fn util_wc(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, file_args) = parse_wc_flags(&argv[1..]);

    if file_args.is_empty() {
        if let Some(mut stdin) = ctx.stdin.take() {
            return match wc_emit_reader(ctx.output, &mut stdin, None, &flags, "wc") {
                Ok(_) => 0,
                Err(code) => code,
            };
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
        match open_reader_for_path(ctx, &full, path, "wc") {
            Ok(mut reader) => {
                match wc_emit_reader(ctx.output, reader.as_mut(), Some(path), &flags, "wc") {
                    Ok((lines, words, bytes, _max_line_length)) => {
                        total_lines += lines;
                        total_words += words;
                        total_bytes += bytes;
                    }
                    Err(_) => status = 1,
                }
            }
            Err(_) => status = 1,
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

fn wc_emit_reader(
    output: &mut dyn UtilOutput,
    reader: &mut dyn Read,
    path: Option<&str>,
    flags: &WcFlags,
    cmd: &str,
) -> Result<(usize, usize, usize, usize), i32> {
    let mut buffer = [0u8; 4096];
    let mut lines = 0usize;
    let mut words = 0usize;
    let mut bytes = 0usize;
    let mut max_line_length = 0usize;
    let mut current_line_length = 0usize;
    let mut in_word = false;
    let mut saw_input = false;
    let mut ended_with_newline = false;

    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                saw_input = true;
                bytes += read;
                for &byte in &buffer[..read] {
                    let is_whitespace = byte.is_ascii_whitespace();
                    if is_whitespace {
                        in_word = false;
                    } else if !in_word {
                        words += 1;
                        in_word = true;
                    }

                    if byte == b'\n' {
                        lines += 1;
                        max_line_length = max_line_length.max(current_line_length);
                        current_line_length = 0;
                        ended_with_newline = true;
                    } else {
                        current_line_length += 1;
                        ended_with_newline = false;
                    }
                }
            }
            Err(err) => {
                let msg = format!("{cmd}: stdin read error: {err}\n");
                output.stderr(msg.as_bytes());
                return Err(1);
            }
        }
    }

    if saw_input && !ended_with_newline {
        lines += 1;
        max_line_length = max_line_length.max(current_line_length);
    }

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
        parts.push(format!("{max_line_length:>7}"));
    }
    let mut out = parts.join("");
    if let Some(path) = path {
        out.push(' ');
        out.push_str(path);
    }
    out.push('\n');
    output.stdout(out.as_bytes());
    Ok((lines, words, bytes, max_line_length))
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

    // `-F` (fixed strings) bypasses the regex engine entirely.
    if flags.fixed {
        return grep_match_single(&l, &p, flags);
    }

    // Try the POSIX regex engine first (BRE by default, ERE with `-E`).
    // Fall back to the old literal matcher if the pattern does not parse
    // — this matches the historical behaviour for malformed patterns.
    let compiled = if flags.extended {
        crate::regex_posix::Regex::compile_ere(&p)
    } else {
        crate::regex_posix::Regex::compile_bre(&p)
    };
    match compiled {
        Ok(re) => {
            if flags.word_match {
                grep_regex_word_match(&l, &re)
            } else {
                re.is_match(&l)
            }
        }
        Err(_) => {
            if flags.extended && p.contains('|') {
                return p
                    .split('|')
                    .any(|alt| grep_match_single(&l, alt.trim(), flags));
            }
            grep_match_single(&l, &p, flags)
        }
    }
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

/// Word-boundary match built on top of the regex engine: the regex must
/// match inside the line AND the matched span must be flanked by
/// non-word characters (or line boundaries).
fn grep_regex_word_match(line: &str, re: &crate::regex_posix::Regex) -> bool {
    for (start, end) in re.find_iter_offsets(line) {
        let before_ok = start == 0
            || !line.as_bytes()[start - 1].is_ascii_alphanumeric()
                && line.as_bytes()[start - 1] != b'_';
        let after_ok = end >= line.len()
            || !line.as_bytes()[end].is_ascii_alphanumeric()
                && line.as_bytes()[end] != b'_';
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

fn grep_find_match<'a>(line: &'a str, pattern: &str, flags: &GrepFlags) -> Option<&'a str> {
    let (l, p) = if flags.ignore_case {
        (line.to_lowercase(), pattern.to_lowercase())
    } else {
        (line.to_string(), pattern.to_string())
    };

    // Regex-aware match for `-o`: fall through to literal substring on
    // compile failure, matching the behaviour of `grep_match_pattern`.
    if !flags.fixed {
        let compiled = if flags.extended {
            crate::regex_posix::Regex::compile_ere(&p)
        } else {
            crate::regex_posix::Regex::compile_bre(&p)
        };
        if let Ok(re) = compiled {
            for (start, end) in re.find_iter_offsets(&l) {
                if flags.word_match {
                    let before_ok = start == 0
                        || !l.as_bytes()[start - 1].is_ascii_alphanumeric()
                            && l.as_bytes()[start - 1] != b'_';
                    let after_ok = end >= l.len()
                        || !l.as_bytes()[end].is_ascii_alphanumeric()
                            && l.as_bytes()[end] != b'_';
                    if !(before_ok && after_ok) {
                        continue;
                    }
                }
                return Some(&line[start..end]);
            }
            return None;
        }
    }

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

fn grep_process_reader(
    output: &mut dyn UtilOutput,
    reader: &mut dyn Read,
    filename: Option<&str>,
    flags: &GrepFlags,
    patterns: &[&str],
    cmd: &str,
) -> Result<(bool, u64), i32> {
    let mut match_count = 0u64;
    let mut found = false;
    let mut remaining_after = 0usize;
    let mut printed_separator = false;
    let mut before_buf: VecDeque<(usize, String)> = VecDeque::new();
    let mut pending = Vec::new();
    let mut line_num = 0usize;

    while let Some((line, _had_newline)) =
        read_next_line_from_reader(reader, &mut pending, output, cmd)?
    {
        line_num += 1;
        if grep_line_matches(&line, flags, patterns) {
            found = true;
            match_count += 1;

            if flags.quiet || flags.files_only {
                if let Some(max) = flags.max_count {
                    if match_count >= max as u64 {
                        break;
                    }
                }
                continue;
            }
            if !flags.count_only {
                if flags.before_context > 0 && !before_buf.is_empty() {
                    if printed_separator && flags.before_context > 0 {
                        output.stdout(b"--\n");
                    }
                    for (before_line_num, before_line) in &before_buf {
                        grep_emit_one(
                            output,
                            before_line,
                            *before_line_num,
                            filename,
                            flags,
                            patterns,
                        );
                    }
                }
                before_buf.clear();
                grep_emit_one(output, &line, line_num, filename, flags, patterns);
                remaining_after = flags.after_context;
                printed_separator = true;
            }
            if let Some(max) = flags.max_count {
                if match_count >= max as u64 {
                    break;
                }
            }
        } else if remaining_after > 0 && !flags.count_only {
            grep_emit_one(output, &line, line_num, filename, flags, patterns);
            remaining_after -= 1;
        } else if flags.before_context > 0 {
            before_buf.push_back((line_num, line));
            if before_buf.len() > flags.before_context {
                before_buf.pop_front();
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
    Ok((found, match_count))
}

fn grep_process_stdin(
    ctx: &mut UtilContext<'_>,
    flags: &GrepFlags,
    patterns: &[&str],
) -> Result<(bool, u64), i32> {
    let Some(mut stdin) = ctx.stdin.take() else {
        return Ok((false, 0));
    };
    grep_process_reader(ctx.output, &mut stdin, None, flags, patterns, "grep")
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
            if let Ok(mut reader) = open_reader_for_path(ctx, &child, &child, "grep") {
                let Ok((found, _)) = grep_process_reader(
                    ctx.output,
                    reader.as_mut(),
                    Some(&child),
                    flags,
                    patterns,
                    "grep",
                ) else {
                    continue;
                };
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
        if !ctx.has_stdin() {
            ctx.output.stderr(b"grep: missing file operand\n");
            return 2;
        }
        let Ok((found, _)) = grep_process_stdin(ctx, &flags, &patterns) else {
            return 2;
        };
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
        match open_reader_for_path(ctx, &full, path, "grep") {
            Ok(mut reader) => {
                let fname = if show_fn { Some(*path) } else { None };
                let Ok((found, _)) = grep_process_reader(
                    ctx.output,
                    reader.as_mut(),
                    fname,
                    &adj_flags,
                    &patterns,
                    "grep",
                ) else {
                    continue;
                };
                if found {
                    found_any = true;
                    if adj_flags.files_only {
                        let out = format!("{path}\n");
                        ctx.output.stdout(out.as_bytes());
                    }
                }
            }
            Err(_) => {}
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
    is_last: bool,
    line: &str,
    in_range: &mut bool,
) -> bool {
    match addr {
        SedAddr::None => true,
        SedAddr::Line(n) => line_num == *n,
        SedAddr::Last => is_last,
        SedAddr::Regex(pat) => crate::regex_posix::Regex::compile_bre(pat)
            .map(|re| re.is_match(line))
            .unwrap_or_else(|_| grep_matches(line, pat, false)),
        SedAddr::Range(start, end) => {
            if *in_range {
                if sed_addr_matches(end, line_num, is_last, line, &mut false) {
                    *in_range = false;
                }
                true
            } else if sed_addr_matches(start, line_num, is_last, line, &mut false) {
                *in_range = true;
                true
            } else {
                false
            }
        }
    }
}

fn sed_emit_line(output: &mut dyn UtilOutput, capture: &mut Option<String>, line: &str) {
    output.stdout(line.as_bytes());
    output.stdout(b"\n");
    if let Some(capture) = capture.as_mut() {
        capture.push_str(line);
        capture.push('\n');
    }
}

fn sed_process_reader(
    output: &mut dyn UtilOutput,
    reader: &mut dyn Read,
    instructions: &[SedInstruction],
    suppress_print: bool,
    capture: &mut Option<String>,
    cmd: &str,
) -> Result<(), i32> {
    let mut range_states: Vec<bool> = vec![false; instructions.len()];
    let mut pending = Vec::new();
    let mut line_num = 1usize;
    let mut current = read_next_line_from_reader(reader, &mut pending, output, cmd)?;
    let mut next = if current.is_some() {
        read_next_line_from_reader(reader, &mut pending, output, cmd)?
    } else {
        None
    };

    while let Some((line, _had_newline)) = current.take() {
        let is_last = next.is_none();
        let mut current_text = line;
        let mut deleted = false;
        let mut printed = false;
        let mut quit = false;

        for (ci, instr) in instructions.iter().enumerate() {
            if !sed_addr_matches(
                &instr.addr,
                line_num,
                is_last,
                &current_text,
                &mut range_states[ci],
            ) {
                continue;
            }
            match &instr.cmd {
                SedCmd::Substitute(sub) => {
                    // Use POSIX BRE regex when the pattern compiles;
                    // fall back to literal replacement otherwise so
                    // malformed patterns still behave sensibly.
                    current_text = match crate::regex_posix::Regex::compile_bre(&sub.pattern) {
                        Ok(re) => {
                            if sub.global {
                                re.replace_all(&current_text, &sub.replacement)
                            } else {
                                re.replace(&current_text, &sub.replacement)
                            }
                        }
                        Err(_) => {
                            if sub.global {
                                current_text.replace(&sub.pattern, &sub.replacement)
                            } else {
                                current_text.replacen(&sub.pattern, &sub.replacement, 1)
                            }
                        }
                    };
                }
                SedCmd::Delete => {
                    deleted = true;
                    break;
                }
                SedCmd::Print => {
                    sed_emit_line(output, capture, &current_text);
                    printed = true;
                }
                SedCmd::Transliterate(from, to) => {
                    current_text = current_text
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
                    if !suppress_print && !printed {
                        sed_emit_line(output, capture, &current_text);
                        printed = true;
                    }
                    sed_emit_line(output, capture, text);
                }
                SedCmd::InsertText(text) => {
                    sed_emit_line(output, capture, text);
                }
                SedCmd::ChangeText(text) => {
                    sed_emit_line(output, capture, text);
                    deleted = true;
                    printed = true;
                    break;
                }
                SedCmd::Quit => {
                    quit = true;
                    break;
                }
            }
        }

        if !deleted && !suppress_print && !printed {
            sed_emit_line(output, capture, &current_text);
        }
        if quit {
            break;
        }

        current = next.take();
        if current.is_some() {
            next = read_next_line_from_reader(reader, &mut pending, output, cmd)?;
        }
        line_num += 1;
    }

    Ok(())
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
            if let Ok(script) = collect_path_text(ctx, &full, argv[i + 1], "sed") {
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

    if in_place && !file_args.is_empty() {
        for path in &file_args {
            let full = resolve_path(ctx.cwd, path);
            if let Some(ref suffix) = in_place_suffix {
                let backup = format!("{full}{suffix}");
                let _ = crate::helpers::copy_file_contents(ctx.fs, &full, &backup);
            }
            let mut reader = match open_reader_for_path(ctx, &full, path, "sed") {
                Ok(reader) => reader,
                Err(status) => return status,
            };
            let mut dummy = SedDummyOutput;
            let mut result = Some(String::new());
            if let Err(status) = sed_process_reader(
                &mut dummy,
                reader.as_mut(),
                &instructions,
                suppress_print,
                &mut result,
                "sed",
            ) {
                return status;
            }
            if let Ok(h) = ctx.fs.open(&full, OpenOptions::write()) {
                let _ = ctx.fs.write_file(h, result.unwrap_or_default().as_bytes());
                ctx.fs.close(h);
            }
        }
        return 0;
    }

    if file_args.is_empty() {
        let Some(mut stdin) = ctx.stdin.take() else {
            ctx.output.stderr(b"sed: missing operand\n");
            return 1;
        };
        let mut capture = None;
        return match sed_process_reader(
            ctx.output,
            &mut stdin,
            &instructions,
            suppress_print,
            &mut capture,
            "sed",
        ) {
            Ok(()) => 0,
            Err(status) => status,
        };
    }

    for path in &file_args {
        let full = resolve_path(ctx.cwd, path);
        let mut reader = match open_reader_for_path(ctx, &full, path, "sed") {
            Ok(reader) => reader,
            Err(status) => return status,
        };
        let mut capture = None;
        if let Err(status) = sed_process_reader(
            ctx.output,
            reader.as_mut(),
            &instructions,
            suppress_print,
            &mut capture,
            "sed",
        ) {
            return status;
        }
    }
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
        let na = parse_leading_number(ka.trim());
        let nb = parse_leading_number(kb.trim());
        na.partial_cmp(&nb).unwrap_or(std::cmp::Ordering::Equal)
    } else if flags.version_sort {
        version_compare(ka, kb)
    } else if flags.ignore_case {
        ka.to_lowercase().cmp(&kb.to_lowercase())
    } else {
        ka.cmp(kb)
    }
}

/// Parse the leading numeric portion of a string, ignoring trailing text.
/// Matches GNU sort -n behavior: `"  3 apple"` → `3.0`, `"foo"` → `0.0`.
fn parse_leading_number(s: &str) -> f64 {
    let s = s.trim_start();
    if s.is_empty() {
        return 0.0;
    }
    // Collect optional sign + digits + optional decimal
    let end = s
        .char_indices()
        .skip_while(|(i, c)| *i == 0 && (*c == '-' || *c == '+'))
        .skip_while(|(_, c)| c.is_ascii_digit())
        .skip_while(|(_, c)| *c == '.')
        .skip_while(|(_, c)| c.is_ascii_digit())
        .map(|(i, _)| i)
        .next()
        .unwrap_or(s.len());
    s[..end].parse::<f64>().unwrap_or(0.0)
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
    let mut lines = match collect_input_lines(ctx, &file_args, "sort") {
        Ok(lines) => lines,
        Err(status) => return status,
    };

    if flags.check {
        for w in lines.windows(2) {
            let ord = sort_compare(&w[0], &w[1], &flags);
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
    if file_args.is_empty() && !ctx.has_stdin() {
        return 0;
    }

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

    if stream_input_lines(ctx, &file_args, "uniq", |line, _had_newline, ctx| {
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
        Ok(())
    })
    .is_err()
    {
        return 1;
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
        } else if arg.starts_with("-d") && arg.len() > 2 {
            delim = arg[2..].chars().next().unwrap_or('\t');
            i += 1;
        } else if arg == "-f" && i + 1 < argv.len() {
            mode = Some(CutMode::Fields(parse_cut_ranges(argv[i + 1])));
            i += 2;
        } else if let Some(spec) = arg.strip_prefix("-f") {
            mode = Some(CutMode::Fields(parse_cut_ranges(spec)));
            i += 1;
        } else if arg == "-c" && i + 1 < argv.len() {
            mode = Some(CutMode::Chars(parse_cut_ranges(argv[i + 1])));
            i += 2;
        } else if let Some(spec) = arg.strip_prefix("-c") {
            mode = Some(CutMode::Chars(parse_cut_ranges(spec)));
            i += 1;
        } else if arg == "-b" && i + 1 < argv.len() {
            mode = Some(CutMode::Bytes(parse_cut_ranges(argv[i + 1])));
            i += 2;
        } else if let Some(spec) = arg.strip_prefix("-b") {
            mode = Some(CutMode::Bytes(parse_cut_ranges(spec)));
            i += 1;
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
    if stream_input_lines(ctx, &file_args, "cut", |line, _had_newline, ctx| {
        match &mode {
            CutMode::Fields(ranges) => {
                if only_delimited && !line.contains(delim) {
                    return Ok(());
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
        Ok(())
    })
    .is_err()
    {
        return 1;
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

fn tr_process_utf8_chunk(pending: &mut Vec<u8>, chunk: &[u8], mut f: impl FnMut(char)) {
    pending.extend_from_slice(chunk);
    loop {
        match std::str::from_utf8(pending) {
            Ok(text) => {
                for ch in text.chars() {
                    f(ch);
                }
                pending.clear();
                return;
            }
            Err(err) => {
                let valid = err.valid_up_to();
                if valid > 0 {
                    let text = String::from_utf8_lossy(&pending[..valid]).to_string();
                    for ch in text.chars() {
                        f(ch);
                    }
                    pending.drain(..valid);
                    continue;
                }
                if err.error_len().is_some() {
                    let text = String::from_utf8_lossy(&pending[..1]).to_string();
                    for ch in text.chars() {
                        f(ch);
                    }
                    pending.drain(..1);
                    continue;
                }
                return;
            }
        }
    }
}

fn tr_flush_pending_lossy(pending: &mut Vec<u8>, mut f: impl FnMut(char)) {
    if pending.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(pending).to_string();
    pending.clear();
    for ch in text.chars() {
        f(ch);
    }
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

    if !ctx.has_stdin() {
        ctx.output.stderr(b"tr: missing operand\n");
        return 1;
    }

    if set_args.is_empty() {
        ctx.output.stderr(b"tr: missing operand\n");
        return 1;
    }

    let from_chars = tr_expand_set(set_args[0]);

    if delete && squeeze && set_args.len() >= 2 {
        let squeeze_chars = tr_expand_set(set_args[1]);
        let mut prev: Option<char> = None;
        let mut pending = Vec::new();
        if stream_input_chunks(ctx, &[], "tr", |chunk, ctx| {
            tr_process_utf8_chunk(&mut pending, chunk, |c| {
                let in_set = from_chars.contains(&c);
                let keep = if complement { in_set } else { !in_set };
                if !keep {
                    return;
                }
                if squeeze_chars.contains(&c) && prev == Some(c) {
                    return;
                }
                let mut buf = [0u8; 4];
                ctx.output.stdout(c.encode_utf8(&mut buf).as_bytes());
                prev = Some(c);
            });
            Ok(())
        })
        .is_err()
        {
            return 1;
        }
        if !pending.is_empty() {
            tr_flush_pending_lossy(&mut pending, |c| {
                let in_set = from_chars.contains(&c);
                let keep = if complement { in_set } else { !in_set };
                if !keep {
                    return;
                }
                if squeeze_chars.contains(&c) && prev == Some(c) {
                    return;
                }
                let mut buf = [0u8; 4];
                ctx.output.stdout(c.encode_utf8(&mut buf).as_bytes());
                prev = Some(c);
            });
        }
        return 0;
    }

    if delete {
        let mut pending = Vec::new();
        if stream_input_chunks(ctx, &[], "tr", |chunk, ctx| {
            tr_process_utf8_chunk(&mut pending, chunk, |c| {
                let in_set = from_chars.contains(&c);
                let keep = if complement { in_set } else { !in_set };
                if keep {
                    let mut buf = [0u8; 4];
                    ctx.output.stdout(c.encode_utf8(&mut buf).as_bytes());
                }
            });
            Ok(())
        })
        .is_err()
        {
            return 1;
        }
        if !pending.is_empty() {
            tr_flush_pending_lossy(&mut pending, |c| {
                let in_set = from_chars.contains(&c);
                let keep = if complement { in_set } else { !in_set };
                if keep {
                    let mut buf = [0u8; 4];
                    ctx.output.stdout(c.encode_utf8(&mut buf).as_bytes());
                }
            });
        }
        return 0;
    }

    if squeeze && set_args.len() < 2 {
        let mut prev: Option<char> = None;
        let mut pending = Vec::new();
        if stream_input_chunks(ctx, &[], "tr", |chunk, ctx| {
            tr_process_utf8_chunk(&mut pending, chunk, |c| {
                if from_chars.contains(&c) && prev == Some(c) {
                    return;
                }
                let mut buf = [0u8; 4];
                ctx.output.stdout(c.encode_utf8(&mut buf).as_bytes());
                prev = Some(c);
            });
            Ok(())
        })
        .is_err()
        {
            return 1;
        }
        if !pending.is_empty() {
            tr_flush_pending_lossy(&mut pending, |c| {
                if from_chars.contains(&c) && prev == Some(c) {
                    return;
                }
                let mut buf = [0u8; 4];
                ctx.output.stdout(c.encode_utf8(&mut buf).as_bytes());
                prev = Some(c);
            });
        }
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

    let mut prev: Option<char> = None;
    let mut pending = Vec::new();
    if stream_input_chunks(ctx, &[], "tr", |chunk, ctx| {
        tr_process_utf8_chunk(&mut pending, chunk, |c| {
            let translated = if let Some(pos) = from_set.iter().position(|&fc| fc == c) {
                to_chars.get(pos).or(to_chars.last()).copied().unwrap_or(c)
            } else {
                c
            };
            if squeeze && to_chars.contains(&translated) && prev == Some(translated) {
                return;
            }
            let mut buf = [0u8; 4];
            ctx.output
                .stdout(translated.encode_utf8(&mut buf).as_bytes());
            prev = Some(translated);
        });
        Ok(())
    })
    .is_err()
    {
        return 1;
    }
    if !pending.is_empty() {
        tr_flush_pending_lossy(&mut pending, |c| {
            let translated = if let Some(pos) = from_set.iter().position(|&fc| fc == c) {
                to_chars.get(pos).or(to_chars.last()).copied().unwrap_or(c)
            } else {
                c
            };
            if squeeze && to_chars.contains(&translated) && prev == Some(translated) {
                return;
            }
            let mut buf = [0u8; 4];
            ctx.output
                .stdout(translated.encode_utf8(&mut buf).as_bytes());
            prev = Some(translated);
        });
    }
    0
}

pub(crate) fn util_tee(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut append = false;
    if args.first() == Some(&"-a") {
        append = true;
        args = &args[1..];
    }
    let mut status = 0;
    let mut handles = Vec::new();
    for path in args.iter().copied() {
        let full = resolve_path(ctx.cwd, path);
        if !append {
            match ctx.fs.open(&full, OpenOptions::write()) {
                Ok(h) => ctx.fs.close(h),
                Err(e) => {
                    emit_error(ctx.output, "tee", path, &e);
                    status = 1;
                    continue;
                }
            }
        }
        match ctx.fs.open(&full, OpenOptions::append()) {
            Ok(h) => handles.push((path, h)),
            Err(e) => {
                emit_error(ctx.output, "tee", path, &e);
                status = 1;
            }
        }
    }

    if let Some(mut stdin) = ctx.stdin.take() {
        let mut buffer = [0u8; 4096];
        loop {
            match stdin.read_chunk(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let chunk = &buffer[..read];
                    ctx.output.stdout(chunk);
                    for (path, handle) in &handles {
                        if let Err(e) = ctx.fs.write_file(*handle, chunk) {
                            emit_error(ctx.output, "tee", path, &e);
                            status = 1;
                        }
                    }
                }
                Err(err) => {
                    let msg = format!("tee: stdin read error: {err}\n");
                    ctx.output.stderr(msg.as_bytes());
                    status = 1;
                    break;
                }
            }
        }
    }
    for (_, handle) in handles {
        ctx.fs.close(handle);
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

struct PasteSource<'a> {
    reader: Box<dyn Read + 'a>,
    pending: Vec<u8>,
    finished: bool,
}

impl<'a> PasteSource<'a> {
    fn new(reader: Box<dyn Read + 'a>) -> Self {
        Self {
            reader,
            pending: Vec::new(),
            finished: false,
        }
    }

    fn next_line(&mut self, output: &mut dyn UtilOutput) -> Result<Option<String>, i32> {
        if self.finished {
            return Ok(None);
        }
        match read_next_line_from_reader(self.reader.as_mut(), &mut self.pending, output, "paste")?
        {
            Some((line, _had_newline)) => Ok(Some(line)),
            None => {
                self.finished = true;
                Ok(None)
            }
        }
    }
}

fn paste_open_sources<'a>(
    ctx: &mut UtilContext<'a>,
    args: &[&str],
) -> Result<Vec<PasteSource<'a>>, i32> {
    let mut sources = Vec::new();
    for path in args {
        let source = if *path == "-" {
            if let Some(stdin) = ctx.stdin.take() {
                PasteSource::new(Box::new(stdin))
            } else {
                PasteSource::new(Box::new(std::io::Cursor::new(Vec::new())))
            }
        } else {
            let full = resolve_path(ctx.cwd, path);
            PasteSource::new(open_reader_for_path(ctx, &full, path, "paste")?)
        };
        sources.push(source);
    }
    Ok(sources)
}

fn paste_serial(
    output: &mut dyn UtilOutput,
    sources: &mut [PasteSource<'_>],
    delimiter: &str,
) -> Result<(), i32> {
    for source in sources.iter_mut() {
        let mut first = true;
        while let Some(line) = source.next_line(output)? {
            if !first {
                output.stdout(delimiter.as_bytes());
            }
            output.stdout(line.as_bytes());
            first = false;
        }
        output.stdout(b"\n");
    }
    Ok(())
}

fn paste_merge(
    output: &mut dyn UtilOutput,
    sources: &mut [PasteSource<'_>],
    delimiter: &str,
) -> Result<(), i32> {
    loop {
        let mut row = Vec::with_capacity(sources.len());
        let mut saw_any = false;
        for source in sources.iter_mut() {
            let line = source.next_line(output)?;
            if line.is_some() {
                saw_any = true;
            }
            row.push(line);
        }
        if !saw_any {
            return Ok(());
        }
        for (idx, line) in row.into_iter().enumerate() {
            if idx > 0 {
                output.stdout(delimiter.as_bytes());
            }
            if let Some(line) = line {
                output.stdout(line.as_bytes());
            }
        }
        output.stdout(b"\n");
    }
}

pub(crate) fn util_paste(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, args) = parse_paste_flags(argv);

    if args.is_empty() {
        if !ctx.has_stdin() {
            ctx.output.stderr(b"paste: missing operand\n");
            return 1;
        }
        let mut ended_with_newline = true;
        if stream_input_chunks(ctx, &[], "paste", |chunk, ctx| {
            ended_with_newline = chunk.last().copied() == Some(b'\n');
            ctx.output.stdout(chunk);
            Ok(())
        })
        .is_err()
        {
            return 1;
        }
        if !ended_with_newline {
            ctx.output.stdout(b"\n");
        }
        return 0;
    }

    let mut sources = match paste_open_sources(ctx, args) {
        Ok(sources) => sources,
        Err(status) => return status,
    };

    if flags.serial {
        return match paste_serial(ctx.output, &mut sources, &flags.delimiter) {
            Ok(()) => 0,
            Err(status) => status,
        };
    }
    match paste_merge(ctx.output, &mut sources, &flags.delimiter) {
        Ok(()) => 0,
        Err(status) => status,
    }
}

pub(crate) fn util_rev(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let file_args = &argv[1..];
    if file_args.is_empty() && !ctx.has_stdin() {
        ctx.output.stderr(b"rev: missing operand\n");
        return 1;
    }
    if stream_input_lines(ctx, file_args, "rev", |line, _had_newline, ctx| {
        let reversed: String = line.chars().rev().collect();
        ctx.output.stdout(reversed.as_bytes());
        ctx.output.stdout(b"\n");
        Ok(())
    })
    .is_err()
    {
        return 1;
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
        if ctx.has_stdin() {
            let separator = "\u{2500}";
            let rule_left: String = separator.repeat(7);
            let rule_right: String = separator.repeat(20);
            let vert = "\u{2502}";
            if flags.show_header {
                bat_emit_chrome(ctx.output, None, &rule_left, &rule_right);
            }
            let mut line_num = 0usize;
            if stream_input_lines(ctx, &[], "bat", |line, _had_newline, ctx| {
                line_num += 1;
                if !bat_in_range(line_num, flags.line_range) {
                    return Ok(());
                }
                let display_line = if flags.show_all {
                    make_visible(line)
                } else {
                    line.to_string()
                };
                let out = if flags.show_numbers {
                    format!("{line_num:>5}   {vert} {display_line}\n")
                } else {
                    format!("{display_line}\n")
                };
                ctx.output.stdout(out.as_bytes());
                Ok(())
            })
            .is_err()
            {
                return 1;
            }
            if flags.show_header {
                let bot_corner = "\u{2534}";
                let footer = format!("{rule_left}{bot_corner}{rule_right}\n");
                ctx.output.stdout(footer.as_bytes());
            }
            return 0;
        }
        ctx.output.stderr(b"bat: missing operand\n");
        return 1;
    }

    let mut status = 0;
    for path in &file_args {
        let full = resolve_path(ctx.cwd, path);
        match open_reader_for_path(ctx, &full, path, "bat") {
            Ok(mut reader) => {
                if bat_output(
                    ctx.output,
                    Some(path),
                    reader.as_mut(),
                    flags.show_numbers,
                    flags.show_header,
                    flags.line_range,
                    flags.show_all,
                )
                .is_err()
                {
                    status = 1;
                }
            }
            Err(_) => status = 1,
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
    output: &mut dyn UtilOutput,
    filename: Option<&str>,
    rule_left: &str,
    rule_right: &str,
) {
    let top_corner = "\u{252C}";
    let mid_corner = "\u{253C}";
    let vert = "\u{2502}";

    let header_line = format!("{rule_left}{top_corner}{rule_right}\n");
    output.stdout(header_line.as_bytes());
    if let Some(name) = filename {
        let file_line = format!("       {vert} File: {name}\n");
        output.stdout(file_line.as_bytes());
    }
    let sep_line = format!("{rule_left}{mid_corner}{rule_right}\n");
    output.stdout(sep_line.as_bytes());
}

fn bat_output(
    output: &mut dyn UtilOutput,
    filename: Option<&str>,
    reader: &mut dyn Read,
    show_numbers: bool,
    show_header: bool,
    line_range: Option<(Option<usize>, Option<usize>)>,
    show_all: bool,
) -> Result<(), i32> {
    let separator = "\u{2500}";
    let vert = "\u{2502}";
    let rule_left: String = separator.repeat(7);
    let rule_right: String = separator.repeat(20);

    if show_header {
        bat_emit_chrome(output, filename, &rule_left, &rule_right);
    }

    let mut line_num = 0usize;
    let mut pending = Vec::new();
    while let Some((line, _had_newline)) =
        read_next_line_from_reader(reader, &mut pending, output, "bat")?
    {
        line_num += 1;
        if !bat_in_range(line_num, line_range) {
            continue;
        }

        let display_line = if show_all { make_visible(&line) } else { line };

        let out = if show_numbers {
            format!("{line_num:>5}   {vert} {display_line}\n")
        } else {
            format!("{display_line}\n")
        };
        output.stdout(out.as_bytes());
    }

    if show_header {
        let bot_corner = "\u{2534}";
        let footer = format!("{rule_left}{bot_corner}{rule_right}\n");
        output.stdout(footer.as_bytes());
    }
    Ok(())
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
    if flags.table_mode {
        let text = match collect_input_text(ctx, args, "column") {
            Ok(text) => text,
            Err(status) => return status,
        };
        if text.is_empty() {
            return 0;
        }
        column_table_output(ctx, &text, flags.input_delim.as_ref());
    } else {
        let text = match collect_input_text(ctx, args, "column") {
            Ok(text) => text,
            Err(status) => return status,
        };
        if text.is_empty() {
            return 0;
        }
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
    use std::io::Read;
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
                stdin: Some(crate::UtilStdin::from_bytes(stdin)),
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

    fn run_stdin_reader(
        f: fn(&mut UtilContext<'_>, &[&str]) -> i32,
        argv: &[&str],
        stdin: impl Read + 'static,
        fs: &mut MemoryFs,
    ) -> (i32, String, String) {
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs,
                output: &mut output,
                cwd: "/",
                stdin: Some(crate::UtilStdin::from_reader(stdin)),
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

    struct InfiniteLinesReader;

    impl Read for InfiniteLinesReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            let pattern = b"x\n";
            for (idx, slot) in buf.iter_mut().enumerate() {
                *slot = pattern[idx % pattern.len()];
            }
            Ok(buf.len())
        }
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

    // Regression: grep must use BRE regex, not literal substring match.
    #[test]
    fn grep_bre_char_class() {
        let mut fs = make_fs_with_file("/g.txt", b"foo 123\nbar\nbaz 99\n");
        let (status, out, _) = run(
            util_grep,
            &["grep", "[0-9][0-9]*", "/g.txt"],
            &mut fs,
        );
        assert_eq!(status, 0);
        assert!(out.contains("foo 123"));
        assert!(out.contains("baz 99"));
        assert!(!out.contains("bar"));
    }

    #[test]
    fn grep_ere_alternation() {
        let mut fs = make_fs_with_file("/g.txt", b"foo\nbar\nbaz\nquux\n");
        let (status, out, _) = run(
            util_grep,
            &["grep", "-E", "foo|baz", "/g.txt"],
            &mut fs,
        );
        assert_eq!(status, 0);
        assert_eq!(out, "foo\nbaz\n");
    }

    #[test]
    fn grep_bre_escaped_brackets() {
        let mut fs = make_fs_with_file("/g.txt", b"[section]\nkey=value\n[other]\n");
        let (status, out, _) = run(
            util_grep,
            &["grep", r"\[section\]", "/g.txt"],
            &mut fs,
        );
        assert_eq!(status, 0);
        assert_eq!(out, "[section]\n");
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
    // Regression: the agent harness flagged that `sed /\[section\]/p`
    // produced no output because sed was doing literal substring matching
    // instead of BRE regex.  Verify the regex engine is actually wired up.
    // -------------------------------------------------------------------

    #[test]
    fn sed_bre_escaped_brackets_address() {
        let mut fs = make_fs_with_file(
            "/cfg.txt",
            b"[section]\nkey=value\n[other]\nkey2=value2\n",
        );
        let (status, out, _) = run(
            util_sed,
            &["sed", "-n", r"/\[section\]/p", "/cfg.txt"],
            &mut fs,
        );
        assert_eq!(status, 0);
        assert_eq!(out, "[section]\n");
    }

    #[test]
    fn sed_bre_escaped_brackets_substitute() {
        let mut fs = make_fs_with_file("/cfg.txt", b"[section]\nkey=secret\n");
        let (status, out, _) = run(
            util_sed,
            &["sed", r"s/\[section\]/[SECRET]/", "/cfg.txt"],
            &mut fs,
        );
        assert_eq!(status, 0);
        assert_eq!(out, "[SECRET]\nkey=secret\n");
    }

    #[test]
    fn sed_char_class_substitute() {
        // A real regex feature: character classes.
        let mut fs = make_fs_with_file("/nums.txt", b"foo 123 bar 42\n");
        let (status, out, _) = run(
            util_sed,
            &["sed", "s/[0-9][0-9]*/N/g", "/nums.txt"],
            &mut fs,
        );
        assert_eq!(status, 0);
        assert_eq!(out, "foo N bar N\n");
    }

    #[test]
    fn sed_ampersand_backref_in_replacement() {
        let mut fs = make_fs_with_file("/p.txt", b"foo 42 bar\n");
        let (status, out, _) = run(
            util_sed,
            &["sed", "s/[0-9][0-9]*/<&>/", "/p.txt"],
            &mut fs,
        );
        assert_eq!(status, 0);
        assert_eq!(out, "foo <42> bar\n");
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

    #[test]
    fn head_reads_file_input_via_reader_path() {
        let mut fs = make_fs_with_file("/head.txt", b"one\ntwo\nthree\nfour\n");
        let (status, out, err) = run(util_head, &["head", "-n", "2", "/head.txt"], &mut fs);
        assert_eq!(status, 0, "{err}");
        assert_eq!(out, "one\ntwo\n");
    }

    #[test]
    fn head_stops_on_streaming_stdin_without_draining() {
        let mut fs = make_fs();
        let (status, out, err) = run_stdin_reader(
            util_head,
            &["head", "-n", "3"],
            InfiniteLinesReader,
            &mut fs,
        );
        assert_eq!(status, 0, "{err}");
        assert_eq!(out, "x\nx\nx\n");
    }

    #[test]
    fn wc_counts_file_input_chunks() {
        let mut fs = make_fs_with_file("/wc.txt", b"aa\nbb\ncc\n");
        let (status, out, err) = run(util_wc, &["wc", "-l", "/wc.txt"], &mut fs);
        assert_eq!(status, 0, "{err}");
        assert_eq!(out, "      3 /wc.txt\n");
    }

    #[test]
    fn wc_counts_streaming_stdin_chunks() {
        let mut fs = make_fs();
        let (status, out, err) = run_stdin_reader(
            util_wc,
            &["wc", "-l"],
            std::io::Cursor::new(b"aa\nbb\ncc\n".to_vec()),
            &mut fs,
        );
        assert_eq!(status, 0, "{err}");
        assert_eq!(out, "      3\n");
    }

    #[test]
    fn paste_merges_file_inputs_incrementally() {
        let mut fs = make_fs_with_file("/a.txt", b"a1\na2\n");
        let h = fs.open("/b.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"b1\nb2\nb3\n").unwrap();
        fs.close(h);
        let (status, out, err) = run(util_paste, &["paste", "/a.txt", "/b.txt"], &mut fs);
        assert_eq!(status, 0, "{err}");
        assert_eq!(out, "a1\tb1\na2\tb2\n\tb3\n");
    }

    #[test]
    fn paste_serial_reads_each_file_as_stream() {
        let mut fs = make_fs_with_file("/a.txt", b"a1\na2\n");
        let h = fs.open("/b.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"b1\nb2\n").unwrap();
        fs.close(h);
        let (status, out, err) = run(
            util_paste,
            &["paste", "-s", "-d", ",", "/a.txt", "/b.txt"],
            &mut fs,
        );
        assert_eq!(status, 0, "{err}");
        assert_eq!(out, "a1,a2\nb1,b2\n");
    }

    #[test]
    fn sed_last_address_works_on_file_stream() {
        let mut fs = make_fs_with_file("/sed.txt", b"first\nmiddle\nlast\n");
        let (status, out, err) = run(util_sed, &["sed", "-n", "$p", "/sed.txt"], &mut fs);
        assert_eq!(status, 0, "{err}");
        assert_eq!(out, "last\n");
    }
}
