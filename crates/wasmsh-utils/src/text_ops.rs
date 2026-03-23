//! Text utilities: head, tail, wc, grep, sed, sort, uniq, cut, tr, tee.

use wasmsh_fs::{OpenOptions, Vfs};

use crate::helpers::*;
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

    if found { 0 } else { 1 }
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
        lines.sort();
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
    if args.first() == Some(&"-c") { count = true; args = &args[1..]; }
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
        } else { break; }
    }
    let text = get_input_text(ctx, args);
    for line in text.lines() {
        let parts: Vec<&str> = line.split(delim).collect();
        let selected: Vec<&str> = fields.iter()
            .filter_map(|&f| if f > 0 { parts.get(f - 1).copied() } else { None })
            .collect();
        ctx.output.stdout(selected.join(&delim.to_string()).as_bytes());
        ctx.output.stdout(b"\n");
    }
    0
}

pub(crate) fn util_tr(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let args = &argv[1..];
    if args.len() < 2 {
        if args.first() == Some(&"-d") && args.len() >= 2 {
            // delete mode handled below
        } else {
            ctx.output.stderr(b"tr: missing operand\n");
            return 1;
        }
    }
    let text = if let Some(data) = ctx.stdin {
        String::from_utf8_lossy(data).to_string()
    } else { return 1; };

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
    let result: String = text.chars().map(|c| {
        if let Some(pos) = from_chars.iter().position(|&fc| fc == c) {
            to_chars.get(pos).or(to_chars.last()).copied().unwrap_or(c)
        } else { c }
    }).collect();
    ctx.output.stdout(result.as_bytes());
    0
}

pub(crate) fn util_tee(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut append = false;
    if args.first() == Some(&"-a") { append = true; args = &args[1..]; }
    let data = if let Some(d) = ctx.stdin { d.to_vec() } else { Vec::new() };
    ctx.output.stdout(&data);
    for path in args {
        let full = resolve_path(ctx.cwd, path);
        let opts = if append { OpenOptions::append() } else { OpenOptions::write() };
        if let Ok(h) = ctx.fs.open(&full, opts) {
            let _ = ctx.fs.write_file(h, &data);
            ctx.fs.close(h);
        }
    }
    0
}
