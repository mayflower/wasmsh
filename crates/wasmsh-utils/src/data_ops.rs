//! Data/string utilities: seq, basename, dirname, expr, xargs.

use crate::helpers::*;
use crate::UtilContext;

pub(crate) fn util_seq(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let args = &argv[1..];
    let (start, end, step) = match args.len() {
        1 => (1i64, args[0].parse().unwrap_or(1), 1i64),
        2 => (
            args[0].parse().unwrap_or(1),
            args[1].parse().unwrap_or(1),
            1,
        ),
        3 => (
            args[0].parse().unwrap_or(1),
            args[2].parse().unwrap_or(1),
            args[1].parse().unwrap_or(1),
        ),
        _ => {
            ctx.output.stderr(b"seq: missing operand\n");
            return 1;
        }
    };
    let mut i = start;
    while (step > 0 && i <= end) || (step < 0 && i >= end) {
        let s = format!("{i}\n");
        ctx.output.stdout(s.as_bytes());
        i += step;
    }
    0
}

pub(crate) fn util_basename(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    if !require_args(argv, 2, ctx.output) {
        return 1;
    }
    let path = argv[1];
    let suffix = argv.get(2).copied().unwrap_or("");
    let name = path.rsplit('/').next().unwrap_or(path);
    let result = if !suffix.is_empty() && name.ends_with(suffix) && name.len() > suffix.len() {
        &name[..name.len() - suffix.len()]
    } else {
        name
    };
    ctx.output.stdout(result.as_bytes());
    ctx.output.stdout(b"\n");
    0
}

pub(crate) fn util_dirname(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    if !require_args(argv, 2, ctx.output) {
        return 1;
    }
    let path = argv[1];
    let dir = if let Some(pos) = path.rfind('/') {
        if pos == 0 {
            "/"
        } else {
            &path[..pos]
        }
    } else {
        "."
    };
    ctx.output.stdout(dir.as_bytes());
    ctx.output.stdout(b"\n");
    0
}

pub(crate) fn util_expr(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let args = &argv[1..];
    if args.len() == 3 {
        let left: i64 = args[0].parse().unwrap_or(0);
        let right: i64 = args[2].parse().unwrap_or(0);
        let result = match args[1] {
            "+" => left + right,
            "-" => left - right,
            "*" => left * right,
            "/" => {
                if right != 0 {
                    left / right
                } else {
                    0
                }
            }
            "%" => {
                if right != 0 {
                    left % right
                } else {
                    0
                }
            }
            "=" => {
                if args[0] == args[2] {
                    1
                } else {
                    0
                }
            }
            "!=" => {
                if args[0] != args[2] {
                    1
                } else {
                    0
                }
            }
            _ => 0,
        };
        let s = format!("{result}\n");
        ctx.output.stdout(s.as_bytes());
        if result == 0 {
            1
        } else {
            0
        }
    } else if args.len() == 1 {
        ctx.output.stdout(args[0].as_bytes());
        ctx.output.stdout(b"\n");
        if args[0] == "0" || args[0].is_empty() {
            1
        } else {
            0
        }
    } else {
        ctx.output.stderr(b"expr: syntax error\n");
        2
    }
}

pub(crate) fn util_xargs(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let cmd = if argv.len() > 1 { argv[1] } else { "echo" };
    let data = if let Some(d) = ctx.stdin {
        String::from_utf8_lossy(d).to_string()
    } else {
        return 0;
    };
    let words: Vec<&str> = data.split_whitespace().collect();
    if words.is_empty() {
        return 0;
    }
    if cmd == "echo" {
        ctx.output.stdout(words.join(" ").as_bytes());
        ctx.output.stdout(b"\n");
    } else {
        // For non-echo commands, output the full command line
        // (actual command execution would need runtime access)
        let mut full = String::from(cmd);
        for w in &words {
            full.push(' ');
            full.push_str(w);
        }
        full.push('\n');
        ctx.output.stdout(full.as_bytes());
    }
    0
}
