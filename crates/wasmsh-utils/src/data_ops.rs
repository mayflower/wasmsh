//! Data/string utilities: seq, basename, dirname, expr, xargs, yes, md5sum, sha256sum, base64.

use crate::helpers::{
    hashsum_util, hex_encode, require_args, stream_input_chunks, stream_input_whitespace_tokens,
};
use crate::UtilContext;

const SEQ_MAX_ITERATIONS: usize = 10_000_000;

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
    let mut separator = "\n".to_string();
    let mut equal_width = false;
    let mut format_str: Option<String> = None;
    let mut num_args: Vec<&str> = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i];
        if arg == "-s" && i + 1 < argv.len() {
            separator = argv[i + 1].to_string();
            i += 2;
        } else if arg == "-w" {
            equal_width = true;
            i += 1;
        } else if arg == "-f" && i + 1 < argv.len() {
            format_str = Some(argv[i + 1].to_string());
            i += 2;
        } else {
            num_args.push(arg);
            i += 1;
        }
    }
    let (start, end, step) = match num_args.len() {
        1 => {
            let Some(e) = seq_parse(num_args[0], ctx.output) else {
                return 1;
            };
            (1i64, e, 1i64)
        }
        2 => {
            let Some(s) = seq_parse(num_args[0], ctx.output) else {
                return 1;
            };
            let Some(e) = seq_parse(num_args[1], ctx.output) else {
                return 1;
            };
            (s, e, 1)
        }
        3 => {
            let Some(s) = seq_parse(num_args[0], ctx.output) else {
                return 1;
            };
            let Some(st) = seq_parse(num_args[1], ctx.output) else {
                return 1;
            };
            let Some(e) = seq_parse(num_args[2], ctx.output) else {
                return 1;
            };
            (s, e, st)
        }
        _ => {
            ctx.output.stderr(b"seq: missing operand\n");
            return 1;
        }
    };
    if step == 0 {
        ctx.output.stderr(b"seq: zero increment\n");
        return 1;
    }
    let width = if equal_width {
        let max_val = start.abs().max(end.abs());
        format!("{max_val}").len()
    } else {
        0
    };
    let mut vals: Vec<String> = Vec::new();
    let mut j = start;
    let mut count = 0usize;
    while (step > 0 && j <= end) || (step < 0 && j >= end) {
        let s = if let Some(ref fmt) = format_str {
            // Simple %g/%f/%e support
            fmt.replace("%g", &format!("{j}"))
                .replace("%f", &format!("{j}.000000"))
                .replace("%e", &format!("{j}"))
        } else if equal_width {
            format!("{j:0>width$}")
        } else {
            format!("{j}")
        };
        vals.push(s);
        j += step;
        count += 1;
        if count >= SEQ_MAX_ITERATIONS {
            break;
        }
    }
    let output = vals.join(&separator);
    ctx.output.stdout(output.as_bytes());
    ctx.output.stdout(b"\n");
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
    if args.len() == 1 {
        return expr_emit_scalar(ctx, args[0]);
    }
    // 2-arg forms: length STRING
    if args.len() == 2 && args[0] == "length" {
        let len = i64::try_from(args[1].len()).unwrap_or(i64::MAX);
        return expr_emit_result(ctx, len);
    }
    // 3-arg forms: binary ops, match, index
    if args.len() == 3 {
        if args[0] == "match" {
            return expr_match(ctx, args[1], args[2]);
        }
        if args[0] == "index" {
            return expr_index(ctx, args[1], args[2]);
        }
        return expr_eval_binary(ctx, args);
    }
    // 4-arg form: substr STRING POS LENGTH
    if args.len() == 4 && args[0] == "substr" {
        return expr_substr(ctx, args[1], args[2], args[3]);
    }
    // Try as binary expression with more args (shouldn't happen normally)
    if args.len() >= 3 {
        return expr_eval_binary(ctx, &args[..3]);
    }
    ctx.output.stderr(b"expr: syntax error\n");
    2
}

fn expr_emit_result(ctx: &mut UtilContext<'_>, result: i64) -> i32 {
    let s = format!("{result}\n");
    ctx.output.stdout(s.as_bytes());
    i32::from(result == 0)
}

fn expr_emit_scalar(ctx: &mut UtilContext<'_>, value: &str) -> i32 {
    ctx.output.stdout(value.as_bytes());
    ctx.output.stdout(b"\n");
    i32::from(value == "0" || value.is_empty())
}

fn expr_parse_operand(ctx: &mut UtilContext<'_>, value: &str) -> Result<i64, i32> {
    value.parse::<i64>().map_err(|_| {
        let msg = format!("expr: non-numeric argument: '{value}'\n");
        ctx.output.stderr(msg.as_bytes());
        2
    })
}

fn expr_division_by_zero(ctx: &mut UtilContext<'_>) -> i32 {
    ctx.output.stderr(b"expr: division by zero\n");
    2
}

fn expr_eval_string(op: &str, left: &str, right: &str) -> i64 {
    match op {
        "=" => i64::from(left == right),
        "!=" => i64::from(left != right),
        _ => 0,
    }
}

fn expr_eval_numeric(
    ctx: &mut UtilContext<'_>,
    op: &str,
    left: i64,
    right: i64,
) -> Result<i64, i32> {
    match op {
        "+" => Ok(left.wrapping_add(right)),
        "-" => Ok(left.wrapping_sub(right)),
        "*" => Ok(left.wrapping_mul(right)),
        "/" => {
            if right == 0 {
                Err(expr_division_by_zero(ctx))
            } else {
                Ok(left.wrapping_div(right))
            }
        }
        "%" => {
            if right == 0 {
                Err(expr_division_by_zero(ctx))
            } else {
                Ok(left.wrapping_rem(right))
            }
        }
        _ => Ok(0),
    }
}

fn expr_eval_binary(ctx: &mut UtilContext<'_>, args: &[&str]) -> i32 {
    if matches!(args[1], "=" | "!=") {
        return expr_emit_result(ctx, expr_eval_string(args[1], args[0], args[2]));
    }
    // Relational operators: compare as numbers if possible, else strings
    if matches!(args[1], "<" | ">" | "<=" | ">=") {
        let la = args[0].parse::<i64>();
        let ra = args[2].parse::<i64>();
        let result = if let (Ok(l), Ok(r)) = (la, ra) {
            match args[1] {
                "<" => l < r,
                ">" => l > r,
                "<=" => l <= r,
                ">=" => l >= r,
                _ => false,
            }
        } else {
            match args[1] {
                "<" => args[0] < args[2],
                ">" => args[0] > args[2],
                "<=" => args[0] <= args[2],
                ">=" => args[0] >= args[2],
                _ => false,
            }
        };
        return expr_emit_result(ctx, i64::from(result));
    }
    // String regex match: STRING : REGEX
    if args[1] == ":" {
        return expr_match(ctx, args[0], args[2]);
    }
    let left = match expr_parse_operand(ctx, args[0]) {
        Ok(value) => value,
        Err(status) => return status,
    };
    let right = match expr_parse_operand(ctx, args[2]) {
        Ok(value) => value,
        Err(status) => return status,
    };
    match expr_eval_numeric(ctx, args[1], left, right) {
        Ok(result) => expr_emit_result(ctx, result),
        Err(status) => status,
    }
}

fn expr_match(ctx: &mut UtilContext<'_>, string: &str, _pattern: &str) -> i32 {
    // Simple match: return length of match at start of string
    // Full regex not implemented; return length of string if pattern starts with '.'
    // For basic support, match literal prefix
    let len = i64::try_from(string.len()).unwrap_or(i64::MAX);
    expr_emit_result(ctx, len)
}

fn expr_index(ctx: &mut UtilContext<'_>, string: &str, chars: &str) -> i32 {
    for (i, c) in string.chars().enumerate() {
        if chars.contains(c) {
            return expr_emit_result(ctx, i64::try_from(i + 1).unwrap_or(i64::MAX));
        }
    }
    expr_emit_result(ctx, 0)
}

fn expr_substr(ctx: &mut UtilContext<'_>, string: &str, pos: &str, len: &str) -> i32 {
    let pos: usize = pos.parse().unwrap_or(0);
    let len: usize = len.parse().unwrap_or(0);
    if pos == 0 {
        ctx.output.stdout(b"\n");
        return 1;
    }
    let chars: Vec<char> = string.chars().collect();
    let start = (pos - 1).min(chars.len());
    let end = (start + len).min(chars.len());
    let sub: String = chars[start..end].iter().collect();
    ctx.output.stdout(sub.as_bytes());
    ctx.output.stdout(b"\n");
    i32::from(sub.is_empty())
}

pub(crate) fn util_xargs(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut replace_str: Option<&str> = None;
    let mut max_args: Option<usize> = None;
    let mut null_delim = false;
    let mut cmd_start = 1;
    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i];
        if arg == "-I" && i + 1 < argv.len() {
            replace_str = Some(argv[i + 1]);
            i += 2;
            cmd_start = i;
        } else if arg == "-n" && i + 1 < argv.len() {
            max_args = argv[i + 1].parse().ok();
            i += 2;
            cmd_start = i;
        } else if arg == "-0" || arg == "--null" {
            null_delim = true;
            i += 1;
            cmd_start = i;
        } else if (arg == "-d" || arg == "-P" || arg == "-L") && i + 1 < argv.len() {
            i += 2;
            cmd_start = i;
        } else if arg == "-t" || arg == "-p" {
            i += 1;
            cmd_start = i;
        } else {
            break;
        }
    }
    let cmd_args = &argv[cmd_start..];
    let cmd = if cmd_args.is_empty() {
        "echo"
    } else {
        cmd_args[0]
    };
    let extra: Vec<&str> = if cmd_args.len() > 1 {
        cmd_args[1..].to_vec()
    } else {
        Vec::new()
    };
    let mut items = Vec::new();
    if null_delim {
        let mut pending = Vec::new();
        if stream_input_chunks(ctx, &[], "xargs", |chunk, _| {
            pending.extend_from_slice(chunk);
            while let Some(pos) = pending.iter().position(|&b| b == b'\0') {
                let item = pending.drain(..pos).collect::<Vec<u8>>();
                pending.drain(..1);
                if !item.is_empty() {
                    items.push(String::from_utf8_lossy(&item).to_string());
                }
            }
            Ok(())
        })
        .is_err()
        {
            return 1;
        }
        if !pending.is_empty() {
            items.push(String::from_utf8_lossy(&pending).to_string());
        }
    } else if stream_input_whitespace_tokens(ctx, &[], "xargs", |token, _| {
        items.push(token.to_string());
        Ok(())
    })
    .is_err()
    {
        return 1;
    }
    if items.is_empty() {
        return 0;
    }
    if let Some(repl) = replace_str {
        for item in &items {
            if cmd == "echo" {
                let out = if extra.is_empty() {
                    item.clone()
                } else {
                    extra
                        .iter()
                        .map(|a| a.replace(repl, item))
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                ctx.output.stdout(out.as_bytes());
                ctx.output.stdout(b"\n");
            } else {
                let mut line = String::from(cmd);
                for ea in &extra {
                    line.push(' ');
                    line.push_str(&ea.replace(repl, item));
                }
                line.push('\n');
                ctx.output.stdout(line.as_bytes());
            }
        }
    } else if let Some(n) = max_args {
        for chunk in items.chunks(n) {
            if cmd == "echo" {
                ctx.output.stdout(
                    chunk
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                        .as_bytes(),
                );
                ctx.output.stdout(b"\n");
            } else {
                let mut line = String::from(cmd);
                for ea in &extra {
                    line.push(' ');
                    line.push_str(ea);
                }
                for item in chunk {
                    line.push(' ');
                    line.push_str(item);
                }
                line.push('\n');
                ctx.output.stdout(line.as_bytes());
            }
        }
    } else if cmd == "echo" {
        ctx.output.stdout(
            items
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(" ")
                .as_bytes(),
        );
        ctx.output.stdout(b"\n");
    } else {
        let mut line = String::from(cmd);
        for ea in &extra {
            line.push(' ');
            line.push_str(ea);
        }
        for item in &items {
            line.push(' ');
            line.push_str(item);
        }
        line.push('\n');
        ctx.output.stdout(line.as_bytes());
    }
    0
}

// ---------------------------------------------------------------------------
// yes
// ---------------------------------------------------------------------------

/// Maximum number of lines `yes` will output to prevent infinite loops in sandbox.
const YES_MAX_LINES: usize = 65_536;

pub(crate) fn util_yes(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let text = if argv.len() > 1 {
        argv[1..].join(" ")
    } else {
        "y".to_string()
    };
    let line = format!("{text}\n");
    let line_bytes = line.as_bytes();
    for _ in 0..YES_MAX_LINES {
        ctx.output.stdout(line_bytes);
    }
    0
}

// ---------------------------------------------------------------------------
// MD5 — backed by the `md-5` crate (RustCrypto).  See ADR-0024.
// ---------------------------------------------------------------------------

fn md5_digest(data: &[u8]) -> [u8; 16] {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(data);
    h.finalize().into()
}

pub(crate) fn util_md5sum(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    hashsum_util(ctx, argv, "md5sum", |data| hex_encode(&md5_digest(data)))
}

// ---------------------------------------------------------------------------
// SHA-256 — backed by the `sha2` crate (RustCrypto).  See ADR-0024.
// ---------------------------------------------------------------------------

fn sha256_digest(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
}

pub(crate) fn util_sha256sum(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    hashsum_util(ctx, argv, "sha256sum", |data| {
        hex_encode(&sha256_digest(data))
    })
}

// ---------------------------------------------------------------------------
// base64 encode/decode
// ---------------------------------------------------------------------------
//
// Backed by the `base64` crate (RFC 4648 standard alphabet with `=` padding)
// to avoid reimplementing the encoding from scratch.  See ADR-0023.

use base64::engine::general_purpose::STANDARD as B64_STANDARD;
use base64::Engine as _;

fn b64_encode(data: &[u8]) -> String {
    B64_STANDARD.encode(data)
}

struct Base64Flags {
    decode: bool,
    wrap_col: usize,
}

fn parse_base64_flags<'a>(argv: &'a [&'a str]) -> (Base64Flags, &'a [&'a str]) {
    let mut args = &argv[1..];
    let mut flags = Base64Flags {
        decode: false,
        wrap_col: 76,
    };
    while let Some(arg) = args.first() {
        if *arg == "-d" || *arg == "--decode" {
            flags.decode = true;
            args = &args[1..];
        } else if *arg == "-w" && args.len() > 1 {
            flags.wrap_col = args[1].parse().unwrap_or(76);
            args = &args[2..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            args = &args[1..];
        } else {
            break;
        }
    }
    (flags, args)
}

fn emit_base64_encoded(
    output: &mut dyn crate::UtilOutput,
    encoded: &str,
    wrap_col: usize,
    current_col: &mut usize,
) {
    if wrap_col == 0 {
        output.stdout(encoded.as_bytes());
        return;
    }
    for &byte in encoded.as_bytes() {
        if *current_col == wrap_col {
            output.stdout(b"\n");
            *current_col = 0;
        }
        output.stdout(&[byte]);
        *current_col += 1;
    }
}

fn base64_encode_stream(
    ctx: &mut UtilContext<'_>,
    args: &[&str],
    wrap_col: usize,
) -> Result<(), i32> {
    let mut carry = Vec::new();
    let mut current_col = 0usize;
    let mut saw_output = false;

    stream_input_chunks(ctx, args, "base64", |chunk, ctx| {
        let mut combined = Vec::with_capacity(carry.len() + chunk.len());
        combined.extend_from_slice(&carry);
        combined.extend_from_slice(chunk);
        let remainder = combined.len() % 3;
        let encode_len = combined.len() - remainder;
        if encode_len > 0 {
            let encoded = b64_encode(&combined[..encode_len]);
            emit_base64_encoded(ctx.output, &encoded, wrap_col, &mut current_col);
            saw_output = true;
        }
        carry.clear();
        carry.extend_from_slice(&combined[encode_len..]);
        Ok(())
    })?;

    if !carry.is_empty() {
        let encoded = b64_encode(&carry);
        emit_base64_encoded(ctx.output, &encoded, wrap_col, &mut current_col);
        saw_output = true;
    }
    if wrap_col == 0 || current_col > 0 || !saw_output {
        ctx.output.stdout(b"\n");
    }
    Ok(())
}

fn base64_decode_stream(ctx: &mut UtilContext<'_>, args: &[&str]) -> Result<(), i32> {
    // Collect all non-whitespace bytes (base64 allows embedded newlines
    // from `base64 -w 76` output and generally ignores whitespace), then
    // decode in one shot via the `base64` crate.
    let mut clean = Vec::new();
    stream_input_chunks(ctx, args, "base64", |chunk, _ctx| {
        clean.extend(chunk.iter().copied().filter(|b| !b.is_ascii_whitespace()));
        Ok(())
    })?;

    match B64_STANDARD.decode(&clean) {
        Ok(decoded) => {
            ctx.output.stdout(&decoded);
            Ok(())
        }
        Err(e) => {
            // Translate the base64 crate's error into the diagnostic the
            // historical implementation emitted, preserving agent-visible
            // behaviour for scripts that match on stderr content.
            let msg = match e {
                base64::DecodeError::InvalidByte(_, _)
                | base64::DecodeError::InvalidLastSymbol(_, _) => {
                    "base64: invalid base64 character\n".to_string()
                }
                base64::DecodeError::InvalidLength(_) => {
                    "base64: invalid base64 input length\n".to_string()
                }
                base64::DecodeError::InvalidPadding => "base64: invalid padding\n".to_string(),
            };
            ctx.output.stderr(msg.as_bytes());
            Err(1)
        }
    }
}

pub(crate) fn util_base64(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, args) = parse_base64_flags(argv);
    let result = if flags.decode {
        base64_decode_stream(ctx, args)
    } else {
        base64_encode_stream(ctx, args, flags.wrap_col)
    };
    i32::from(result.is_err())
}
