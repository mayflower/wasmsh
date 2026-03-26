//! Data/string utilities: seq, basename, dirname, expr, xargs, yes, md5sum, sha256sum, base64.

use crate::helpers::{get_input_text, hashsum_util, hex_encode, require_args};
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
    if step == 0 {
        ctx.output.stderr(b"seq: zero increment\n");
        return 1;
    }
    let mut i = start;
    let mut count = 0usize;
    while (step > 0 && i <= end) || (step < 0 && i >= end) {
        let s = format!("{i}\n");
        ctx.output.stdout(s.as_bytes());
        i += step;
        count += 1;
        if count >= SEQ_MAX_ITERATIONS {
            break;
        }
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
        return expr_eval_binary(ctx, args);
    }
    if args.len() == 1 {
        return expr_emit_scalar(ctx, args[0]);
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
// MD5 — clean-room implementation of RFC 1321
// ---------------------------------------------------------------------------

/// Per-round left-rotate amounts.
const MD5_S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// Pre-computed T[i] = floor(2^32 * |sin(i+1)|).
const MD5_K: [u32; 64] = [
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

#[allow(clippy::many_single_char_names)]
fn md5_digest(data: &[u8]) -> [u8; 16] {
    let mut a0: u32 = 0x6745_2301;
    let mut b0: u32 = 0xefcd_ab89;
    let mut c0: u32 = 0x98ba_dcfe;
    let mut d0: u32 = 0x1032_5476;

    // Pre-processing: pad to 64-byte blocks
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    // Process each 64-byte block
    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            let base = i * 4;
            *word = u32::from_le_bytes([
                chunk[base],
                chunk[base + 1],
                chunk[base + 2],
                chunk[base + 3],
            ]);
        }

        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);

        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                (a.wrapping_add(f).wrapping_add(MD5_K[i]).wrapping_add(m[g])).rotate_left(MD5_S[i]),
            );
            a = temp;
        }

        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut result = [0u8; 16];
    result[0..4].copy_from_slice(&a0.to_le_bytes());
    result[4..8].copy_from_slice(&b0.to_le_bytes());
    result[8..12].copy_from_slice(&c0.to_le_bytes());
    result[12..16].copy_from_slice(&d0.to_le_bytes());
    result
}

pub(crate) fn util_md5sum(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    hashsum_util(ctx, argv, "md5sum", |data| hex_encode(&md5_digest(data)))
}

// ---------------------------------------------------------------------------
// SHA-256 — clean-room implementation of FIPS 180-4
// ---------------------------------------------------------------------------

const SHA256_K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

const SHA256_H: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

#[allow(clippy::many_single_char_names)]
fn sha256_digest(data: &[u8]) -> [u8; 32] {
    let mut h = SHA256_H;

    // Pre-processing: pad to 64-byte blocks
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    // Process each 64-byte block
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().enumerate().take(16) {
            let base = i * 4;
            *word = u32::from_be_bytes([
                chunk[base],
                chunk[base + 1],
                chunk[base + 2],
                chunk[base + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut result = [0u8; 32];
    for (i, val) in h.iter().enumerate() {
        result[i * 4..(i + 1) * 4].copy_from_slice(&val.to_be_bytes());
    }
    result
}

pub(crate) fn util_sha256sum(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    hashsum_util(ctx, argv, "sha256sum", |data| {
        hex_encode(&sha256_digest(data))
    })
}

// ---------------------------------------------------------------------------
// base64 encode/decode
// ---------------------------------------------------------------------------

const B64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(B64_CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64_CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64_CHARS[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn b64_decode_char(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn b64_decode(input: &str) -> Result<Vec<u8>, &'static str> {
    // Strip whitespace
    let clean: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if clean.is_empty() {
        return Ok(Vec::new());
    }
    if !clean.len().is_multiple_of(4) {
        return Err("invalid base64 input length");
    }
    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    for chunk in clean.chunks_exact(4) {
        let a = b64_decode_char(chunk[0]).ok_or("invalid base64 character")?;
        let b = b64_decode_char(chunk[1]).ok_or("invalid base64 character")?;
        let c_val = if chunk[2] == b'=' {
            None
        } else {
            Some(b64_decode_char(chunk[2]).ok_or("invalid base64 character")?)
        };
        let d_val = if chunk[3] == b'=' {
            None
        } else {
            Some(b64_decode_char(chunk[3]).ok_or("invalid base64 character")?)
        };

        let triple = (u32::from(a) << 18)
            | (u32::from(b) << 12)
            | (u32::from(c_val.unwrap_or(0)) << 6)
            | u32::from(d_val.unwrap_or(0));

        out.push((triple >> 16) as u8);
        if c_val.is_some() {
            out.push((triple >> 8) as u8);
        }
        if d_val.is_some() {
            out.push(triple as u8);
        }
    }
    Ok(out)
}

pub(crate) fn util_base64(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut decode = false;
    let mut wrap_col: usize = 76;

    // Parse flags
    while let Some(arg) = args.first() {
        if *arg == "-d" || *arg == "--decode" {
            decode = true;
            args = &args[1..];
        } else if *arg == "-w" && args.len() > 1 {
            wrap_col = args[1].parse().unwrap_or(76);
            args = &args[2..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            args = &args[1..];
        } else {
            break;
        }
    }

    let text = get_input_text(ctx, args);
    let input_data = if !args.is_empty() || !text.is_empty() {
        text.into_bytes()
    } else if let Some(d) = ctx.stdin {
        d.to_vec()
    } else {
        Vec::new()
    };

    if decode {
        let input_str = String::from_utf8_lossy(&input_data);
        match b64_decode(&input_str) {
            Ok(decoded) => {
                ctx.output.stdout(&decoded);
                0
            }
            Err(e) => {
                let msg = format!("base64: {e}\n");
                ctx.output.stderr(msg.as_bytes());
                1
            }
        }
    } else {
        let encoded = b64_encode(&input_data);
        if wrap_col == 0 {
            ctx.output.stdout(encoded.as_bytes());
            ctx.output.stdout(b"\n");
        } else {
            // Wrap at specified column width
            let bytes = encoded.as_bytes();
            let mut pos = 0;
            while pos < bytes.len() {
                let end = (pos + wrap_col).min(bytes.len());
                ctx.output.stdout(&bytes[pos..end]);
                ctx.output.stdout(b"\n");
                pos = end;
            }
        }
        0
    }
}
