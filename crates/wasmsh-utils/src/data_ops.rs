//! Data/string utilities: seq, basename, dirname, expr, xargs.

use crate::helpers::require_args;
use crate::UtilContext;

fn seq_parse(s: &str, output: &mut dyn crate::UtilOutput) -> Option<i64> {
    if let Ok(v) = s.parse::<i64>() {
        Some(v)
    } else {
        let msg = format!("seq: invalid argument: '{s}'\n");
        output.stderr(msg.as_bytes());
        None
    }
}

pub(crate) fn util_seq(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let args = &argv[1..];
    let (start, end, step) = match args.len() {
        1 => {
            let Some(e) = seq_parse(args[0], ctx.output) else {
                return 1;
            };
            (1i64, e, 1i64)
        }
        2 => {
            let Some(s) = seq_parse(args[0], ctx.output) else {
                return 1;
            };
            let Some(e) = seq_parse(args[1], ctx.output) else {
                return 1;
            };
            (s, e, 1)
        }
        3 => {
            let Some(s) = seq_parse(args[0], ctx.output) else {
                return 1;
            };
            let Some(st) = seq_parse(args[1], ctx.output) else {
                return 1;
            };
            let Some(e) = seq_parse(args[2], ctx.output) else {
                return 1;
            };
            (s, e, st)
        }
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
        // String comparison operators don't require numeric operands
        if args[1] == "=" || args[1] == "!=" {
            let result = match args[1] {
                "=" => i64::from(args[0] == args[2]),
                "!=" => i64::from(args[0] != args[2]),
                _ => 0,
            };
            let s = format!("{result}\n");
            ctx.output.stdout(s.as_bytes());
            return i32::from(result == 0);
        }
        let Ok(left) = args[0].parse::<i64>() else {
            let msg = format!("expr: non-numeric argument: '{}'\n", args[0]);
            ctx.output.stderr(msg.as_bytes());
            return 2;
        };
        let Ok(right) = args[2].parse::<i64>() else {
            let msg = format!("expr: non-numeric argument: '{}'\n", args[2]);
            ctx.output.stderr(msg.as_bytes());
            return 2;
        };
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
            _ => 0,
        };
        let s = format!("{result}\n");
        ctx.output.stdout(s.as_bytes());
        i32::from(result == 0)
    } else if args.len() == 1 {
        ctx.output.stdout(args[0].as_bytes());
        ctx.output.stdout(b"\n");
        i32::from(args[0] == "0" || args[0].is_empty())
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
