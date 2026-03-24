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
                    _ => {}
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
        if let Ok(h) = ctx.fs.open(&full, opts) {
            if let Err(e) = ctx.fs.write_file(h, &data) {
                emit_error(ctx.output, "tee", path, &e);
                status = 1;
            }
            ctx.fs.close(h);
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
