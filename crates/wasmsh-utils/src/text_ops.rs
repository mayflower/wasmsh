//! Text utilities: head, tail, wc, grep, sed, sort, uniq, cut, tr, tee, paste, rev, column.

use wasmsh_fs::{OpenOptions, Vfs};

use crate::helpers::{
    emit_error, get_input_text, grep_matches, parse_line_count, read_text, resolve_path,
};
use crate::{UtilContext, UtilOutput};

pub(crate) fn util_head(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (n, _from_start, files) = parse_line_count(argv, 10);
    if files.is_empty() {
        if let Some(data) = ctx.stdin {
            let text = String::from_utf8_lossy(data);
            for line in text.lines().take(n) {
                ctx.output.stdout(line.as_bytes());
                ctx.output.stdout(b"\n");
            }
            return 0;
        }
        ctx.output.stderr(b"head: missing operand\n");
        return 1;
    }
    let mut status = 0;
    for path in files {
        let full = resolve_path(ctx.cwd, path);
        match read_text(ctx.fs, &full) {
            Ok(text) => {
                for line in text.lines().take(n) {
                    ctx.output.stdout(line.as_bytes());
                    ctx.output.stdout(b"\n");
                }
            }
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

pub(crate) fn util_tail(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (n, from_start, files) = parse_line_count(argv, 10);
    if files.is_empty() {
        if let Some(data) = ctx.stdin {
            let text = String::from_utf8_lossy(data);
            tail_output(&text, n, from_start, ctx.output);
            return 0;
        }
        ctx.output.stderr(b"tail: missing operand\n");
        return 1;
    }
    let mut status = 0;
    for path in files {
        let full = resolve_path(ctx.cwd, path);
        match read_text(ctx.fs, &full) {
            Ok(text) => {
                tail_output(&text, n, from_start, ctx.output);
            }
            Err(e) => {
                emit_error(ctx.output, "tail", path, &e);
                status = 1;
            }
        }
    }
    status
}

pub(crate) fn util_wc(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    if argv.len() < 2 {
        if let Some(data) = ctx.stdin {
            let text = String::from_utf8_lossy(data);
            let lines = text.lines().count();
            let words = text.split_whitespace().count();
            let bytes = data.len();
            let out = format!("{lines:>7} {words:>7} {bytes:>7}\n");
            ctx.output.stdout(out.as_bytes());
            return 0;
        }
        ctx.output.stderr(b"wc: missing operand\n");
        return 1;
    }
    let mut status = 0;
    for path in &argv[1..] {
        let full = resolve_path(ctx.cwd, path);
        match read_text(ctx.fs, &full) {
            Ok(text) => {
                let lines = text.lines().count();
                let words = text.split_whitespace().count();
                let bytes = text.len();
                let out = format!("{lines:>7} {words:>7} {bytes:>7} {path}\n");
                ctx.output.stdout(out.as_bytes());
            }
            Err(e) => {
                emit_error(ctx.output, "wc", path, &e);
                status = 1;
            }
        }
    }
    status
}

pub(crate) fn util_grep(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut ignore_case = false;
    let mut invert = false;
    let mut count_only = false;
    let mut show_line_numbers = false;

    while let Some(arg) = args.first() {
        if arg.starts_with('-') && arg.len() > 1 {
            for c in arg[1..].chars() {
                match c {
                    'i' => ignore_case = true,
                    'v' => invert = true,
                    'c' => count_only = true,
                    'n' => show_line_numbers = true,
                    _ => {
                        let msg = format!("grep: invalid option -- '{c}'\n");
                        ctx.output.stderr(msg.as_bytes());
                        return 2;
                    }
                }
            }
            args = &args[1..];
        } else {
            break;
        }
    }

    if args.is_empty() {
        ctx.output.stderr(b"grep: missing pattern\n");
        return 2;
    }

    let pattern = args[0];
    let file_args = &args[1..];

    let text = if file_args.is_empty() {
        if let Some(data) = ctx.stdin {
            String::from_utf8_lossy(data).to_string()
        } else {
            ctx.output.stderr(b"grep: missing file operand\n");
            return 2;
        }
    } else {
        let mut combined = String::new();
        for path in file_args {
            let full = resolve_path(ctx.cwd, path);
            match read_text(ctx.fs, &full) {
                Ok(t) => combined.push_str(&t),
                Err(e) => {
                    emit_error(ctx.output, "grep", path, &e);
                    return 2;
                }
            }
        }
        combined
    };

    let mut match_count = 0u64;
    let mut found = false;

    for (i, line) in text.lines().enumerate() {
        let matches = grep_matches(line, pattern, ignore_case);
        let matches = if invert { !matches } else { matches };
        if matches {
            found = true;
            match_count += 1;
            if !count_only {
                if show_line_numbers {
                    let out = format!("{}:{}\n", i + 1, line);
                    ctx.output.stdout(out.as_bytes());
                } else {
                    ctx.output.stdout(line.as_bytes());
                    ctx.output.stdout(b"\n");
                }
            }
        }
    }

    if count_only {
        let out = format!("{match_count}\n");
        ctx.output.stdout(out.as_bytes());
    }

    i32::from(!found)
}

pub(crate) fn util_sed(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let args = &argv[1..];
    if args.is_empty() {
        ctx.output.stderr(b"sed: missing script\n");
        return 1;
    }

    let expr = args[0];
    let file_args = &args[1..];
    let text = get_input_text(ctx, file_args);
    if let Some(sub) = parse_sed_substitute(expr) {
        for line in text.lines() {
            let result = if sub.global {
                line.replace(&sub.pattern, &sub.replacement)
            } else {
                line.replacen(&sub.pattern, &sub.replacement, 1)
            };
            ctx.output.stdout(result.as_bytes());
            ctx.output.stdout(b"\n");
        }
        0
    } else {
        ctx.output.stderr(b"sed: unsupported expression\n");
        1
    }
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

pub(crate) fn util_sort(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut numeric = false;
    let mut reverse = false;

    while let Some(arg) = args.first() {
        if arg.starts_with('-') && arg.len() > 1 {
            for c in arg[1..].chars() {
                match c {
                    'n' => numeric = true,
                    'r' => reverse = true,
                    _ => {}
                }
            }
            args = &args[1..];
        } else {
            break;
        }
    }

    let text = get_input_text(ctx, args);
    let mut lines: Vec<&str> = text.lines().collect();
    if numeric {
        lines.sort_by(|a, b| {
            let na: i64 = a.trim().parse().unwrap_or(0);
            let nb: i64 = b.trim().parse().unwrap_or(0);
            na.cmp(&nb)
        });
    } else {
        lines.sort_unstable();
    }
    if reverse {
        lines.reverse();
    }

    for line in &lines {
        ctx.output.stdout(line.as_bytes());
        ctx.output.stdout(b"\n");
    }
    0
}

pub(crate) fn util_uniq(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut count = false;
    if args.first() == Some(&"-c") {
        count = true;
        args = &args[1..];
    }
    let text = get_input_text(ctx, args);
    let mut prev: Option<String> = None;
    let mut cnt: usize = 0;
    let emit = |output: &mut dyn UtilOutput, line: &str, n: usize| {
        if count {
            let s = format!("{n:>7} {line}\n");
            output.stdout(s.as_bytes());
        } else {
            output.stdout(line.as_bytes());
            output.stdout(b"\n");
        }
    };
    for line in text.lines() {
        if prev.as_deref() == Some(line) {
            cnt += 1;
        } else {
            if let Some(ref p) = prev {
                emit(ctx.output, p, cnt);
            }
            prev = Some(line.to_string());
            cnt = 1;
        }
    }
    if let Some(ref p) = prev {
        emit(ctx.output, p, cnt);
    }
    0
}

pub(crate) fn util_cut(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut delim = '\t';
    let mut fields: Vec<usize> = Vec::new();
    while let Some(arg) = args.first() {
        if *arg == "-d" && args.len() > 1 {
            delim = args[1].chars().next().unwrap_or('\t');
            args = &args[2..];
        } else if *arg == "-f" && args.len() > 1 {
            fields = args[1].split(',').filter_map(|s| s.parse().ok()).collect();
            args = &args[2..];
        } else {
            break;
        }
    }
    let text = get_input_text(ctx, args);
    for line in text.lines() {
        let parts: Vec<&str> = line.split(delim).collect();
        let selected: Vec<&str> = fields
            .iter()
            .filter_map(|&f| {
                if f > 0 {
                    parts.get(f - 1).copied()
                } else {
                    None
                }
            })
            .collect();
        ctx.output
            .stdout(selected.join(&delim.to_string()).as_bytes());
        ctx.output.stdout(b"\n");
    }
    0
}

pub(crate) fn util_tr(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let args = &argv[1..];
    if args.is_empty() {
        ctx.output.stderr(b"tr: missing operand\n");
        return 1;
    }
    let text = if let Some(data) = ctx.stdin {
        String::from_utf8_lossy(data).to_string()
    } else {
        return 1;
    };

    if args.first() == Some(&"-d") && args.len() >= 2 {
        let del_chars = args[1];
        let result: String = text.chars().filter(|c| !del_chars.contains(*c)).collect();
        ctx.output.stdout(result.as_bytes());
        return 0;
    }
    let from = args[0];
    let to = args[1];
    let from_chars: Vec<char> = from.chars().collect();
    let to_chars: Vec<char> = to.chars().collect();
    let result: String = text
        .chars()
        .map(|c| {
            if let Some(pos) = from_chars.iter().position(|&fc| fc == c) {
                to_chars.get(pos).or(to_chars.last()).copied().unwrap_or(c)
            } else {
                c
            }
        })
        .collect();
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

pub(crate) fn util_paste(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut delimiter = "\t".to_string();
    let mut serial = false;

    // Parse flags
    while let Some(arg) = args.first() {
        if *arg == "-d" && args.len() > 1 {
            delimiter = args[1].to_string();
            args = &args[2..];
        } else if *arg == "-s" {
            serial = true;
            args = &args[1..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            // Try combined flags like -sd or -ds
            let flags = &arg[1..];
            let mut consumed = true;
            for c in flags.chars() {
                match c {
                    's' => serial = true,
                    'd' => {
                        // -d requires next arg as delimiter
                        if args.len() > 1 {
                            delimiter = args[1].to_string();
                            args = &args[1..];
                        }
                    }
                    _ => {
                        consumed = false;
                        break;
                    }
                }
            }
            if consumed {
                args = &args[1..];
            } else {
                break;
            }
        } else {
            break;
        }
    }

    if args.is_empty() {
        // Read from stdin
        let text = if let Some(data) = ctx.stdin {
            String::from_utf8_lossy(data).to_string()
        } else {
            ctx.output.stderr(b"paste: missing operand\n");
            return 1;
        };
        // Just pass through stdin
        ctx.output.stdout(text.as_bytes());
        if !text.ends_with('\n') {
            ctx.output.stdout(b"\n");
        }
        return 0;
    }

    // Read all files
    let mut file_lines: Vec<Vec<String>> = Vec::new();
    for path in args {
        if *path == "-" {
            // Read from stdin
            let text = if let Some(data) = ctx.stdin {
                String::from_utf8_lossy(data).to_string()
            } else {
                String::new()
            };
            file_lines.push(text.lines().map(String::from).collect());
        } else {
            let full = resolve_path(ctx.cwd, path);
            match read_text(ctx.fs, &full) {
                Ok(text) => {
                    file_lines.push(text.lines().map(String::from).collect());
                }
                Err(e) => {
                    emit_error(ctx.output, "paste", path, &e);
                    return 1;
                }
            }
        }
    }

    if serial {
        // Serial mode: each file's lines on one output line
        for lines in &file_lines {
            let joined = lines.join(&delimiter);
            ctx.output.stdout(joined.as_bytes());
            ctx.output.stdout(b"\n");
        }
    } else {
        // Normal mode: merge corresponding lines
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

pub(crate) fn util_bat(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut show_numbers = true;
    let mut show_header = true;
    let mut line_range: Option<(Option<usize>, Option<usize>)> = None;
    let mut show_all = false;

    while let Some(arg) = args.first() {
        match *arg {
            "-n" | "--number" => {
                show_numbers = true;
                args = &args[1..];
            }
            "-p" | "--plain" => {
                show_numbers = false;
                show_header = false;
                args = &args[1..];
            }
            "-A" | "--show-all" => {
                show_all = true;
                args = &args[1..];
            }
            "-r" | "--line-range" if args.len() > 1 => {
                line_range = parse_bat_range(args[1]);
                args = &args[2..];
            }
            "-l" | "--language" if args.len() > 1 => {
                // Accepted but ignored (no highlighting in VFS)
                args = &args[2..];
            }
            "--paging" if args.len() > 1 => {
                // Accepted, no-op
                args = &args[2..];
            }
            _ if arg.starts_with("--style=") => {
                let style = &arg["--style=".len()..];
                match style {
                    "plain" => {
                        show_numbers = false;
                        show_header = false;
                    }
                    "numbers" => {
                        show_numbers = true;
                        show_header = false;
                    }
                    "header" => {
                        show_numbers = false;
                        show_header = true;
                    }
                    _ => {
                        show_numbers = true;
                        show_header = true;
                    }
                }
                args = &args[1..];
            }
            _ if arg.starts_with("--line-range=") => {
                line_range = parse_bat_range(&arg["--line-range=".len()..]);
                args = &args[1..];
            }
            _ if arg.starts_with("--paging=") => {
                // No-op
                args = &args[1..];
            }
            _ if arg.starts_with("--language=") => {
                // No-op
                args = &args[1..];
            }
            _ if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") => {
                // Combined short flags
                let flags = &arg[1..];
                let mut recognized = true;
                for ch in flags.chars() {
                    match ch {
                        'n' => show_numbers = true,
                        'p' => {
                            show_numbers = false;
                            show_header = false;
                        }
                        'A' => show_all = true,
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

    // File args
    let file_args: Vec<&str> = args.to_vec();

    if file_args.is_empty() {
        // Read from stdin
        if let Some(data) = ctx.stdin {
            let text = String::from_utf8_lossy(data).to_string();
            bat_output(
                ctx,
                None,
                &text,
                show_numbers,
                show_header,
                line_range,
                show_all,
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
                    show_numbers,
                    show_header,
                    line_range,
                    show_all,
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
    let top_corner = "\u{252C}";
    let mid_corner = "\u{253C}";
    let bot_corner = "\u{2534}";
    let vert = "\u{2502}";

    let rule_left: String = separator.repeat(7);
    let rule_right: String = separator.repeat(20);

    if show_header {
        let header_line = format!("{rule_left}{top_corner}{rule_right}\n");
        ctx.output.stdout(header_line.as_bytes());
        if let Some(name) = filename {
            let file_line = format!("       {vert} File: {name}\n");
            ctx.output.stdout(file_line.as_bytes());
        }
        let sep_line = format!("{rule_left}{mid_corner}{rule_right}\n");
        ctx.output.stdout(sep_line.as_bytes());
    }

    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let line_num = i + 1;

        // Apply line range filter
        if let Some((start, end)) = line_range {
            if let Some(s) = start {
                if line_num < s {
                    continue;
                }
            }
            if let Some(e) = end {
                if line_num > e {
                    continue;
                }
            }
        }

        let display_line = if show_all {
            make_visible(line)
        } else {
            line.to_string()
        };

        if show_numbers {
            let out = format!("{line_num:>5}   {vert} {display_line}\n");
            ctx.output.stdout(out.as_bytes());
        } else {
            let out = format!("{display_line}\n");
            ctx.output.stdout(out.as_bytes());
        }
    }

    if show_header {
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

pub(crate) fn util_column(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut table_mode = false;
    let mut input_delim: Option<String> = None;

    // Parse flags
    while let Some(arg) = args.first() {
        if *arg == "-t" {
            table_mode = true;
            args = &args[1..];
        } else if *arg == "-s" && args.len() > 1 {
            input_delim = Some(args[1].to_string());
            args = &args[2..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            args = &args[1..];
        } else {
            break;
        }
    }

    let text = get_input_text(ctx, args);
    if text.is_empty() {
        return 0;
    }

    if table_mode {
        // Split each line into fields and align columns
        let rows: Vec<Vec<&str>> = text
            .lines()
            .filter(|l| !l.is_empty())
            .map(|line| {
                if let Some(ref d) = input_delim {
                    line.split(d.as_str()).collect()
                } else {
                    line.split_whitespace().collect()
                }
            })
            .collect();

        if rows.is_empty() {
            return 0;
        }

        // Compute maximum width for each column
        let max_cols = rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut col_widths = vec![0usize; max_cols];
        for row in &rows {
            for (i, field) in row.iter().enumerate() {
                col_widths[i] = col_widths[i].max(field.len());
            }
        }

        // Output aligned table
        for row in &rows {
            let mut line = String::new();
            for (i, field) in row.iter().enumerate() {
                if i > 0 {
                    line.push_str("  ");
                }
                if i < row.len() - 1 {
                    // Left-align and pad
                    line.push_str(field);
                    let padding = col_widths[i].saturating_sub(field.len());
                    for _ in 0..padding {
                        line.push(' ');
                    }
                } else {
                    // Last column: no trailing padding
                    line.push_str(field);
                }
            }
            line.push('\n');
            ctx.output.stdout(line.as_bytes());
        }
    } else {
        // Simple mode: just pass through
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
        assert!(out.contains("1"), "expected line number 1");
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
        assert!(out.contains("1"));
        assert!(out.contains("2"));
        assert!(out.contains("3"));
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
        assert!(out.contains("c"));
        assert!(out.contains("d"));
        assert!(out.contains("e"));
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
}
