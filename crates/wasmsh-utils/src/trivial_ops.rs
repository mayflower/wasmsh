//! Trivial utilities: which, rmdir, tac, nl, shuf, cmp, comm, fold, nproc, expand, unexpand,
//! truncate, factor, strings, cksum, tsort, install, timeout, cal.

use std::sync::atomic::{AtomicU64, Ordering};

use wasmsh_fs::{OpenOptions, Vfs};

use crate::helpers::{copy_file_contents, emit_error, get_input_text, read_text, resolve_path};
use crate::UtilContext;

// ---------------------------------------------------------------------------
// XorShift64 PRNG (shared with shuf)
// ---------------------------------------------------------------------------

struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0xDEAD_BEEF } else { seed },
        }
    }

    fn next(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }
}

/// Global counter for PRNG seeding.
static SHUF_COUNTER: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------------------
// Known command set for `which`
// ---------------------------------------------------------------------------

/// Commands recognized by `which`. This includes all utilities registered in the
/// `UtilRegistry` plus well-known builtins.
const KNOWN_COMMANDS: &[&str] = &[
    // File utilities
    "cat",
    "ls",
    "mkdir",
    "rm",
    "touch",
    "mv",
    "cp",
    "ln",
    "readlink",
    "realpath",
    "stat",
    "find",
    "chmod",
    "mktemp",
    // Text utilities
    "head",
    "tail",
    "wc",
    "grep",
    "sed",
    "sort",
    "uniq",
    "cut",
    "tr",
    "tee",
    "paste",
    "rev",
    "column",
    // Data/string utilities
    "seq",
    "basename",
    "dirname",
    "expr",
    "xargs",
    "yes",
    "md5sum",
    "sha256sum",
    "base64",
    // System/env utilities
    "env",
    "printenv",
    "id",
    "whoami",
    "uname",
    "hostname",
    "sleep",
    "date",
    // Trivial utilities (this file)
    "which",
    "rmdir",
    "tac",
    "nl",
    "shuf",
    "cmp",
    "comm",
    "fold",
    "nproc",
    "expand",
    "unexpand",
    "truncate",
    "factor",
    "strings",
    "cksum",
    "tsort",
    "install",
    "timeout",
    "cal",
    // Common builtins (not in registry but useful to resolve)
    "echo",
    "printf",
    "read",
    "cd",
    "pwd",
    "export",
    "unset",
    "set",
    "test",
    "[",
    "true",
    "false",
    "exit",
    "return",
    "shift",
    "source",
    ".",
    "eval",
    "exec",
    "type",
    "alias",
    "unalias",
    "declare",
    "typeset",
    "local",
    "readonly",
    "let",
    "mapfile",
    "readarray",
    "builtin",
    "command",
    "getopts",
    "trap",
    "wait",
    "jobs",
    "kill",
    "history",
    "shopt",
    "select",
    "break",
    "continue",
    "case",
];

fn is_known_command(name: &str) -> bool {
    KNOWN_COMMANDS.contains(&name)
}

// ---------------------------------------------------------------------------
// which
// ---------------------------------------------------------------------------

pub(crate) fn util_which(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut show_all = false;

    while let Some(arg) = args.first() {
        if *arg == "-a" {
            show_all = true;
            args = &args[1..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            args = &args[1..];
        } else {
            break;
        }
    }

    if args.is_empty() {
        ctx.output.stderr(b"which: missing operand\n");
        return 1;
    }

    let mut status = 0;
    for name in args {
        if is_known_command(name) {
            let line = format!("/usr/bin/{name}\n");
            ctx.output.stdout(line.as_bytes());
            // With -a we only have one location anyway, so nothing extra to print
            let _ = show_all;
        } else {
            let msg = format!("which: no {name} in (/usr/bin)\n");
            ctx.output.stderr(msg.as_bytes());
            status = 1;
        }
    }
    status
}

// ---------------------------------------------------------------------------
// rmdir
// ---------------------------------------------------------------------------

pub(crate) fn util_rmdir(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut parents = false;

    while let Some(arg) = args.first() {
        if *arg == "-p" || *arg == "--parents" {
            parents = true;
            args = &args[1..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            args = &args[1..];
        } else {
            break;
        }
    }

    if args.is_empty() {
        ctx.output.stderr(b"rmdir: missing operand\n");
        return 1;
    }

    let mut status = 0;
    for path in args {
        let full = resolve_path(ctx.cwd, path);
        if let Err(msg) = rmdir_one(ctx, &full, path) {
            ctx.output.stderr(msg.as_bytes());
            status = 1;
            continue;
        }

        if parents {
            // Remove parent directories as long as they are empty
            let mut current = full.clone();
            loop {
                let parent = match current.rfind('/') {
                    Some(0) | None => break, // reached root
                    Some(pos) => &current[..pos],
                };
                if parent.is_empty() {
                    break;
                }
                // Check if parent is empty
                match ctx.fs.read_dir(parent) {
                    Ok(entries) if entries.is_empty() => {
                        if ctx.fs.remove_dir(parent).is_err() {
                            break;
                        }
                        current = parent.to_string();
                    }
                    _ => break,
                }
            }
        }
    }
    status
}

fn rmdir_one(ctx: &mut UtilContext<'_>, full: &str, display: &str) -> Result<(), String> {
    // Check that it exists and is a directory
    match ctx.fs.stat(full) {
        Ok(meta) if !meta.is_dir => {
            return Err(format!("rmdir: '{display}': Not a directory\n"));
        }
        Ok(_) => {}
        Err(e) => {
            return Err(format!("rmdir: '{display}': {e}\n"));
        }
    }

    // Check that the directory is empty
    match ctx.fs.read_dir(full) {
        Ok(entries) if !entries.is_empty() => {
            return Err(format!("rmdir: '{display}': Directory not empty\n"));
        }
        Err(e) => {
            return Err(format!("rmdir: '{display}': {e}\n"));
        }
        Ok(_) => {}
    }

    if let Err(e) = ctx.fs.remove_dir(full) {
        return Err(format!("rmdir: '{display}': {e}\n"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// tac — reverse cat
// ---------------------------------------------------------------------------

pub(crate) fn util_tac(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let file_args = &argv[1..];
    let text = get_input_text(ctx, file_args);
    if text.is_empty() && file_args.is_empty() && ctx.stdin.is_none() {
        ctx.output.stderr(b"tac: missing operand\n");
        return 1;
    }
    let lines: Vec<&str> = text.lines().collect();
    for line in lines.iter().rev() {
        ctx.output.stdout(line.as_bytes());
        ctx.output.stdout(b"\n");
    }
    0
}

// ---------------------------------------------------------------------------
// nl — number lines
// ---------------------------------------------------------------------------

pub(crate) fn util_nl(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut number_all = false; // -b a: number all lines; -b t (default): non-empty only

    while let Some(arg) = args.first() {
        if *arg == "-b" && args.len() > 1 {
            match args[1] {
                "a" => number_all = true,
                "t" => number_all = false,
                _ => {}
            }
            args = &args[2..];
        } else if *arg == "-ba" {
            number_all = true;
            args = &args[1..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            args = &args[1..];
        } else {
            break;
        }
    }

    let text = get_input_text(ctx, args);
    if text.is_empty() && args.is_empty() && ctx.stdin.is_none() {
        ctx.output.stderr(b"nl: missing operand\n");
        return 1;
    }

    let mut line_num: u64 = 0;
    for line in text.lines() {
        let is_empty = line.is_empty();
        if number_all || !is_empty {
            line_num += 1;
            let out = format!("{line_num:>6}\t{line}\n");
            ctx.output.stdout(out.as_bytes());
        } else {
            // Blank line — output without numbering
            ctx.output.stdout(b"\n");
        }
    }
    0
}

// ---------------------------------------------------------------------------
// shuf — shuffle lines
// ---------------------------------------------------------------------------

pub(crate) fn util_shuf(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut count: Option<usize> = None;

    while let Some(arg) = args.first() {
        if *arg == "-n" && args.len() > 1 {
            count = args[1].parse().ok();
            args = &args[2..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            args = &args[1..];
        } else {
            break;
        }
    }

    let text = get_input_text(ctx, args);
    if text.is_empty() && args.is_empty() && ctx.stdin.is_none() {
        ctx.output.stderr(b"shuf: missing operand\n");
        return 1;
    }

    let mut lines: Vec<&str> = text.lines().collect();
    let len = lines.len();
    if len == 0 {
        return 0;
    }

    // Fisher-Yates shuffle
    let seed = SHUF_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut rng = XorShift64::new(seed.wrapping_mul(0x517C_C1B7_2722_0A95));

    for i in (1..len).rev() {
        let j = (rng.next() % (i as u64 + 1)) as usize;
        lines.swap(i, j);
    }

    let limit = count.unwrap_or(len).min(len);
    for line in &lines[..limit] {
        ctx.output.stdout(line.as_bytes());
        ctx.output.stdout(b"\n");
    }
    0
}

// ---------------------------------------------------------------------------
// cmp — byte-by-byte file comparison
// ---------------------------------------------------------------------------

pub(crate) fn util_cmp(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut silent = false;
    let mut verbose = false;

    while let Some(arg) = args.first() {
        if *arg == "-s" || *arg == "--silent" || *arg == "--quiet" {
            silent = true;
            args = &args[1..];
        } else if *arg == "-l" {
            verbose = true;
            args = &args[1..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            args = &args[1..];
        } else {
            break;
        }
    }

    if args.len() < 2 {
        ctx.output.stderr(b"cmp: missing operand\n");
        return 2;
    }

    let path1 = resolve_path(ctx.cwd, args[0]);
    let path2 = resolve_path(ctx.cwd, args[1]);

    let Some(data1) = read_file_bytes(ctx, &path1, args[0]) else {
        return 2;
    };
    let Some(data2) = read_file_bytes(ctx, &path2, args[1]) else {
        return 2;
    };

    let min_len = data1.len().min(data2.len());
    let mut differ = false;

    for i in 0..min_len {
        if data1[i] != data2[i] {
            differ = true;
            if silent {
                return 1;
            }
            if verbose {
                let out = format!("{:>4} {:>3} {:>3}\n", i + 1, data1[i], data2[i]);
                ctx.output.stdout(out.as_bytes());
            } else {
                #[allow(clippy::naive_bytecount)]
                let line_num = data1[..i].iter().filter(|&&b| b == b'\n').count() + 1;
                let out = format!(
                    "{} {} differ: byte {}, line {line_num}\n",
                    args[0],
                    args[1],
                    i + 1
                );
                ctx.output.stdout(out.as_bytes());
                return 1;
            }
        }
    }

    if data1.len() != data2.len() {
        if !silent {
            let shorter = if data1.len() < data2.len() {
                args[0]
            } else {
                args[1]
            };
            let msg = format!("cmp: EOF on {shorter}\n");
            ctx.output.stderr(msg.as_bytes());
        }
        return 1;
    }

    i32::from(differ)
}

fn read_file_bytes(ctx: &mut UtilContext<'_>, full: &str, display: &str) -> Option<Vec<u8>> {
    match ctx.fs.open(full, OpenOptions::read()) {
        Ok(h) => {
            let result = ctx.fs.read_file(h);
            ctx.fs.close(h);
            match result {
                Ok(data) => Some(data),
                Err(e) => {
                    emit_error(ctx.output, "cmp", display, &e);
                    None
                }
            }
        }
        Err(e) => {
            emit_error(ctx.output, "cmp", display, &e);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// comm — compare two sorted files
// ---------------------------------------------------------------------------

pub(crate) fn util_comm(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut suppress1 = false;
    let mut suppress2 = false;
    let mut suppress3 = false;

    while let Some(arg) = args.first() {
        if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") {
            for ch in arg[1..].chars() {
                match ch {
                    '1' => suppress1 = true,
                    '2' => suppress2 = true,
                    '3' => suppress3 = true,
                    _ => {}
                }
            }
            args = &args[1..];
        } else {
            break;
        }
    }

    if args.len() < 2 {
        ctx.output.stderr(b"comm: missing operand\n");
        return 1;
    }

    let full1 = resolve_path(ctx.cwd, args[0]);
    let full2 = resolve_path(ctx.cwd, args[1]);

    let text1 = match read_text(ctx.fs, &full1) {
        Ok(t) => t,
        Err(e) => {
            emit_error(ctx.output, "comm", args[0], &e);
            return 1;
        }
    };
    let text2 = match read_text(ctx.fs, &full2) {
        Ok(t) => t,
        Err(e) => {
            emit_error(ctx.output, "comm", args[1], &e);
            return 1;
        }
    };

    let lines1: Vec<&str> = text1.lines().collect();
    let lines2: Vec<&str> = text2.lines().collect();
    let mut i = 0;
    let mut j = 0;

    // Compute column prefixes based on suppression flags
    // Column 1 (only in file1): no prefix / suppressed
    // Column 2 (only in file2): one tab / suppressed
    // Column 3 (both): two tabs / suppressed
    let col2_prefix = if suppress1 { "" } else { "\t" };
    let col3_prefix = if suppress1 && suppress2 {
        ""
    } else if suppress1 || suppress2 {
        "\t"
    } else {
        "\t\t"
    };

    while i < lines1.len() && j < lines2.len() {
        match lines1[i].cmp(lines2[j]) {
            std::cmp::Ordering::Less => {
                if !suppress1 {
                    ctx.output.stdout(lines1[i].as_bytes());
                    ctx.output.stdout(b"\n");
                }
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                if !suppress2 {
                    ctx.output.stdout(col2_prefix.as_bytes());
                    ctx.output.stdout(lines2[j].as_bytes());
                    ctx.output.stdout(b"\n");
                }
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                if !suppress3 {
                    ctx.output.stdout(col3_prefix.as_bytes());
                    ctx.output.stdout(lines1[i].as_bytes());
                    ctx.output.stdout(b"\n");
                }
                i += 1;
                j += 1;
            }
        }
    }

    // Remaining lines in file1
    while i < lines1.len() {
        if !suppress1 {
            ctx.output.stdout(lines1[i].as_bytes());
            ctx.output.stdout(b"\n");
        }
        i += 1;
    }

    // Remaining lines in file2
    while j < lines2.len() {
        if !suppress2 {
            ctx.output.stdout(col2_prefix.as_bytes());
            ctx.output.stdout(lines2[j].as_bytes());
            ctx.output.stdout(b"\n");
        }
        j += 1;
    }

    0
}

// ---------------------------------------------------------------------------
// fold — wrap lines to width
// ---------------------------------------------------------------------------

pub(crate) fn util_fold(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut width: usize = 80;
    let mut break_at_spaces = false;

    while let Some(arg) = args.first() {
        if *arg == "-w" && args.len() > 1 {
            width = args[1].parse().unwrap_or(80);
            args = &args[2..];
        } else if *arg == "-s" {
            break_at_spaces = true;
            args = &args[1..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            // Try -wN combined form
            if let Some(rest) = arg.strip_prefix("-w") {
                if let Ok(w) = rest.parse::<usize>() {
                    width = w;
                }
            }
            args = &args[1..];
        } else {
            break;
        }
    }

    if width == 0 {
        width = 80;
    }

    let text = get_input_text(ctx, args);
    if text.is_empty() && args.is_empty() && ctx.stdin.is_none() {
        ctx.output.stderr(b"fold: missing operand\n");
        return 1;
    }

    for line in text.lines() {
        if line.len() <= width {
            ctx.output.stdout(line.as_bytes());
            ctx.output.stdout(b"\n");
            continue;
        }

        if break_at_spaces {
            fold_at_spaces(ctx, line, width);
        } else {
            // Hard break at width
            let bytes = line.as_bytes();
            let mut pos = 0;
            while pos < bytes.len() {
                let end = (pos + width).min(bytes.len());
                ctx.output.stdout(&bytes[pos..end]);
                ctx.output.stdout(b"\n");
                pos = end;
            }
        }
    }
    0
}

fn fold_at_spaces(ctx: &mut UtilContext<'_>, line: &str, width: usize) {
    let bytes = line.as_bytes();
    let mut pos = 0;

    while pos < bytes.len() {
        if pos + width >= bytes.len() {
            ctx.output.stdout(&bytes[pos..]);
            ctx.output.stdout(b"\n");
            break;
        }

        // Find the last space within the width
        let segment = &bytes[pos..pos + width];
        let break_pos = match segment.iter().rposition(|&b| b == b' ') {
            Some(sp) => sp,
            None => width, // No space found, hard break
        };

        ctx.output.stdout(&bytes[pos..pos + break_pos]);
        ctx.output.stdout(b"\n");

        pos += break_pos;
        // Skip the space if we broke at one
        if pos < bytes.len() && bytes[pos] == b' ' {
            pos += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// nproc
// ---------------------------------------------------------------------------

pub(crate) fn util_nproc(ctx: &mut UtilContext<'_>, _argv: &[&str]) -> i32 {
    // In WASM, always report 1 processor
    ctx.output.stdout(b"1\n");
    0
}

// ---------------------------------------------------------------------------
// expand — tabs to spaces
// ---------------------------------------------------------------------------

pub(crate) fn util_expand(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut tab_width: usize = 8;

    while let Some(arg) = args.first() {
        if *arg == "-t" && args.len() > 1 {
            tab_width = args[1].parse().unwrap_or(8);
            args = &args[2..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            if let Some(rest) = arg.strip_prefix("-t") {
                if let Ok(w) = rest.parse::<usize>() {
                    tab_width = w;
                }
            }
            args = &args[1..];
        } else {
            break;
        }
    }

    if tab_width == 0 {
        tab_width = 8;
    }

    let text = get_input_text(ctx, args);
    if text.is_empty() && args.is_empty() && ctx.stdin.is_none() {
        return 0;
    }

    for line in text.lines() {
        let mut col = 0;
        let mut out = String::new();
        for ch in line.chars() {
            if ch == '\t' {
                let spaces = tab_width - (col % tab_width);
                for _ in 0..spaces {
                    out.push(' ');
                }
                col += spaces;
            } else {
                out.push(ch);
                col += 1;
            }
        }
        out.push('\n');
        ctx.output.stdout(out.as_bytes());
    }
    0
}

// ---------------------------------------------------------------------------
// unexpand — spaces to tabs
// ---------------------------------------------------------------------------

pub(crate) fn util_unexpand(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut tab_width: usize = 8;
    let mut all = false;

    while let Some(arg) = args.first() {
        if *arg == "-t" && args.len() > 1 {
            tab_width = args[1].parse().unwrap_or(8);
            args = &args[2..];
        } else if *arg == "-a" || *arg == "--all" {
            all = true;
            args = &args[1..];
        } else if *arg == "--first-only" {
            all = false;
            args = &args[1..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            if let Some(rest) = arg.strip_prefix("-t") {
                if let Ok(w) = rest.parse::<usize>() {
                    tab_width = w;
                }
            }
            args = &args[1..];
        } else {
            break;
        }
    }

    if tab_width == 0 {
        tab_width = 8;
    }

    let text = get_input_text(ctx, args);
    if text.is_empty() && args.is_empty() && ctx.stdin.is_none() {
        return 0;
    }

    for line in text.lines() {
        let out = if all {
            unexpand_line(line, tab_width)
        } else {
            unexpand_leading(line, tab_width)
        };
        ctx.output.stdout(out.as_bytes());
        ctx.output.stdout(b"\n");
    }
    0
}

fn unexpand_leading(line: &str, tab_width: usize) -> String {
    let mut out = String::new();
    let mut col = 0;
    let mut in_leading = true;

    for ch in line.chars() {
        if in_leading && ch == ' ' {
            col += 1;
            if col % tab_width == 0 {
                out.push('\t');
            }
        } else {
            if in_leading {
                // Emit remaining spaces that didn't fill a tab stop
                let remaining = col % tab_width;
                for _ in 0..remaining {
                    out.push(' ');
                }
                in_leading = false;
            }
            out.push(ch);
        }
    }

    if in_leading {
        let remaining = col % tab_width;
        for _ in 0..remaining {
            out.push(' ');
        }
    }

    out
}

fn unexpand_line(line: &str, tab_width: usize) -> String {
    let mut out = String::new();
    let mut col = 0;
    let mut space_count = 0;

    for ch in line.chars() {
        if ch == ' ' {
            space_count += 1;
            col += 1;
            if col % tab_width == 0 && space_count > 1 {
                out.push('\t');
                space_count = 0;
            }
        } else {
            // Flush remaining spaces
            for _ in 0..space_count {
                out.push(' ');
            }
            space_count = 0;
            out.push(ch);
            col += 1;
        }
    }

    // Flush trailing spaces
    for _ in 0..space_count {
        out.push(' ');
    }

    out
}

// ---------------------------------------------------------------------------
// truncate — set file size
// ---------------------------------------------------------------------------

pub(crate) fn util_truncate(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut size_spec: Option<&str> = None;

    while let Some(arg) = args.first() {
        if *arg == "-s" && args.len() > 1 {
            size_spec = Some(args[1]);
            args = &args[2..];
        } else if let Some(rest) = arg.strip_prefix("-s") {
            if !rest.is_empty() {
                size_spec = Some(rest);
            }
            args = &args[1..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            args = &args[1..];
        } else {
            break;
        }
    }

    let Some(spec) = size_spec else {
        ctx.output.stderr(b"truncate: missing -s option\n");
        return 1;
    };

    if args.is_empty() {
        ctx.output.stderr(b"truncate: missing file operand\n");
        return 1;
    }

    // Parse size specification: optional +/- prefix, then number
    let (mode, num_str) = if let Some(rest) = spec.strip_prefix('+') {
        ('+', rest)
    } else if let Some(rest) = spec.strip_prefix('-') {
        ('-', rest)
    } else {
        ('=', spec)
    };

    let Ok(size_val) = num_str.parse::<u64>() else {
        let msg = format!("truncate: invalid size: '{spec}'\n");
        ctx.output.stderr(msg.as_bytes());
        return 1;
    };

    let mut status = 0;
    for path in args {
        let full = resolve_path(ctx.cwd, path);

        // Read current contents (or empty if file doesn't exist)
        let current_data = match ctx.fs.open(&full, OpenOptions::read()) {
            Ok(h) => {
                let data = ctx.fs.read_file(h).unwrap_or_default();
                ctx.fs.close(h);
                data
            }
            Err(_) => Vec::new(),
        };

        let new_size = match mode {
            '+' => (current_data.len() as u64).saturating_add(size_val) as usize,
            '-' => (current_data.len() as u64).saturating_sub(size_val) as usize,
            _ => size_val as usize,
        };

        let mut new_data = current_data;
        if new_size < new_data.len() {
            new_data.truncate(new_size);
        } else {
            new_data.resize(new_size, 0);
        }

        match ctx.fs.open(&full, OpenOptions::write()) {
            Ok(h) => {
                if let Err(e) = ctx.fs.write_file(h, &new_data) {
                    emit_error(ctx.output, "truncate", path, &e);
                    status = 1;
                }
                ctx.fs.close(h);
            }
            Err(e) => {
                emit_error(ctx.output, "truncate", path, &e);
                status = 1;
            }
        }
    }
    status
}

// ---------------------------------------------------------------------------
// factor — prime factorization
// ---------------------------------------------------------------------------

pub(crate) fn util_factor(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let args = &argv[1..];

    // Read from arguments or stdin
    let numbers: Vec<String> = if args.is_empty() {
        if let Some(data) = ctx.stdin {
            let text = String::from_utf8_lossy(data);
            text.split_whitespace().map(String::from).collect()
        } else {
            ctx.output.stderr(b"factor: missing operand\n");
            return 1;
        }
    } else {
        args.iter().map(|s| (*s).to_string()).collect()
    };

    let mut status = 0;
    for num_str in &numbers {
        let Ok(mut n) = num_str.parse::<u64>() else {
            let msg = format!("factor: '{num_str}' is not a valid positive integer\n");
            ctx.output.stderr(msg.as_bytes());
            status = 1;
            continue;
        };

        let mut factors = Vec::new();
        let mut d = 2u64;

        while d.saturating_mul(d) <= n {
            while n % d == 0 {
                factors.push(d);
                n /= d;
            }
            d += 1;
        }
        if n > 1 {
            factors.push(n);
        }

        let orig: u64 = num_str.parse().unwrap_or(0);
        let factors_str: Vec<String> = factors.iter().map(ToString::to_string).collect();
        let out = format!("{orig}: {}\n", factors_str.join(" "));
        ctx.output.stdout(out.as_bytes());
    }
    status
}

// ---------------------------------------------------------------------------
// cksum — CRC-32 checksum (ISO 3309, polynomial 0xEDB88320)
// ---------------------------------------------------------------------------

/// Build CRC-32 lookup table at compile time.
const fn build_crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0u32;
    while i < 256 {
        let mut crc = i;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i as usize] = crc;
        i += 1;
    }
    table
}

const CRC32_TABLE: [u32; 256] = build_crc32_table();

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        let index = ((crc ^ u32::from(byte)) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC32_TABLE[index];
    }
    !crc
}

pub(crate) fn util_cksum(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let file_args = &argv[1..];

    if file_args.is_empty() {
        // Read from stdin
        let data = if let Some(d) = ctx.stdin {
            d.to_vec()
        } else {
            Vec::new()
        };
        let checksum = crc32(&data);
        let out = format!("{checksum} {}\n", data.len());
        ctx.output.stdout(out.as_bytes());
        return 0;
    }

    let mut status = 0;
    for path in file_args {
        let full = resolve_path(ctx.cwd, path);
        match ctx.fs.open(&full, OpenOptions::read()) {
            Ok(h) => {
                match ctx.fs.read_file(h) {
                    Ok(data) => {
                        let checksum = crc32(&data);
                        let out = format!("{checksum} {} {path}\n", data.len());
                        ctx.output.stdout(out.as_bytes());
                    }
                    Err(e) => {
                        emit_error(ctx.output, "cksum", path, &e);
                        status = 1;
                    }
                }
                ctx.fs.close(h);
            }
            Err(e) => {
                emit_error(ctx.output, "cksum", path, &e);
                status = 1;
            }
        }
    }
    status
}

// ---------------------------------------------------------------------------
// tsort — topological sort
// ---------------------------------------------------------------------------

pub(crate) fn util_tsort(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let file_args = &argv[1..];
    let text = get_input_text(ctx, file_args);
    if text.is_empty() && file_args.is_empty() && ctx.stdin.is_none() {
        ctx.output.stderr(b"tsort: missing operand\n");
        return 1;
    }

    let tokens: Vec<&str> = text.split_whitespace().collect();
    if !tokens.len().is_multiple_of(2) {
        ctx.output.stderr(b"tsort: odd number of tokens\n");
        return 1;
    }

    // Build adjacency list using indices for efficiency
    let mut node_index: Vec<String> = Vec::new();
    let mut node_map = std::collections::HashMap::<String, usize>::new();

    let get_or_insert = |name: &str,
                         idx: &mut Vec<String>,
                         map: &mut std::collections::HashMap<String, usize>|
     -> usize {
        if let Some(&i) = map.get(name) {
            i
        } else {
            let i = idx.len();
            idx.push(name.to_string());
            map.insert(name.to_string(), i);
            i
        }
    };

    // First pass: collect all nodes
    let mut edges: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let from = get_or_insert(tokens[i], &mut node_index, &mut node_map);
        let to = get_or_insert(tokens[i + 1], &mut node_index, &mut node_map);
        if from != to {
            edges.push((from, to));
        }
        i += 2;
    }

    let n = node_index.len();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut in_degree: Vec<usize> = vec![0; n];

    for &(from, to) in &edges {
        adj[from].push(to);
        in_degree[to] += 1;
    }

    // Kahn's algorithm
    let mut queue: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    for (idx, &deg) in in_degree.iter().enumerate() {
        if deg == 0 {
            queue.push_back(idx);
        }
    }

    let mut result: Vec<usize> = Vec::with_capacity(n);
    while let Some(node) = queue.pop_front() {
        result.push(node);
        for &neighbor in &adj[node] {
            in_degree[neighbor] -= 1;
            if in_degree[neighbor] == 0 {
                queue.push_back(neighbor);
            }
        }
    }

    if result.len() != n {
        ctx.output.stderr(b"tsort: input contains a cycle\n");
        // Still output what we can
        for idx in &result {
            ctx.output.stdout(node_index[*idx].as_bytes());
            ctx.output.stdout(b"\n");
        }
        return 1;
    }

    for idx in &result {
        ctx.output.stdout(node_index[*idx].as_bytes());
        ctx.output.stdout(b"\n");
    }
    0
}

// ---------------------------------------------------------------------------
// install — copy files with directory creation
// ---------------------------------------------------------------------------

pub(crate) fn util_install(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut dir_mode = false;

    while let Some(arg) = args.first() {
        if *arg == "-d" {
            dir_mode = true;
            args = &args[1..];
        } else if *arg == "-m" && args.len() > 1 {
            // Mode is ignored in VFS, but consume the argument
            args = &args[2..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            args = &args[1..];
        } else {
            break;
        }
    }

    if args.is_empty() {
        ctx.output.stderr(b"install: missing operand\n");
        return 1;
    }

    if dir_mode {
        // install -d DIR ... — create directories (like mkdir -p)
        let mut status = 0;
        for path in args {
            let full = resolve_path(ctx.cwd, path);
            if let Err(e) = create_dir_parents(ctx, &full) {
                let msg = format!("install: cannot create directory '{path}': {e}\n");
                ctx.output.stderr(msg.as_bytes());
                status = 1;
            }
        }
        return status;
    }

    if args.len() < 2 {
        ctx.output.stderr(b"install: missing destination operand\n");
        return 1;
    }

    let src = resolve_path(ctx.cwd, args[0]);
    let dst = resolve_path(ctx.cwd, args[args.len() - 1]);

    // Ensure parent directory of destination exists
    if let Some(pos) = dst.rfind('/') {
        let parent = &dst[..pos];
        if !parent.is_empty() && ctx.fs.stat(parent).is_err() {
            if let Err(e) = create_dir_parents(ctx, parent) {
                let msg = format!("install: cannot create directory: {e}\n");
                ctx.output.stderr(msg.as_bytes());
                return 1;
            }
        }
    }

    if let Err(e) = copy_file_contents(ctx.fs, &src, &dst) {
        emit_error(ctx.output, "install", args[0], &e);
        return 1;
    }
    0
}

fn create_dir_parents(ctx: &mut UtilContext<'_>, path: &str) -> Result<(), String> {
    // Build up path components and create each one
    let mut current = String::new();
    for component in path.split('/') {
        if component.is_empty() {
            current.push('/');
            continue;
        }
        if current.len() > 1 {
            current.push('/');
        }
        current.push_str(component);

        match ctx.fs.stat(&current) {
            Ok(meta) if meta.is_dir => {}
            Ok(_) => return Err(format!("'{current}' exists but is not a directory")),
            Err(_) => {
                ctx.fs.create_dir(&current).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// timeout — conceptual pass-through
// ---------------------------------------------------------------------------

pub(crate) fn util_timeout(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];

    // Skip flags
    while let Some(arg) = args.first() {
        if arg.starts_with('-') && arg.len() > 1 {
            // Skip flags like --signal, -k, etc. with their values
            if (*arg == "-s" || *arg == "--signal" || *arg == "-k" || *arg == "--kill-after")
                && args.len() > 1
            {
                args = &args[2..];
            } else {
                args = &args[1..];
            }
        } else {
            break;
        }
    }

    // First positional arg is the duration (skip it), rest is the command
    if args.is_empty() {
        ctx.output.stderr(b"timeout: missing operand\n");
        return 1;
    }

    // Skip the duration argument
    args = &args[1..];

    if args.is_empty() {
        ctx.output.stderr(b"timeout: missing command\n");
        return 1;
    }

    // Output the command that would be executed
    // (actual timeout enforcement is at the VM level via step_budget)
    let cmd = args.join(" ");
    let out = format!("{cmd}\n");
    ctx.output.stdout(out.as_bytes());
    0
}

// ---------------------------------------------------------------------------
// cal — simple calendar
// ---------------------------------------------------------------------------

pub(crate) fn util_cal(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let args = &argv[1..];

    // Parse optional month and year arguments
    let (month, year) = match args.len() {
        0 => {
            // Default: January 2026 (deterministic for sandbox)
            (1u32, 2026u32)
        }
        1 => {
            // Just year
            match args[0].parse::<u32>() {
                Ok(y) if y >= 1 => (1, y),
                _ => {
                    let msg = format!("cal: invalid year '{}'\n", args[0]);
                    ctx.output.stderr(msg.as_bytes());
                    return 1;
                }
            }
        }
        _ => {
            // month year
            let m = match args[0].parse::<u32>() {
                Ok(m) if (1..=12).contains(&m) => m,
                _ => {
                    let msg = format!("cal: invalid month '{}'\n", args[0]);
                    ctx.output.stderr(msg.as_bytes());
                    return 1;
                }
            };
            let y = match args[1].parse::<u32>() {
                Ok(y) if y >= 1 => y,
                _ => {
                    let msg = format!("cal: invalid year '{}'\n", args[1]);
                    ctx.output.stderr(msg.as_bytes());
                    return 1;
                }
            };
            (m, y)
        }
    };

    let month_names = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];

    let name = month_names[(month - 1) as usize];
    let header = format!("{name} {year}");
    // Center the header in 20 chars
    let pad = if header.len() < 20 {
        (20 - header.len()) / 2
    } else {
        0
    };
    let header_line = format!("{:>width$}{}\n", "", header, width = pad);
    ctx.output.stdout(header_line.as_bytes());
    ctx.output.stdout(b"Su Mo Tu We Th Fr Sa\n");

    let days_in_month = days_in_month(month, year);
    let start_day = day_of_week(year, month, 1); // 0=Sunday

    // Print leading spaces
    let mut col = start_day as usize;
    for _ in 0..col {
        ctx.output.stdout(b"   ");
    }

    for day in 1..=days_in_month {
        if col > 0 {
            ctx.output.stdout(b" ");
        }
        let s = format!("{day:>2}");
        ctx.output.stdout(s.as_bytes());
        col += 1;
        if col == 7 {
            ctx.output.stdout(b"\n");
            col = 0;
        }
    }
    if col != 0 {
        ctx.output.stdout(b"\n");
    }
    0
}

/// Returns 1 if `year` is a leap year.
fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

/// Number of days in a given month.
fn days_in_month(month: u32, year: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Zeller's congruence to find day of week (0=Sunday, 1=Monday, ... 6=Saturday).
#[allow(clippy::many_single_char_names, clippy::cast_possible_wrap)]
fn day_of_week(year: u32, month: u32, day: u32) -> u32 {
    // Adjust month: January=13, February=14 (of the previous year)
    let (y, m) = if month <= 2 {
        (year as i32 - 1, month as i32 + 12)
    } else {
        (year as i32, month as i32)
    };

    let q = day as i32;
    let k = y % 100;
    let j = y / 100;

    let h = (q + (13 * (m + 1)) / 5 + k + k / 4 + j / 4 - 2 * j) % 7;
    // h: 0=Saturday, 1=Sunday, 2=Monday, ...
    // Convert to 0=Sunday, 1=Monday, ...
    ((h + 6) % 7) as u32
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

    // -----------------------------------------------------------------------
    // which
    // -----------------------------------------------------------------------

    #[test]
    fn which_known_command() {
        let mut fs = make_fs();
        let (status, stdout, _) = run(util_which, &["which", "cat"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(stdout, "/usr/bin/cat\n");
    }

    #[test]
    fn which_unknown_command() {
        let mut fs = make_fs();
        let (status, stdout, stderr) = run(util_which, &["which", "nonexistent_cmd"], &mut fs);
        assert_eq!(status, 1);
        assert!(stdout.is_empty());
        assert!(stderr.contains("no nonexistent_cmd"));
    }

    #[test]
    fn which_flag_a() {
        let mut fs = make_fs();
        let (status, stdout, _) = run(util_which, &["which", "-a", "echo"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(stdout, "/usr/bin/echo\n");
    }

    #[test]
    fn which_multiple_commands() {
        let mut fs = make_fs();
        let (status, stdout, _) = run(util_which, &["which", "cat", "ls"], &mut fs);
        assert_eq!(status, 0);
        assert!(stdout.contains("/usr/bin/cat\n"));
        assert!(stdout.contains("/usr/bin/ls\n"));
    }

    #[test]
    fn which_missing_operand() {
        let mut fs = make_fs();
        let (status, _, stderr) = run(util_which, &["which"], &mut fs);
        assert_eq!(status, 1);
        assert!(stderr.contains("missing operand"));
    }

    // -----------------------------------------------------------------------
    // rmdir
    // -----------------------------------------------------------------------

    #[test]
    fn rmdir_empty_dir() {
        let mut fs = make_fs();
        fs.create_dir("/emptydir").unwrap();
        let (status, _, _) = run(util_rmdir, &["rmdir", "/emptydir"], &mut fs);
        assert_eq!(status, 0);
        assert!(fs.stat("/emptydir").is_err());
    }

    #[test]
    fn rmdir_nonempty_dir_fails() {
        let mut fs = make_fs_with_file("/mydir/file.txt", b"data");
        let (status, _, stderr) = run(util_rmdir, &["rmdir", "/mydir"], &mut fs);
        assert_eq!(status, 1);
        assert!(stderr.contains("not empty") || stderr.contains("Not empty") || stderr.contains("Directory not empty"));
    }

    #[test]
    fn rmdir_parents() {
        let mut fs = make_fs();
        fs.create_dir("/a").unwrap();
        fs.create_dir("/a/b").unwrap();
        fs.create_dir("/a/b/c").unwrap();
        let (status, _, _) = run(util_rmdir, &["rmdir", "-p", "/a/b/c"], &mut fs);
        assert_eq!(status, 0);
        // c should be removed; b should be removed (was empty after c removed);
        // a should be removed (was empty after b removed)
        assert!(fs.stat("/a/b/c").is_err());
        assert!(fs.stat("/a/b").is_err());
        assert!(fs.stat("/a").is_err());
    }

    #[test]
    fn rmdir_nonexistent_dir() {
        let mut fs = make_fs();
        let (status, _, stderr) = run(util_rmdir, &["rmdir", "/nope"], &mut fs);
        assert_eq!(status, 1);
        assert!(!stderr.is_empty());
    }

    // -----------------------------------------------------------------------
    // tac
    // -----------------------------------------------------------------------

    #[test]
    fn tac_reverse_lines() {
        let mut fs = make_fs_with_file("/lines.txt", b"a\nb\nc");
        let (status, stdout, _) = run(util_tac, &["tac", "/lines.txt"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(stdout, "c\nb\na\n");
    }

    #[test]
    fn tac_single_line() {
        let mut fs = make_fs_with_file("/one.txt", b"only");
        let (status, stdout, _) = run(util_tac, &["tac", "/one.txt"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(stdout, "only\n");
    }

    #[test]
    fn tac_stdin() {
        let mut fs = make_fs();
        let (status, stdout, _) = run_stdin(util_tac, &["tac"], b"x\ny\nz", &mut fs);
        assert_eq!(status, 0);
        assert_eq!(stdout, "z\ny\nx\n");
    }

    // -----------------------------------------------------------------------
    // nl
    // -----------------------------------------------------------------------

    #[test]
    fn nl_skip_empty_lines_default() {
        let mut fs = make_fs_with_file("/f.txt", b"hello\n\nworld");
        let (status, stdout, _) = run(util_nl, &["nl", "/f.txt"], &mut fs);
        assert_eq!(status, 0);
        // Default: -b t (non-empty only)
        assert!(stdout.contains("1\thello"));
        assert!(stdout.contains("\n\n")); // blank line not numbered
        assert!(stdout.contains("2\tworld"));
    }

    #[test]
    fn nl_number_all_lines() {
        let mut fs = make_fs_with_file("/f.txt", b"a\n\nb");
        let (status, stdout, _) = run(util_nl, &["nl", "-b", "a", "/f.txt"], &mut fs);
        assert_eq!(status, 0);
        // With -b a, even empty lines get numbered
        assert!(stdout.contains("1\ta"));
        assert!(stdout.contains("2\t"));
        assert!(stdout.contains("3\tb"));
    }

    #[test]
    fn nl_stdin() {
        let mut fs = make_fs();
        let (status, stdout, _) = run_stdin(util_nl, &["nl"], b"foo\nbar", &mut fs);
        assert_eq!(status, 0);
        assert!(stdout.contains("1\tfoo"));
        assert!(stdout.contains("2\tbar"));
    }

    // -----------------------------------------------------------------------
    // shuf
    // -----------------------------------------------------------------------

    #[test]
    fn shuf_same_line_count() {
        let mut fs = make_fs_with_file("/data.txt", b"a\nb\nc\nd\ne");
        let (status, stdout, _) = run(util_shuf, &["shuf", "/data.txt"], &mut fs);
        assert_eq!(status, 0);
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines.len(), 5);
        // All original values present
        for v in &["a", "b", "c", "d", "e"] {
            assert!(lines.contains(v));
        }
    }

    #[test]
    fn shuf_n_limits_count() {
        let mut fs = make_fs_with_file("/data.txt", b"a\nb\nc\nd\ne");
        let (status, stdout, _) = run(util_shuf, &["shuf", "-n", "2", "/data.txt"], &mut fs);
        assert_eq!(status, 0);
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn shuf_empty_input() {
        let mut fs = make_fs_with_file("/empty.txt", b"");
        let (status, stdout, _) = run(util_shuf, &["shuf", "/empty.txt"], &mut fs);
        assert_eq!(status, 0);
        assert!(stdout.is_empty());
    }

    // -----------------------------------------------------------------------
    // cmp
    // -----------------------------------------------------------------------

    #[test]
    fn cmp_identical_files() {
        let mut fs = make_fs_with_file("/a.txt", b"hello world");
        let h = fs.open("/b.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"hello world").unwrap();
        fs.close(h);
        let (status, stdout, _) = run(util_cmp, &["cmp", "/a.txt", "/b.txt"], &mut fs);
        assert_eq!(status, 0);
        assert!(stdout.is_empty());
    }

    #[test]
    fn cmp_different_files() {
        let mut fs = make_fs_with_file("/a.txt", b"hello");
        let h = fs.open("/b.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"hallo").unwrap();
        fs.close(h);
        let (status, stdout, _) = run(util_cmp, &["cmp", "/a.txt", "/b.txt"], &mut fs);
        assert_eq!(status, 1);
        assert!(stdout.contains("differ"));
        assert!(stdout.contains("byte 2"));
    }

    #[test]
    fn cmp_silent_flag() {
        let mut fs = make_fs_with_file("/a.txt", b"abc");
        let h = fs.open("/b.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"axc").unwrap();
        fs.close(h);
        let (status, stdout, _) = run(util_cmp, &["cmp", "-s", "/a.txt", "/b.txt"], &mut fs);
        assert_eq!(status, 1);
        assert!(stdout.is_empty());
    }

    #[test]
    fn cmp_verbose_flag() {
        let mut fs = make_fs_with_file("/a.txt", b"ab");
        let h = fs.open("/b.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"ax").unwrap();
        fs.close(h);
        let (status, stdout, _) = run(util_cmp, &["cmp", "-l", "/a.txt", "/b.txt"], &mut fs);
        assert_eq!(status, 1);
        // -l shows byte position, byte values for each difference
        assert!(stdout.contains('2'));
    }

    // -----------------------------------------------------------------------
    // comm
    // -----------------------------------------------------------------------

    #[test]
    fn comm_two_sorted_files() {
        let mut fs = make_fs_with_file("/a.txt", b"a\nb\nc\nd");
        let h = fs.open("/b.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"b\nc\ne").unwrap();
        fs.close(h);
        let (status, stdout, _) = run(util_comm, &["comm", "/a.txt", "/b.txt"], &mut fs);
        assert_eq!(status, 0);
        // Column 1 (only in a): a, d — no prefix
        // Column 2 (only in b): e — one tab prefix
        // Column 3 (both): b, c — two tab prefix
        assert!(stdout.contains("a\n"));
        assert!(stdout.contains("\t\tb\n"));
        assert!(stdout.contains("\t\tc\n"));
        assert!(stdout.contains("d\n"));
        assert!(stdout.contains("\te\n"));
    }

    #[test]
    fn comm_suppress_column_1() {
        let mut fs = make_fs_with_file("/a.txt", b"a\nb\nc");
        let h = fs.open("/b.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"b\nd").unwrap();
        fs.close(h);
        let (status, stdout, _) = run(util_comm, &["comm", "-1", "/a.txt", "/b.txt"], &mut fs);
        assert_eq!(status, 0);
        // Column 1 suppressed, so "a" and "c" should not appear
        assert!(!stdout.contains("a\n"));
        assert!(!stdout.contains("c\n"));
        // Column 2 (only in b): d
        assert!(stdout.contains("d\n"));
        // Column 3 (both): b
        assert!(stdout.contains("\tb\n"));
    }

    #[test]
    fn comm_suppress_column_3() {
        let mut fs = make_fs_with_file("/a.txt", b"a\nb");
        let h = fs.open("/b.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"b\nc").unwrap();
        fs.close(h);
        let (status, stdout, _) = run(util_comm, &["comm", "-3", "/a.txt", "/b.txt"], &mut fs);
        assert_eq!(status, 0);
        // Common "b" should not appear
        assert!(!stdout.contains('b'));
        assert!(stdout.contains("a\n"));
        assert!(stdout.contains("\tc\n"));
    }

    // -----------------------------------------------------------------------
    // fold
    // -----------------------------------------------------------------------

    #[test]
    fn fold_default_80() {
        // Line shorter than 80 should pass through unchanged
        let mut fs = make_fs_with_file("/f.txt", b"short line");
        let (status, stdout, _) = run(util_fold, &["fold", "/f.txt"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(stdout, "short line\n");
    }

    #[test]
    fn fold_w10() {
        let mut fs = make_fs_with_file("/f.txt", b"abcdefghijklmno");
        let (status, stdout, _) = run(util_fold, &["fold", "-w", "10", "/f.txt"], &mut fs);
        assert_eq!(status, 0);
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "abcdefghij");
        assert_eq!(lines[1], "klmno");
    }

    #[test]
    fn fold_break_at_spaces() {
        let mut fs = make_fs_with_file("/f.txt", b"hello world foo bar baz");
        let (status, stdout, _) = run(util_fold, &["fold", "-s", "-w", "12", "/f.txt"], &mut fs);
        assert_eq!(status, 0);
        // Each line should be at most 12 chars (breaking at spaces)
        for line in stdout.lines() {
            assert!(line.len() <= 12, "line too long: {line:?}");
        }
    }

    // -----------------------------------------------------------------------
    // nproc
    // -----------------------------------------------------------------------

    #[test]
    fn nproc_returns_one() {
        let mut fs = make_fs();
        let (status, stdout, _) = run(util_nproc, &["nproc"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(stdout, "1\n");
    }

    // -----------------------------------------------------------------------
    // expand
    // -----------------------------------------------------------------------

    #[test]
    fn expand_tabs_default_8() {
        let mut fs = make_fs_with_file("/f.txt", b"a\tb");
        let (status, stdout, _) = run(util_expand, &["expand", "/f.txt"], &mut fs);
        assert_eq!(status, 0);
        // 'a' is at col 0, tab expands to fill to next tab stop (col 8)
        assert_eq!(stdout, "a       b\n");
    }

    #[test]
    fn expand_tabs_t4() {
        let mut fs = make_fs_with_file("/f.txt", b"\thello");
        let (status, stdout, _) = run(util_expand, &["expand", "-t", "4", "/f.txt"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(stdout, "    hello\n");
    }

    #[test]
    fn expand_multiple_tabs() {
        let mut fs = make_fs_with_file("/f.txt", b"a\tb\tc");
        let (status, stdout, _) = run(util_expand, &["expand", "-t", "4", "/f.txt"], &mut fs);
        assert_eq!(status, 0);
        // 'a' at col 0, tab fills to col 4: "a   ", 'b' at col 4, tab fills to col 8: "b   ", 'c'
        assert_eq!(stdout, "a   b   c\n");
    }

    // -----------------------------------------------------------------------
    // unexpand
    // -----------------------------------------------------------------------

    #[test]
    fn unexpand_leading_spaces() {
        // 8 leading spaces should become one tab (default tab stop 8)
        let mut fs = make_fs_with_file("/f.txt", b"        hello");
        let (status, stdout, _) = run(util_unexpand, &["unexpand", "/f.txt"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(stdout, "\thello\n");
    }

    #[test]
    fn unexpand_all_flag() {
        // With -a, spaces throughout the line are converted
        let mut fs = make_fs_with_file("/f.txt", b"a       b");
        let (status, stdout, _) = run(util_unexpand, &["unexpand", "-a", "/f.txt"], &mut fs);
        assert_eq!(status, 0);
        // 'a' then 7 spaces: col 0='a', cols 1-7 are spaces, col 8 is tab stop
        // That's 7 spaces (positions 1-7), hitting tab stop at 8
        assert!(stdout.contains('\t'));
    }

    #[test]
    fn unexpand_no_change_when_insufficient_spaces() {
        // 3 leading spaces (less than 8) should stay as spaces
        let mut fs = make_fs_with_file("/f.txt", b"   hi");
        let (status, stdout, _) = run(util_unexpand, &["unexpand", "/f.txt"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(stdout, "   hi\n");
    }

    // -----------------------------------------------------------------------
    // truncate
    // -----------------------------------------------------------------------

    #[test]
    fn truncate_grow_file() {
        let mut fs = make_fs_with_file("/f.txt", b"abc");
        let (status, _, _) = run(util_truncate, &["truncate", "-s", "10", "/f.txt"], &mut fs);
        assert_eq!(status, 0);
        let h = fs.open("/f.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(data.len(), 10);
        assert_eq!(&data[..3], b"abc");
        assert!(data[3..].iter().all(|&b| b == 0));
    }

    #[test]
    fn truncate_shrink_file() {
        let mut fs = make_fs_with_file("/f.txt", b"hello world");
        let (status, _, _) = run(util_truncate, &["truncate", "-s", "5", "/f.txt"], &mut fs);
        assert_eq!(status, 0);
        let h = fs.open("/f.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&data, b"hello");
    }

    #[test]
    fn truncate_relative_add() {
        let mut fs = make_fs_with_file("/f.txt", b"abc");
        let (status, _, _) = run(
            util_truncate,
            &["truncate", "-s", "+5", "/f.txt"],
            &mut fs,
        );
        assert_eq!(status, 0);
        let h = fs.open("/f.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(data.len(), 8); // 3 + 5
    }

    #[test]
    fn truncate_relative_subtract() {
        let mut fs = make_fs_with_file("/f.txt", b"hello world");
        let (status, _, _) = run(
            util_truncate,
            &["truncate", "-s", "-6", "/f.txt"],
            &mut fs,
        );
        assert_eq!(status, 0);
        let h = fs.open("/f.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&data, b"hello");
    }

    // -----------------------------------------------------------------------
    // factor
    // -----------------------------------------------------------------------

    #[test]
    fn factor_small_primes() {
        let mut fs = make_fs();
        let (status, stdout, _) = run(util_factor, &["factor", "7"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(stdout, "7: 7\n");
    }

    #[test]
    fn factor_composite_number() {
        let mut fs = make_fs();
        let (status, stdout, _) = run(util_factor, &["factor", "12"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(stdout, "12: 2 2 3\n");
    }

    #[test]
    fn factor_one() {
        let mut fs = make_fs();
        let (status, stdout, _) = run(util_factor, &["factor", "1"], &mut fs);
        assert_eq!(status, 0);
        assert_eq!(stdout, "1: \n");
    }

    #[test]
    fn factor_large_number() {
        let mut fs = make_fs();
        let (status, stdout, _) = run(util_factor, &["factor", "1000003"], &mut fs);
        assert_eq!(status, 0);
        // 1000003 is prime
        assert_eq!(stdout, "1000003: 1000003\n");
    }

    #[test]
    fn factor_from_stdin() {
        let mut fs = make_fs();
        let (status, stdout, _) = run_stdin(util_factor, &["factor"], b"15", &mut fs);
        assert_eq!(status, 0);
        assert_eq!(stdout, "15: 3 5\n");
    }

    // -----------------------------------------------------------------------
    // cksum
    // -----------------------------------------------------------------------

    #[test]
    fn cksum_known_input() {
        let mut fs = make_fs();
        let (status, stdout, _) = run_stdin(util_cksum, &["cksum"], b"hello\n", &mut fs);
        assert_eq!(status, 0);
        // Verify format: checksum + space + size
        let parts: Vec<&str> = stdout.split_whitespace().collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1], "6"); // 6 bytes
        // Verify the checksum parses as a number
        parts[0].parse::<u32>().unwrap();
    }

    #[test]
    fn cksum_file() {
        let mut fs = make_fs_with_file("/f.txt", b"test data");
        let (status, stdout, _) = run(util_cksum, &["cksum", "/f.txt"], &mut fs);
        assert_eq!(status, 0);
        let parts: Vec<&str> = stdout.split_whitespace().collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[1], "9"); // 9 bytes
        assert_eq!(parts[2], "/f.txt");
    }

    #[test]
    fn cksum_multiple_files() {
        let mut fs = make_fs_with_file("/a.txt", b"aaa");
        let h = fs.open("/b.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"bbb").unwrap();
        fs.close(h);
        let (status, stdout, _) = run(util_cksum, &["cksum", "/a.txt", "/b.txt"], &mut fs);
        assert_eq!(status, 0);
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("/a.txt"));
        assert!(lines[1].contains("/b.txt"));
    }

    #[test]
    fn cksum_empty_input() {
        let mut fs = make_fs();
        let (status, stdout, _) = run_stdin(util_cksum, &["cksum"], b"", &mut fs);
        assert_eq!(status, 0);
        let parts: Vec<&str> = stdout.split_whitespace().collect();
        assert_eq!(parts[1], "0");
    }

    // -----------------------------------------------------------------------
    // tsort
    // -----------------------------------------------------------------------

    #[test]
    fn tsort_linear_order() {
        let mut fs = make_fs();
        let (status, stdout, _) = run_stdin(util_tsort, &["tsort"], b"a b b c", &mut fs);
        assert_eq!(status, 0);
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines.len(), 3);
        // a must come before b, b must come before c
        let pos_a = lines.iter().position(|&l| l == "a").unwrap();
        let pos_b = lines.iter().position(|&l| l == "b").unwrap();
        let pos_c = lines.iter().position(|&l| l == "c").unwrap();
        assert!(pos_a < pos_b);
        assert!(pos_b < pos_c);
    }

    #[test]
    fn tsort_cycle_detection() {
        let mut fs = make_fs();
        let (status, _, stderr) = run_stdin(util_tsort, &["tsort"], b"a b b a", &mut fs);
        assert_eq!(status, 1);
        assert!(stderr.contains("cycle"));
    }

    #[test]
    fn tsort_self_loop_ignored() {
        // Self-edges (a a) are ignored per the implementation
        let mut fs = make_fs();
        let (status, stdout, _) = run_stdin(util_tsort, &["tsort"], b"a a", &mut fs);
        assert_eq!(status, 0);
        assert_eq!(stdout.trim(), "a");
    }

    // -----------------------------------------------------------------------
    // install
    // -----------------------------------------------------------------------

    #[test]
    fn install_d_creates_directory() {
        let mut fs = make_fs();
        let (status, _, _) = run(util_install, &["install", "-d", "/a/b/c"], &mut fs);
        assert_eq!(status, 0);
        assert!(fs.stat("/a/b/c").unwrap().is_dir);
    }

    #[test]
    fn install_copy_file() {
        let mut fs = make_fs_with_file("/src.txt", b"content");
        let (status, _, _) = run(
            util_install,
            &["install", "/src.txt", "/dst.txt"],
            &mut fs,
        );
        assert_eq!(status, 0);
        let h = fs.open("/dst.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&data, b"content");
    }

    #[test]
    fn install_creates_parent_dirs() {
        let mut fs = make_fs_with_file("/src.txt", b"data");
        let (status, _, _) = run(
            util_install,
            &["install", "/src.txt", "/new/path/dst.txt"],
            &mut fs,
        );
        assert_eq!(status, 0);
        assert!(fs.stat("/new/path").unwrap().is_dir);
        let h = fs.open("/new/path/dst.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&data, b"data");
    }

    // -----------------------------------------------------------------------
    // timeout
    // -----------------------------------------------------------------------

    #[test]
    fn timeout_passes_through_command() {
        let mut fs = make_fs();
        let (status, stdout, _) = run(
            util_timeout,
            &["timeout", "5", "echo", "hello"],
            &mut fs,
        );
        assert_eq!(status, 0);
        assert_eq!(stdout, "echo hello\n");
    }

    #[test]
    fn timeout_missing_command() {
        let mut fs = make_fs();
        let (status, _, stderr) = run(util_timeout, &["timeout", "5"], &mut fs);
        assert_eq!(status, 1);
        assert!(stderr.contains("missing command"));
    }

    #[test]
    fn timeout_missing_operand() {
        let mut fs = make_fs();
        let (status, _, stderr) = run(util_timeout, &["timeout"], &mut fs);
        assert_eq!(status, 1);
        assert!(stderr.contains("missing operand"));
    }

    // -----------------------------------------------------------------------
    // cal
    // -----------------------------------------------------------------------

    #[test]
    fn cal_default_output() {
        let mut fs = make_fs();
        let (status, stdout, _) = run(util_cal, &["cal"], &mut fs);
        assert_eq!(status, 0);
        // Default is January 2026
        assert!(stdout.contains("January 2026"));
        assert!(stdout.contains("Su Mo Tu We Th Fr Sa"));
    }

    #[test]
    fn cal_specific_month_year() {
        let mut fs = make_fs();
        let (status, stdout, _) = run(util_cal, &["cal", "3", "2026"], &mut fs);
        assert_eq!(status, 0);
        assert!(stdout.contains("March 2026"));
        assert!(stdout.contains("Su Mo Tu We Th Fr Sa"));
        // March 2026 starts on Sunday, so 1 should be first
        assert!(stdout.contains(" 1 "));
    }

    #[test]
    fn cal_invalid_month() {
        let mut fs = make_fs();
        let (status, _, stderr) = run(util_cal, &["cal", "13", "2026"], &mut fs);
        assert_eq!(status, 1);
        assert!(stderr.contains("invalid month"));
    }

    #[test]
    fn cal_february_leap_year() {
        let mut fs = make_fs();
        let (status, stdout, _) = run(util_cal, &["cal", "2", "2024"], &mut fs);
        assert_eq!(status, 0);
        assert!(stdout.contains("February 2024"));
        // 2024 is a leap year, so Feb has 29 days
        assert!(stdout.contains("29"));
    }
}
