//! Binary utilities: xxd, dd, strings, split.

use wasmsh_fs::{OpenOptions, Vfs};

use crate::helpers::{
    emit_error, read_file_bytes, read_input_bytes, resolve_path, write_file_bytes,
};
use crate::UtilContext;

fn parse_size(s: &str) -> Option<u64> {
    if let Some(n) = s.strip_suffix('K').or(s.strip_suffix('k')) {
        return n.parse::<u64>().ok().map(|v| v * 1024);
    }
    if let Some(n) = s.strip_suffix('M').or(s.strip_suffix('m')) {
        return n.parse::<u64>().ok().map(|v| v * 1024 * 1024);
    }
    if let Some(n) = s.strip_suffix('G').or(s.strip_suffix('g')) {
        return n.parse::<u64>().ok().map(|v| v * 1024 * 1024 * 1024);
    }
    s.parse().ok()
}

// ---------------------------------------------------------------------------
// xxd — hex dump
// ---------------------------------------------------------------------------

struct XxdFlags {
    reverse: bool,
    plain: bool,
    c_include: bool,
    limit: Option<usize>,
    skip: usize,
    cols: usize,
}

fn parse_xxd_flags(ctx: &mut UtilContext<'_>, argv: &[&str]) -> Result<(XxdFlags, usize), i32> {
    let mut args = &argv[1..];
    let mut flags = XxdFlags {
        reverse: false,
        plain: false,
        c_include: false,
        limit: None,
        skip: 0,
        cols: 16,
    };
    let mut consumed = 1;

    while let Some(arg) = args.first() {
        match *arg {
            "-r" => flags.reverse = true,
            "-p" => flags.plain = true,
            "-i" => flags.c_include = true,
            "-l" if args.len() > 1 => {
                flags.limit = args[1].parse().ok();
                args = &args[2..];
                consumed += 2;
                continue;
            }
            "-s" if args.len() > 1 => {
                flags.skip = parse_xxd_usize(ctx, args[1])?;
                args = &args[2..];
                consumed += 2;
                continue;
            }
            "-c" if args.len() > 1 => {
                let v = parse_xxd_usize(ctx, args[1])?;
                flags.cols = if v == 0 { 16 } else { v };
                args = &args[2..];
                consumed += 2;
                continue;
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                let msg = format!("xxd: unknown option '{arg}'\n");
                ctx.output.stderr(msg.as_bytes());
                return Err(1);
            }
            _ => break,
        }
        args = &args[1..];
        consumed += 1;
    }
    Ok((flags, consumed))
}

fn parse_xxd_usize(ctx: &mut UtilContext<'_>, val: &str) -> Result<usize, i32> {
    val.parse::<usize>().map_err(|_| {
        let msg = format!("xxd: invalid number: '{val}'\n");
        ctx.output.stderr(msg.as_bytes());
        1
    })
}

pub(crate) fn util_xxd(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, consumed) = match parse_xxd_flags(ctx, argv) {
        Ok(v) => v,
        Err(status) => return status,
    };
    let args = &argv[consumed..];

    if flags.reverse {
        return xxd_reverse(ctx, args);
    }

    let data = match read_input_bytes(ctx, args, "xxd") {
        Ok(d) => d,
        Err(status) => return status,
    };

    let start = flags.skip.min(data.len());
    let data = &data[start..];
    let data = match flags.limit {
        Some(l) => &data[..l.min(data.len())],
        None => data,
    };

    if flags.plain {
        xxd_plain(ctx, data);
    } else if flags.c_include {
        xxd_c_include(ctx, data, args.first().copied().unwrap_or("stdin"));
    } else {
        xxd_default(ctx, data, start, flags.cols);
    }

    0
}

fn xxd_default(ctx: &mut UtilContext<'_>, data: &[u8], base_offset: usize, cols: usize) {
    for (chunk_idx, chunk) in data.chunks(cols).enumerate() {
        let offset = base_offset + chunk_idx * cols;
        let mut line = format!("{offset:08x}:");
        xxd_write_hex(&mut line, chunk, cols);
        line.push_str("  ");
        xxd_write_ascii(&mut line, chunk);
        line.push('\n');
        ctx.output.stdout(line.as_bytes());
    }
}

fn xxd_write_hex(line: &mut String, chunk: &[u8], cols: usize) {
    use std::fmt::Write;

    for (i, &b) in chunk.iter().enumerate() {
        if i % 2 == 0 {
            line.push(' ');
        }
        let _ = write!(line, "{b:02x}");
    }
    xxd_pad_hex(line, chunk.len(), cols);
}

fn xxd_pad_hex(line: &mut String, used: usize, cols: usize) {
    for i in 0..(cols - used) {
        if (used + i).is_multiple_of(2) {
            line.push(' ');
        }
        line.push_str("  ");
    }
}

fn xxd_write_ascii(line: &mut String, chunk: &[u8]) {
    for &b in chunk {
        line.push(if b.is_ascii_graphic() || b == b' ' {
            b as char
        } else {
            '.'
        });
    }
}

fn xxd_plain(ctx: &mut UtilContext<'_>, data: &[u8]) {
    use std::fmt::Write;
    let mut line = String::new();
    for (i, &b) in data.iter().enumerate() {
        let _ = write!(line, "{b:02x}");
        if (i + 1) % 30 == 0 {
            line.push('\n');
            ctx.output.stdout(line.as_bytes());
            line.clear();
        }
    }
    if !line.is_empty() {
        line.push('\n');
        ctx.output.stdout(line.as_bytes());
    }
}

fn xxd_c_include(ctx: &mut UtilContext<'_>, data: &[u8], name: &str) {
    use std::fmt::Write;
    // Sanitize name for C identifier
    let ident: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();

    let header = format!("unsigned char {ident}[] = {{\n");
    ctx.output.stdout(header.as_bytes());

    for (i, &b) in data.iter().enumerate() {
        let mut s = String::new();
        if i % 12 == 0 {
            s.push_str("  ");
        }
        let _ = write!(s, "0x{b:02x}");
        if i + 1 < data.len() {
            s.push_str(", ");
        }
        if (i + 1) % 12 == 0 || i + 1 == data.len() {
            s.push('\n');
        }
        ctx.output.stdout(s.as_bytes());
    }

    let footer = format!("}};\nunsigned int {ident}_len = {};\n", data.len());
    ctx.output.stdout(footer.as_bytes());
}

fn xxd_reverse_read_input(ctx: &mut UtilContext<'_>, file_args: &[&str]) -> Option<String> {
    if !file_args.is_empty() {
        let full = resolve_path(ctx.cwd, file_args[0]);
        match crate::helpers::read_text(ctx.fs, &full) {
            Ok(t) => Some(t),
            Err(e) => {
                emit_error(ctx.output, "xxd", file_args[0], &e);
                None
            }
        }
    } else {
        ctx.stdin.map(|d| String::from_utf8_lossy(d).to_string())
    }
}

fn xxd_extract_hex(line: &str) -> String {
    let hex_part = line.find(':').map_or(line, |pos| &line[pos + 1..]);
    let hex_only = hex_part
        .rfind("  ")
        .map_or(hex_part, |pos| &hex_part[..pos]);
    hex_only.chars().filter(char::is_ascii_hexdigit).collect()
}

fn xxd_hex_to_bytes(hex: &str) -> Vec<u8> {
    let mut result = Vec::new();
    for pair in hex.as_bytes().chunks(2) {
        if pair.len() == 2 {
            let s = std::str::from_utf8(pair).unwrap_or("00");
            if let Ok(b) = u8::from_str_radix(s, 16) {
                result.push(b);
            }
        }
    }
    result
}

fn xxd_reverse(ctx: &mut UtilContext<'_>, file_args: &[&str]) -> i32 {
    let Some(text) = xxd_reverse_read_input(ctx, file_args) else {
        if file_args.is_empty() {
            return 0;
        }
        return 1;
    };

    let mut result = Vec::new();
    for line in text.lines() {
        let hex = xxd_extract_hex(line);
        result.extend(xxd_hex_to_bytes(&hex));
    }

    ctx.output.stdout(&result);
    0
}

// ---------------------------------------------------------------------------
// dd — data copy/convert
// ---------------------------------------------------------------------------

const MAX_DD_SIZE: u64 = 64 * 1024 * 1024; // 64 MiB VFS limit

struct DdArgs<'a> {
    input_file: Option<&'a str>,
    output_file: Option<&'a str>,
    block_size: u64,
    count: Option<u64>,
    skip_blocks: u64,
    seek_blocks: u64,
    conv_ucase: bool,
    conv_lcase: bool,
    conv_notrunc: bool,
}

fn parse_dd_args<'a>(ctx: &mut UtilContext<'_>, argv: &'a [&'a str]) -> Result<DdArgs<'a>, i32> {
    let mut args = DdArgs {
        input_file: None,
        output_file: None,
        block_size: 512,
        count: None,
        skip_blocks: 0,
        seek_blocks: 0,
        conv_ucase: false,
        conv_lcase: false,
        conv_notrunc: false,
    };

    for arg in &argv[1..] {
        parse_dd_single_arg(ctx, arg, &mut args)?;
    }
    Ok(args)
}

fn parse_dd_single_arg<'a>(
    ctx: &mut UtilContext<'_>,
    arg: &'a str,
    args: &mut DdArgs<'a>,
) -> Result<(), i32> {
    if let Some(val) = arg.strip_prefix("if=") {
        args.input_file = Some(val);
    } else if let Some(val) = arg.strip_prefix("of=") {
        args.output_file = Some(val);
    } else if let Some(val) = arg.strip_prefix("bs=") {
        let Some(v) = parse_size(val) else {
            let msg = format!("dd: invalid number: '{val}'\n");
            ctx.output.stderr(msg.as_bytes());
            return Err(1);
        };
        if v > MAX_DD_SIZE {
            ctx.output.stderr(b"dd: block size too large\n");
            return Err(1);
        }
        args.block_size = v;
    } else if let Some(val) = arg.strip_prefix("count=") {
        args.count = val.parse().ok();
    } else if let Some(val) = arg.strip_prefix("skip=") {
        args.skip_blocks = parse_dd_u64(ctx, val)?;
    } else if let Some(val) = arg.strip_prefix("seek=") {
        args.seek_blocks = parse_dd_u64(ctx, val)?;
    } else if let Some(val) = arg.strip_prefix("conv=") {
        parse_dd_conv(ctx, val, args)?;
    }
    Ok(())
}

fn parse_dd_u64(ctx: &mut UtilContext<'_>, val: &str) -> Result<u64, i32> {
    val.parse::<u64>().map_err(|_| {
        let msg = format!("dd: invalid number: '{val}'\n");
        ctx.output.stderr(msg.as_bytes());
        1
    })
}

fn parse_dd_conv(ctx: &mut UtilContext<'_>, val: &str, args: &mut DdArgs<'_>) -> Result<(), i32> {
    for opt in val.split(',') {
        match opt {
            "ucase" => args.conv_ucase = true,
            "lcase" => args.conv_lcase = true,
            "notrunc" => args.conv_notrunc = true,
            _ => {
                let msg = format!("dd: unknown conv option '{opt}'\n");
                ctx.output.stderr(msg.as_bytes());
                return Err(1);
            }
        }
    }
    Ok(())
}

fn dd_copy(input_data: &[u8], args: &DdArgs<'_>) -> Result<(Vec<u8>, u64, u64), &'static str> {
    let bs = args.block_size as usize;

    let skip_bytes = args.skip_blocks.saturating_mul(args.block_size);
    if skip_bytes > MAX_DD_SIZE {
        return Err("dd: skip offset too large\n");
    }
    let input = if skip_bytes as usize >= input_data.len() {
        &[]
    } else {
        &input_data[skip_bytes as usize..]
    };

    let seek_bytes = args.seek_blocks.saturating_mul(args.block_size);
    if seek_bytes > MAX_DD_SIZE {
        return Err("dd: seek offset too large\n");
    }

    let mut output = vec![0u8; seek_bytes as usize];
    let (blocks_full, blocks_partial) = dd_copy_blocks(input, bs, args.count, &mut output);

    dd_apply_conversions(&mut output, args);

    Ok((output, blocks_full, blocks_partial))
}

fn dd_copy_blocks(input: &[u8], bs: usize, count: Option<u64>, output: &mut Vec<u8>) -> (u64, u64) {
    let mut blocks_full = 0u64;
    let mut blocks_partial = 0u64;
    let max_blocks = count.unwrap_or(u64::MAX);
    let mut offset = 0;
    let mut block_count = 0u64;

    while offset < input.len() && block_count < max_blocks {
        let end = (offset + bs).min(input.len());
        let chunk = &input[offset..end];
        if chunk.len() == bs {
            blocks_full += 1;
        } else {
            blocks_partial += 1;
        }
        output.extend_from_slice(chunk);
        offset += bs;
        block_count += 1;
    }
    (blocks_full, blocks_partial)
}

fn dd_apply_conversions(output: &mut [u8], args: &DdArgs<'_>) {
    if args.conv_ucase {
        for b in output.iter_mut() {
            b.make_ascii_uppercase();
        }
    }
    if args.conv_lcase {
        for b in output.iter_mut() {
            b.make_ascii_lowercase();
        }
    }
}

pub(crate) fn util_dd(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let args = match parse_dd_args(ctx, argv) {
        Ok(a) => a,
        Err(status) => return status,
    };

    let input_data = match dd_input_data(ctx, &args) {
        Ok(data) => data,
        Err(status) => return status,
    };

    let (output_data, blocks_full, blocks_partial) = match dd_copy(&input_data, &args) {
        Ok(v) => v,
        Err(msg) => {
            ctx.output.stderr(msg.as_bytes());
            return 1;
        }
    };

    let seek_bytes = args.seek_blocks.saturating_mul(args.block_size) as usize;
    let total_bytes = output_data.len() - seek_bytes;

    if dd_write_output(ctx, &args, &output_data, seek_bytes) != 0 {
        return 1;
    }

    dd_emit_stats(ctx, blocks_full, blocks_partial, total_bytes);

    0
}

fn dd_input_data(ctx: &mut UtilContext<'_>, args: &DdArgs<'_>) -> Result<Vec<u8>, i32> {
    if let Some(path) = args.input_file {
        read_file_bytes(ctx, path, "dd")
    } else if let Some(data) = ctx.stdin {
        Ok(data.to_vec())
    } else {
        Ok(Vec::new())
    }
}

fn dd_write_output(
    ctx: &mut UtilContext<'_>,
    args: &DdArgs<'_>,
    output_data: &[u8],
    seek_bytes: usize,
) -> i32 {
    let Some(path) = args.output_file else {
        ctx.output.stdout(&output_data[seek_bytes..]);
        return 0;
    };

    let full = resolve_path(ctx.cwd, path);
    let opts = if args.conv_notrunc {
        OpenOptions::append()
    } else {
        OpenOptions::write()
    };
    match ctx.fs.open(&full, opts) {
        Ok(h) => {
            if let Err(e) = ctx.fs.write_file(h, output_data) {
                ctx.fs.close(h);
                emit_error(ctx.output, "dd", path, &e);
                return 1;
            }
            ctx.fs.close(h);
            0
        }
        Err(e) => {
            emit_error(ctx.output, "dd", path, &e);
            1
        }
    }
}

fn dd_emit_stats(
    ctx: &mut UtilContext<'_>,
    blocks_full: u64,
    blocks_partial: u64,
    total_bytes: usize,
) {
    let stats = format!(
        "{blocks_full}+{blocks_partial} records in\n\
         {blocks_full}+{blocks_partial} records out\n\
         {total_bytes} bytes transferred\n"
    );
    ctx.output.stderr(stats.as_bytes());
}

// ---------------------------------------------------------------------------
// strings — extract printable strings
// ---------------------------------------------------------------------------

fn parse_strings_min_len(ctx: &mut UtilContext<'_>, val: &str) -> Result<usize, i32> {
    match val.parse::<usize>() {
        Ok(0) => Ok(1),
        Ok(v) => Ok(v),
        Err(_) => {
            let msg = format!("strings: invalid number: '{val}'\n");
            ctx.output.stderr(msg.as_bytes());
            Err(1)
        }
    }
}

fn parse_strings_flags(ctx: &mut UtilContext<'_>, argv: &[&str]) -> Result<(usize, usize), i32> {
    let mut args = &argv[1..];
    let mut min_len: usize = 4;
    let mut consumed = 1;

    while let Some(arg) = args.first() {
        if (*arg == "-n" || *arg == "--bytes") && args.len() > 1 {
            min_len = parse_strings_min_len(ctx, args[1])?;
            args = &args[2..];
            consumed += 2;
        } else if let Some(rest) = arg.strip_prefix("-n") {
            min_len = parse_strings_min_len(ctx, rest)?;
            args = &args[1..];
            consumed += 1;
        } else if arg.starts_with('-') && arg.len() > 1 {
            args = &args[1..];
            consumed += 1;
        } else {
            break;
        }
    }
    Ok((min_len, consumed))
}

fn extract_printable_strings(ctx: &mut UtilContext<'_>, data: &[u8], min_len: usize) {
    let mut current = String::new();
    for &b in data {
        if (0x20..=0x7E).contains(&b) {
            current.push(b as char);
        } else {
            if current.len() >= min_len {
                ctx.output.stdout(current.as_bytes());
                ctx.output.stdout(b"\n");
            }
            current.clear();
        }
    }
    if current.len() >= min_len {
        ctx.output.stdout(current.as_bytes());
        ctx.output.stdout(b"\n");
    }
}

pub(crate) fn util_strings(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (min_len, consumed) = match parse_strings_flags(ctx, argv) {
        Ok(v) => v,
        Err(status) => return status,
    };
    let args = &argv[consumed..];

    let data = match read_input_bytes(ctx, args, "strings") {
        Ok(d) => d,
        Err(status) => return status,
    };

    extract_printable_strings(ctx, &data, min_len);
    0
}

// ---------------------------------------------------------------------------
// split — split file into pieces
// ---------------------------------------------------------------------------

struct SplitArgs<'a> {
    lines: Option<usize>,
    byte_size: Option<u64>,
    chunks: Option<usize>,
    numeric_suffix: bool,
    file_args_start: usize,
    _phantom: std::marker::PhantomData<&'a str>,
}

fn parse_split_args<'a>(
    ctx: &mut UtilContext<'_>,
    argv: &'a [&'a str],
) -> Result<SplitArgs<'a>, i32> {
    let mut args = &argv[1..];
    let mut result = SplitArgs {
        lines: None,
        byte_size: None,
        chunks: None,
        numeric_suffix: false,
        file_args_start: 0,
        _phantom: std::marker::PhantomData,
    };
    let mut consumed = 1;

    while let Some(arg) = args.first() {
        match *arg {
            "-l" if args.len() > 1 => {
                result.lines = args[1].parse().ok();
                args = &args[2..];
                consumed += 2;
            }
            "-b" if args.len() > 1 => {
                result.byte_size = parse_size(args[1]);
                args = &args[2..];
                consumed += 2;
            }
            "-n" if args.len() > 1 => {
                result.chunks = args[1].parse().ok();
                args = &args[2..];
                consumed += 2;
            }
            "-d" => {
                result.numeric_suffix = true;
                args = &args[1..];
                consumed += 1;
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                let msg = format!("split: unknown option '{arg}'\n");
                ctx.output.stderr(msg.as_bytes());
                return Err(1);
            }
            _ => break,
        }
    }
    result.file_args_start = consumed;
    Ok(result)
}

fn split_into_pieces(
    ctx: &mut UtilContext<'_>,
    input_data: &[u8],
    args: &SplitArgs<'_>,
) -> Result<Vec<Vec<u8>>, i32> {
    if let Some(n) = args.chunks {
        return split_by_chunks(ctx, input_data, n);
    }
    if let Some(bs) = args.byte_size {
        return split_by_bytes(ctx, input_data, bs as usize);
    }
    split_by_lines(ctx, input_data, args.lines.unwrap_or(1000))
}

fn split_by_chunks(
    ctx: &mut UtilContext<'_>,
    input_data: &[u8],
    chunks: usize,
) -> Result<Vec<Vec<u8>>, i32> {
    if chunks == 0 {
        ctx.output.stderr(b"split: invalid number of chunks\n");
        return Err(1);
    }
    let chunk_size = input_data.len().div_ceil(chunks);
    Ok(input_data
        .chunks(chunk_size.max(1))
        .map(<[u8]>::to_vec)
        .collect())
}

fn split_by_bytes(
    ctx: &mut UtilContext<'_>,
    input_data: &[u8],
    byte_size: usize,
) -> Result<Vec<Vec<u8>>, i32> {
    if byte_size == 0 {
        ctx.output.stderr(b"split: invalid byte size\n");
        return Err(1);
    }
    Ok(input_data.chunks(byte_size).map(<[u8]>::to_vec).collect())
}

fn split_by_lines(
    ctx: &mut UtilContext<'_>,
    input_data: &[u8],
    line_count: usize,
) -> Result<Vec<Vec<u8>>, i32> {
    if line_count == 0 {
        ctx.output.stderr(b"split: invalid line count\n");
        return Err(1);
    }
    let text = String::from_utf8_lossy(input_data);
    let all_lines: Vec<&str> = text.lines().collect();
    Ok(all_lines
        .chunks(line_count)
        .map(|chunk| split_line_chunk(chunk, text.ends_with('\n')))
        .collect())
}

fn split_line_chunk(chunk: &[&str], ends_with_newline: bool) -> Vec<u8> {
    let mut buf = String::new();
    for (i, line) in chunk.iter().enumerate() {
        buf.push_str(line);
        if i + 1 < chunk.len() || ends_with_newline {
            buf.push('\n');
        }
    }
    buf.into_bytes()
}

pub(crate) fn util_split(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let split_args = match parse_split_args(ctx, argv) {
        Ok(a) => a,
        Err(status) => return status,
    };
    let args = &argv[split_args.file_args_start..];

    let (input_data, prefix) = if !args.is_empty() && args[0] != "-" {
        let data = match read_file_bytes(ctx, args[0], "split") {
            Ok(d) => d,
            Err(status) => return status,
        };
        let p = if args.len() > 1 { args[1] } else { "x" };
        (data, p)
    } else {
        let data = if let Some(d) = ctx.stdin {
            d.to_vec()
        } else {
            Vec::new()
        };
        let p = if args.len() > 1 { args[1] } else { "x" };
        (data, p)
    };

    if input_data.is_empty() {
        return 0;
    }

    let pieces = match split_into_pieces(ctx, &input_data, &split_args) {
        Ok(p) => p,
        Err(status) => return status,
    };

    for (i, piece) in pieces.iter().enumerate() {
        let suffix = if split_args.numeric_suffix {
            format!("{i:02}")
        } else {
            suffix_alpha(i)
        };
        let name = format!("{prefix}{suffix}");
        let full = resolve_path(ctx.cwd, &name);
        if write_file_bytes(ctx, "split", &full, piece) != 0 {
            return 1;
        }
    }

    0
}

/// Generate alphabetic suffix: 0 -> "aa", 1 -> "ab", ..., 25 -> "az", 26 -> "ba", ...
/// Extends to 3-letter suffixes for n >= 676.
fn suffix_alpha(n: usize) -> String {
    if n < 26 * 26 {
        let first = (n / 26) as u8 + b'a';
        let second = (n % 26) as u8 + b'a';
        format!("{}{}", first as char, second as char)
    } else {
        let a = ((n / (26 * 26)) % 26) as u8 + b'a';
        let b = ((n / 26) % 26) as u8 + b'a';
        let c = (n % 26) as u8 + b'a';
        format!("{}{}{}", a as char, b as char, c as char)
    }
}

// ---------------------------------------------------------------------------
// file — detect file type
// ---------------------------------------------------------------------------

struct FileFlags {
    brief: bool,
    mime_type: bool,
}

fn parse_file_flags(ctx: &mut UtilContext<'_>, argv: &[&str]) -> Result<(FileFlags, usize), i32> {
    let mut args = &argv[1..];
    let mut flags = FileFlags {
        brief: false,
        mime_type: false,
    };
    let mut consumed = 1;

    while let Some(arg) = args.first() {
        match *arg {
            "-b" | "--brief" => flags.brief = true,
            "-i" | "--mime-type" => flags.mime_type = true,
            _ if arg.starts_with('-') && arg.len() > 1 => {
                for ch in arg[1..].chars() {
                    match ch {
                        'b' => flags.brief = true,
                        'i' => flags.mime_type = true,
                        _ => {
                            let msg = format!("file: unknown option '-{ch}'\n");
                            ctx.output.stderr(msg.as_bytes());
                            return Err(1);
                        }
                    }
                }
            }
            _ => break,
        }
        args = &args[1..];
        consumed += 1;
    }
    Ok((flags, consumed))
}

fn emit_file_result(ctx: &mut UtilContext<'_>, path: &str, desc: &str, brief: bool) {
    let line = if brief {
        format!("{desc}\n")
    } else {
        format!("{path}: {desc}\n")
    };
    ctx.output.stdout(line.as_bytes());
}

pub(crate) fn util_file(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, consumed) = match parse_file_flags(ctx, argv) {
        Ok(v) => v,
        Err(status) => return status,
    };
    let args = &argv[consumed..];

    if args.is_empty() {
        ctx.output.stderr(b"file: missing operand\n");
        return 1;
    }

    let mut status = 0;
    for path in args {
        let full = resolve_path(ctx.cwd, path);

        match ctx.fs.stat(&full) {
            Ok(meta) if meta.is_dir => {
                let desc = if flags.mime_type {
                    "inode/directory"
                } else {
                    "directory"
                };
                emit_file_result(ctx, path, desc, flags.brief);
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                emit_error(ctx.output, "file", path, &e);
                status = 1;
                continue;
            }
        }

        let Ok(data) = read_file_bytes(ctx, path, "file") else {
            status = 1;
            continue;
        };

        let desc = if flags.mime_type {
            detect_mime_type(&data, path)
        } else {
            detect_file_type(&data, path)
        };
        emit_file_result(ctx, path, &desc, flags.brief);
    }

    status
}

/// Known binary format identified by magic bytes.
#[derive(Clone, Copy)]
enum MagicKind {
    Png,
    Gif,
    Jpeg,
    Pdf,
    Zip,
    Gzip,
    Elf,
    Wasm,
}

fn detect_magic_four_byte(data: &[u8]) -> Option<MagicKind> {
    if data.len() < 4 {
        return None;
    }
    match (data[0], data.get(1..4)) {
        (0x89, Some(b"PNG")) => Some(MagicKind::Png),
        (0x7F, Some(b"ELF")) => Some(MagicKind::Elf),
        (0x00, Some(b"asm")) => Some(MagicKind::Wasm),
        _ => None,
    }
}

fn detect_magic(data: &[u8]) -> Option<MagicKind> {
    const MAGIC_PATTERNS: &[(MagicKind, &[u8])] =
        &[(MagicKind::Pdf, b"%PDF"), (MagicKind::Zip, b"PK\x03\x04")];

    if let Some(kind) = detect_magic_four_byte(data) {
        return Some(kind);
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Some(MagicKind::Gif);
    }
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(MagicKind::Jpeg);
    }
    for (kind, prefix) in MAGIC_PATTERNS {
        if data.starts_with(prefix) {
            return Some(*kind);
        }
    }
    if data.starts_with(&[0x1F, 0x8B]) {
        return Some(MagicKind::Gzip);
    }
    None
}

fn magic_to_type(kind: MagicKind) -> &'static str {
    match kind {
        MagicKind::Png => "PNG image data",
        MagicKind::Gif => "GIF image data",
        MagicKind::Jpeg => "JPEG image data",
        MagicKind::Pdf => "PDF document",
        MagicKind::Zip => "Zip archive data",
        MagicKind::Gzip => "gzip compressed data",
        MagicKind::Elf => "ELF executable",
        MagicKind::Wasm => "WebAssembly (wasm) binary module",
    }
}

fn magic_to_mime(kind: MagicKind) -> &'static str {
    match kind {
        MagicKind::Png => "image/png",
        MagicKind::Gif => "image/gif",
        MagicKind::Jpeg => "image/jpeg",
        MagicKind::Pdf => "application/pdf",
        MagicKind::Zip => "application/zip",
        MagicKind::Gzip => "application/gzip",
        MagicKind::Elf => "application/x-executable",
        MagicKind::Wasm => "application/wasm",
    }
}

/// Detect text format from the first non-whitespace character. Returns `(type_desc, mime)`.
fn detect_text_format(data: &[u8]) -> Option<(&'static str, &'static str)> {
    let first_nws = data.iter().find(|b| !b.is_ascii_whitespace())?;
    if *first_nws == b'{' {
        return Some(("JSON text data", "application/json"));
    }
    if *first_nws == b'<' {
        let text = String::from_utf8_lossy(data);
        let lower = text.to_lowercase();
        if lower.contains("<!doctype") || lower.contains("<html") {
            return Some(("HTML document", "text/html"));
        }
        if lower.contains("<?xml") {
            return Some(("XML document", "application/xml"));
        }
    }
    None
}

fn detect_file_type(data: &[u8], path: &str) -> String {
    if let Some(kind) = detect_magic(data) {
        return magic_to_type(kind).to_string();
    }

    if data.len() >= 2 && &data[..2] == b"#!" {
        let end = data
            .iter()
            .position(|&b| b == b'\n')
            .unwrap_or(data.len().min(128));
        let shebang = String::from_utf8_lossy(&data[..end]);
        return format!("script text executable ({shebang})");
    }

    if let Some((desc, _)) = detect_text_format(data) {
        return desc.to_string();
    }

    if let Some(desc) = extension_type(path) {
        return desc.to_string();
    }

    if is_valid_utf8_text(data) {
        return if data.is_ascii() {
            "ASCII text"
        } else {
            "UTF-8 Unicode text"
        }
        .to_string();
    }

    "data".to_string()
}

fn detect_mime_type(data: &[u8], path: &str) -> String {
    if let Some(kind) = detect_magic(data) {
        return magic_to_mime(kind).to_string();
    }

    if let Some((_, mime)) = detect_text_format(data) {
        return mime.to_string();
    }

    if let Some(mime) = extension_mime(path) {
        return mime.to_string();
    }

    if is_valid_utf8_text(data) {
        return "text/plain".to_string();
    }

    "application/octet-stream".to_string()
}

fn extension_type(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?;
    match ext.to_lowercase().as_str() {
        "rs" => Some("Rust source"),
        "py" => Some("Python script"),
        "js" => Some("JavaScript source"),
        "ts" => Some("TypeScript source"),
        "json" => Some("JSON text data"),
        "toml" => Some("TOML configuration"),
        "yaml" | "yml" => Some("YAML data"),
        "md" => Some("Markdown document"),
        "html" | "htm" => Some("HTML document"),
        "css" => Some("CSS stylesheet"),
        "sh" => Some("Bourne shell script"),
        "txt" => Some("ASCII text"),
        "csv" => Some("CSV text data"),
        "xml" => Some("XML document"),
        "wasm" => Some("WebAssembly binary"),
        "tar" => Some("tar archive"),
        "gz" => Some("gzip compressed data"),
        "zip" => Some("Zip archive data"),
        _ => None,
    }
}

fn extension_mime(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?;
    match ext.to_lowercase().as_str() {
        "json" => Some("application/json"),
        "md" => Some("text/markdown"),
        "html" | "htm" => Some("text/html"),
        "css" => Some("text/css"),
        "xml" => Some("application/xml"),
        "wasm" => Some("application/wasm"),
        "tar" => Some("application/x-tar"),
        "gz" => Some("application/gzip"),
        "zip" => Some("application/zip"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "pdf" => Some("application/pdf"),
        "rs" | "py" | "js" | "ts" | "sh" | "txt" | "csv" | "toml" | "yaml" | "yml" => {
            Some("text/plain")
        }
        _ => None,
    }
}

/// Check if data looks like valid UTF-8 text (no control chars except common ones).
fn is_valid_utf8_text(data: &[u8]) -> bool {
    if data.is_empty() {
        return true;
    }
    let Ok(text) = std::str::from_utf8(data) else {
        return false;
    };
    // Check for non-text control characters
    for ch in text.chars() {
        if ch.is_control() && ch != '\n' && ch != '\r' && ch != '\t' && ch != '\x0C' {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UtilContext, VecOutput};
    use wasmsh_fs::{MemoryFs, OpenOptions, Vfs};

    fn run_util(
        func: fn(&mut UtilContext<'_>, &[&str]) -> i32,
        argv: &[&str],
        fs: &mut MemoryFs,
        stdin: Option<&[u8]>,
    ) -> (i32, VecOutput) {
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs,
                output: &mut output,
                cwd: "/",
                stdin,
                state: None,
            };
            func(&mut ctx, argv)
        };
        (status, output)
    }

    #[test]
    fn xxd_basic() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_util(util_xxd, &["xxd"], &mut fs, Some(b"Hello World\n"));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("00000000:"));
        assert!(s.contains("Hello World"));
    }

    #[test]
    fn xxd_plain_mode() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_util(util_xxd, &["xxd", "-p"], &mut fs, Some(b"AB"));
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "4142");
    }

    #[test]
    fn xxd_reverse() {
        let mut fs = MemoryFs::new();
        let hex = b"00000000: 4142 4344\n";
        let (status, out) = run_util(util_xxd, &["xxd", "-r"], &mut fs, Some(hex));
        assert_eq!(status, 0);
        assert_eq!(&out.stdout, b"ABCD");
    }

    #[test]
    fn xxd_c_include() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_util(util_xxd, &["xxd", "-i"], &mut fs, Some(b"AB"));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("0x41"));
        assert!(s.contains("0x42"));
        assert!(s.contains("_len = 2"));
    }

    #[test]
    fn xxd_limit() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_util(
            util_xxd,
            &["xxd", "-l", "2", "-p"],
            &mut fs,
            Some(b"ABCDEF"),
        );
        assert_eq!(status, 0);
        assert_eq!(out.stdout_str().trim(), "4142");
    }

    #[test]
    fn dd_basic() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/input.dat", OpenOptions::write()).unwrap();
        fs.write_file(h, b"hello world").unwrap();
        fs.close(h);
        let (status, out) = run_util(
            util_dd,
            &["dd", "if=/input.dat", "of=/output.dat"],
            &mut fs,
            None,
        );
        assert_eq!(status, 0);
        // Verify output file
        let h = fs.open("/output.dat", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&data, b"hello world");
        // Stats go to stderr
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("records in"));
    }

    #[test]
    fn dd_ucase() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_util(util_dd, &["dd", "conv=ucase"], &mut fs, Some(b"hello"));
        assert_eq!(status, 0);
        assert_eq!(&out.stdout, b"HELLO");
    }

    #[test]
    fn dd_count() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_util(util_dd, &["dd", "bs=1", "count=3"], &mut fs, Some(b"hello"));
        assert_eq!(status, 0);
        assert_eq!(&out.stdout, b"hel");
    }

    #[test]
    fn strings_basic() {
        let mut fs = MemoryFs::new();
        let mut data = Vec::new();
        data.extend_from_slice(b"\x00\x01hello world\x00\x02ab\x00longer string here\x00");
        let (status, out) = run_util(util_strings, &["strings"], &mut fs, Some(&data));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("hello world"));
        assert!(s.contains("longer string here"));
        assert!(!s.contains("\nab\n")); // too short (< 4)
    }

    #[test]
    fn strings_min_len() {
        let mut fs = MemoryFs::new();
        let data = b"\x00ab\x00abcde\x00";
        let (status, out) = run_util(util_strings, &["strings", "-n", "2"], &mut fs, Some(data));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("ab"));
        assert!(s.contains("abcde"));
    }

    #[test]
    fn split_by_lines() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/input.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"a\nb\nc\nd\ne\n").unwrap();
        fs.close(h);
        let (status, _) = run_util(
            util_split,
            &["split", "-l", "2", "/input.txt"],
            &mut fs,
            None,
        );
        assert_eq!(status, 0);
        // Check pieces exist
        let h = fs.open("/xaa", OpenOptions::read()).unwrap();
        let d = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(std::str::from_utf8(&d).unwrap(), "a\nb\n");

        let h = fs.open("/xab", OpenOptions::read()).unwrap();
        let d = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(std::str::from_utf8(&d).unwrap(), "c\nd\n");
    }

    #[test]
    fn split_numeric_suffix() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/input.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"a\nb\nc\nd\n").unwrap();
        fs.close(h);
        let (status, _) = run_util(
            util_split,
            &["split", "-l", "2", "-d", "/input.txt", "part"],
            &mut fs,
            None,
        );
        assert_eq!(status, 0);
        assert!(fs.stat("/part00").is_ok());
        assert!(fs.stat("/part01").is_ok());
    }

    #[test]
    fn split_by_bytes() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/data.bin", OpenOptions::write()).unwrap();
        fs.write_file(h, b"0123456789").unwrap();
        fs.close(h);
        let (status, _) = run_util(
            util_split,
            &["split", "-b", "3", "/data.bin", "chunk"],
            &mut fs,
            None,
        );
        assert_eq!(status, 0);
        let h = fs.open("/chunkaa", OpenOptions::read()).unwrap();
        let d = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&d, b"012");

        let h = fs.open("/chunkad", OpenOptions::read()).unwrap();
        let d = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&d, b"9");
    }

    // -----------------------------------------------------------------------
    // file tests
    // -----------------------------------------------------------------------

    #[test]
    fn file_png_magic() {
        let mut fs = MemoryFs::new();
        let mut png = vec![0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&[0; 32]); // some extra data
        let h = fs.open("/image.png", OpenOptions::write()).unwrap();
        fs.write_file(h, &png).unwrap();
        fs.close(h);

        let (status, out) = run_util(util_file, &["file", "/image.png"], &mut fs, None);
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("PNG image data"), "got: {s}");
    }

    #[test]
    fn file_json_content() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/data.json", OpenOptions::write()).unwrap();
        fs.write_file(h, b"{\"key\":\"val\"}").unwrap();
        fs.close(h);

        let (status, out) = run_util(util_file, &["file", "/data.json"], &mut fs, None);
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("JSON text data"), "got: {s}");
    }

    #[test]
    fn file_plain_text() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/readme.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"Hello world, this is ASCII text.")
            .unwrap();
        fs.close(h);

        let (status, out) = run_util(util_file, &["file", "/readme.txt"], &mut fs, None);
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("ASCII text"), "got: {s}");
    }

    #[test]
    fn file_extension_fallback() {
        let mut fs = MemoryFs::new();
        // Plain text content but .py extension — should detect via extension
        let h = fs.open("/script.py", OpenOptions::write()).unwrap();
        fs.write_file(h, b"print('hello')").unwrap();
        fs.close(h);

        let (status, out) = run_util(util_file, &["file", "/script.py"], &mut fs, None);
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("Python script"), "got: {s}");
    }

    #[test]
    fn file_binary_data() {
        let mut fs = MemoryFs::new();
        let data: Vec<u8> = (0u8..=255).collect();
        let h = fs.open("/binary.bin", OpenOptions::write()).unwrap();
        fs.write_file(h, &data).unwrap();
        fs.close(h);

        let (status, out) = run_util(util_file, &["file", "/binary.bin"], &mut fs, None);
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("data"), "got: {s}");
    }

    #[test]
    fn file_brief_mode() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/test.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"just text").unwrap();
        fs.close(h);

        let (status, out) = run_util(util_file, &["file", "-b", "/test.txt"], &mut fs, None);
        assert_eq!(status, 0);
        let s = out.stdout_str();
        // Brief mode should NOT include the filename prefix
        assert!(!s.contains("/test.txt:"), "got: {s}");
        assert!(s.contains("ASCII text") || s.contains("text"), "got: {s}");
    }

    #[test]
    fn file_mime_type() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/doc.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"plain text content").unwrap();
        fs.close(h);

        let (status, out) = run_util(util_file, &["file", "-i", "/doc.txt"], &mut fs, None);
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("text/plain"), "got: {s}");
    }

    #[test]
    fn file_gzip_magic() {
        let mut fs = MemoryFs::new();
        let mut gz = vec![0x1Fu8, 0x8B, 0x08, 0x00];
        gz.extend_from_slice(&[0; 20]); // filler
        let h = fs.open("/archive.gz", OpenOptions::write()).unwrap();
        fs.write_file(h, &gz).unwrap();
        fs.close(h);

        let (status, out) = run_util(util_file, &["file", "/archive.gz"], &mut fs, None);
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("gzip compressed data"), "got: {s}");
    }

    #[test]
    fn file_shebang() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/run.sh", OpenOptions::write()).unwrap();
        fs.write_file(h, b"#!/bin/bash\necho hi\n").unwrap();
        fs.close(h);

        let (status, out) = run_util(util_file, &["file", "/run.sh"], &mut fs, None);
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("script text executable"), "got: {s}");
    }

    #[test]
    fn file_missing() {
        let mut fs = MemoryFs::new();
        let (status, _out) = run_util(util_file, &["file", "/nonexistent"], &mut fs, None);
        assert_eq!(status, 1);
    }

    #[test]
    fn parse_size_values() {
        assert_eq!(parse_size("512"), Some(512));
        assert_eq!(parse_size("1K"), Some(1024));
        assert_eq!(parse_size("2M"), Some(2 * 1024 * 1024));
        assert_eq!(parse_size("1G"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size("abc"), None);
    }

    // -------------------------------------------------------------------
    // xxd -i  C include format
    // -------------------------------------------------------------------

    #[test]
    fn xxd_c_include_from_file() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/data.bin", OpenOptions::write()).unwrap();
        fs.write_file(h, b"\x01\x02\x03").unwrap();
        fs.close(h);
        let (status, out) = run_util(util_xxd, &["xxd", "-i", "/data.bin"], &mut fs, None);
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(
            s.contains("unsigned char"),
            "expected C array header, got: {s}"
        );
        assert!(s.contains("0x01"));
        assert!(s.contains("0x02"));
        assert!(s.contains("0x03"));
        assert!(s.contains("_len = 3"));
    }

    // -------------------------------------------------------------------
    // xxd -r  reverse hex dump
    // -------------------------------------------------------------------

    #[test]
    fn xxd_reverse_plain_hex() {
        let mut fs = MemoryFs::new();
        // Feed plain hex (no offsets) through -r
        let hex_input = b"48656c6c6f";
        let (status, out) = run_util(util_xxd, &["xxd", "-r"], &mut fs, Some(hex_input));
        assert_eq!(status, 0);
        assert_eq!(&out.stdout, b"Hello");
    }

    // -------------------------------------------------------------------
    // xxd -l 5  limit bytes
    // -------------------------------------------------------------------

    #[test]
    fn xxd_limit_5_bytes() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_util(
            util_xxd,
            &["xxd", "-l", "5"],
            &mut fs,
            Some(b"Hello World!"),
        );
        assert_eq!(status, 0);
        let s = out.stdout_str();
        // Only the first 5 bytes should appear
        assert!(s.contains("Hello"));
        assert!(!s.contains("World"));
    }

    // -------------------------------------------------------------------
    // xxd -s 3  skip offset
    // -------------------------------------------------------------------

    #[test]
    fn xxd_skip_3_bytes() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_util(
            util_xxd,
            &["xxd", "-s", "3", "-p"],
            &mut fs,
            Some(b"ABCDef"),
        );
        assert_eq!(status, 0);
        let s = out.stdout_str().trim().to_string();
        // Skipping 3 bytes ("ABC"), remaining is "Def" = 44 65 66
        assert_eq!(s, "446566");
    }

    // -------------------------------------------------------------------
    // dd conv=lcase
    // -------------------------------------------------------------------

    #[test]
    fn dd_conv_lcase() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_util(
            util_dd,
            &["dd", "conv=lcase"],
            &mut fs,
            Some(b"HELLO World"),
        );
        assert_eq!(status, 0);
        assert_eq!(&out.stdout, b"hello world");
    }

    // -------------------------------------------------------------------
    // dd conv=ucase (already tested partially, cover from file)
    // -------------------------------------------------------------------

    #[test]
    fn dd_conv_ucase_from_file() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/input.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"lower case text").unwrap();
        fs.close(h);
        let (status, out) = run_util(
            util_dd,
            &["dd", "if=/input.txt", "conv=ucase"],
            &mut fs,
            None,
        );
        assert_eq!(status, 0);
        assert_eq!(&out.stdout, b"LOWER CASE TEXT");
    }

    // -------------------------------------------------------------------
    // dd count=2 bs=5 with specific block handling
    // -------------------------------------------------------------------

    #[test]
    fn dd_count_with_bs() {
        let mut fs = MemoryFs::new();
        // 20 bytes input, bs=5, count=2 => read 10 bytes
        let (status, out) = run_util(
            util_dd,
            &["dd", "bs=5", "count=2"],
            &mut fs,
            Some(b"abcdefghijklmnopqrst"),
        );
        assert_eq!(status, 0);
        assert_eq!(&out.stdout, b"abcdefghij");
    }

    // -------------------------------------------------------------------
    // dd stderr stats output
    // -------------------------------------------------------------------

    #[test]
    fn dd_stderr_stats() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_util(util_dd, &["dd", "bs=1", "count=3"], &mut fs, Some(b"abcde"));
        assert_eq!(status, 0);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("records in"), "expected records in: {err}");
        assert!(err.contains("records out"), "expected records out: {err}");
        assert!(
            err.contains("bytes transferred"),
            "expected bytes transferred: {err}"
        );
    }

    // -------------------------------------------------------------------
    // strings -n 8 with longer minimum
    // -------------------------------------------------------------------

    #[test]
    fn strings_min_len_8() {
        let mut fs = MemoryFs::new();
        let mut data = Vec::new();
        data.extend_from_slice(b"\x00short\x00");
        data.extend_from_slice(b"\x00longstring\x00");
        data.extend_from_slice(b"\x00tiny\x00");
        let (status, out) = run_util(util_strings, &["strings", "-n", "8"], &mut fs, Some(&data));
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("longstring"), "expected longstring: {s}");
        assert!(!s.contains("short"), "should not contain short: {s}");
        assert!(!s.contains("tiny"), "should not contain tiny: {s}");
    }

    // -------------------------------------------------------------------
    // split -b 10 by bytes
    // -------------------------------------------------------------------

    #[test]
    fn split_by_10_bytes() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/data.bin", OpenOptions::write()).unwrap();
        fs.write_file(h, b"0123456789abcdefghij12345").unwrap();
        fs.close(h);
        let (status, _) = run_util(
            util_split,
            &["split", "-b", "10", "/data.bin", "p"],
            &mut fs,
            None,
        );
        assert_eq!(status, 0);
        let h = fs.open("/paa", OpenOptions::read()).unwrap();
        let d = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&d, b"0123456789");

        let h = fs.open("/pab", OpenOptions::read()).unwrap();
        let d = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&d, b"abcdefghij");

        let h = fs.open("/pac", OpenOptions::read()).unwrap();
        let d = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&d, b"12345");
    }

    // -------------------------------------------------------------------
    // split -n 3 into N chunks
    // -------------------------------------------------------------------

    #[test]
    fn split_into_3_chunks() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/data.bin", OpenOptions::write()).unwrap();
        fs.write_file(h, b"123456789").unwrap(); // 9 bytes / 3 = 3 each
        fs.close(h);
        let (status, _) = run_util(
            util_split,
            &["split", "-n", "3", "/data.bin", "c"],
            &mut fs,
            None,
        );
        assert_eq!(status, 0);
        let h = fs.open("/caa", OpenOptions::read()).unwrap();
        let d = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&d, b"123");

        let h = fs.open("/cab", OpenOptions::read()).unwrap();
        let d = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&d, b"456");

        let h = fs.open("/cac", OpenOptions::read()).unwrap();
        let d = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&d, b"789");
    }

    // -------------------------------------------------------------------
    // split -d numeric suffixes
    // -------------------------------------------------------------------

    #[test]
    fn split_numeric_suffixes() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/input.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"line1\nline2\nline3\n").unwrap();
        fs.close(h);
        let (status, _) = run_util(
            util_split,
            &["split", "-l", "1", "-d", "/input.txt", "n"],
            &mut fs,
            None,
        );
        assert_eq!(status, 0);
        assert!(fs.stat("/n00").is_ok(), "n00 should exist");
        assert!(fs.stat("/n01").is_ok(), "n01 should exist");
        assert!(fs.stat("/n02").is_ok(), "n02 should exist");
    }

    // -------------------------------------------------------------------
    // file -i MIME types for more formats
    // -------------------------------------------------------------------

    #[test]
    fn file_mime_png() {
        let mut fs = MemoryFs::new();
        let mut png = vec![0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&[0; 20]);
        let h = fs.open("/img.png", OpenOptions::write()).unwrap();
        fs.write_file(h, &png).unwrap();
        fs.close(h);
        let (status, out) = run_util(util_file, &["file", "-i", "/img.png"], &mut fs, None);
        assert_eq!(status, 0);
        assert!(out.stdout_str().contains("image/png"));
    }

    #[test]
    fn file_mime_gzip() {
        let mut fs = MemoryFs::new();
        let mut gz = vec![0x1Fu8, 0x8B, 0x08, 0x00];
        gz.extend_from_slice(&[0; 20]);
        let h = fs.open("/arc.gz", OpenOptions::write()).unwrap();
        fs.write_file(h, &gz).unwrap();
        fs.close(h);
        let (status, out) = run_util(util_file, &["file", "-i", "/arc.gz"], &mut fs, None);
        assert_eq!(status, 0);
        assert!(out.stdout_str().contains("application/gzip"));
    }

    #[test]
    fn file_mime_json() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/data.json", OpenOptions::write()).unwrap();
        fs.write_file(h, b"{\"a\":1}").unwrap();
        fs.close(h);
        let (status, out) = run_util(util_file, &["file", "-i", "/data.json"], &mut fs, None);
        assert_eq!(status, 0);
        assert!(out.stdout_str().contains("application/json"));
    }

    // -------------------------------------------------------------------
    // file with wasm magic \x00asm
    // -------------------------------------------------------------------

    #[test]
    fn file_wasm_magic() {
        let mut fs = MemoryFs::new();
        let mut wasm = vec![0x00u8, b'a', b's', b'm'];
        wasm.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]); // version
        let h = fs.open("/module.wasm", OpenOptions::write()).unwrap();
        fs.write_file(h, &wasm).unwrap();
        fs.close(h);
        let (status, out) = run_util(util_file, &["file", "/module.wasm"], &mut fs, None);
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(
            s.contains("WebAssembly") || s.contains("wasm"),
            "expected wasm detection, got: {s}"
        );
    }

    // -------------------------------------------------------------------
    // file with ELF magic \x7fELF
    // -------------------------------------------------------------------

    #[test]
    fn file_elf_magic() {
        let mut fs = MemoryFs::new();
        let mut elf = vec![0x7Fu8, b'E', b'L', b'F'];
        elf.extend_from_slice(&[0; 20]);
        let h = fs.open("/binary.elf", OpenOptions::write()).unwrap();
        fs.write_file(h, &elf).unwrap();
        fs.close(h);
        let (status, out) = run_util(util_file, &["file", "/binary.elf"], &mut fs, None);
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("ELF"), "expected ELF detection, got: {s}");
    }

    // -------------------------------------------------------------------
    // file with PDF magic %PDF
    // -------------------------------------------------------------------

    #[test]
    fn file_pdf_magic() {
        let mut fs = MemoryFs::new();
        let mut pdf = b"%PDF-1.4 ".to_vec();
        pdf.extend_from_slice(&[0; 20]);
        let h = fs.open("/doc.pdf", OpenOptions::write()).unwrap();
        fs.write_file(h, &pdf).unwrap();
        fs.close(h);
        let (status, out) = run_util(util_file, &["file", "/doc.pdf"], &mut fs, None);
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("PDF"), "expected PDF detection, got: {s}");
    }

    // -------------------------------------------------------------------
    // file -i  MIME for wasm and ELF
    // -------------------------------------------------------------------

    #[test]
    fn file_mime_wasm() {
        let mut fs = MemoryFs::new();
        let mut wasm = vec![0x00u8, b'a', b's', b'm'];
        wasm.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
        let h = fs.open("/mod.wasm", OpenOptions::write()).unwrap();
        fs.write_file(h, &wasm).unwrap();
        fs.close(h);
        let (status, out) = run_util(util_file, &["file", "-i", "/mod.wasm"], &mut fs, None);
        assert_eq!(status, 0);
        assert!(out.stdout_str().contains("application/wasm"));
    }

    #[test]
    fn file_mime_elf() {
        let mut fs = MemoryFs::new();
        let mut elf = vec![0x7Fu8, b'E', b'L', b'F'];
        elf.extend_from_slice(&[0; 20]);
        let h = fs.open("/bin.elf", OpenOptions::write()).unwrap();
        fs.write_file(h, &elf).unwrap();
        fs.close(h);
        let (status, out) = run_util(util_file, &["file", "-i", "/bin.elf"], &mut fs, None);
        assert_eq!(status, 0);
        assert!(out.stdout_str().contains("application/x-executable"));
    }

    #[test]
    fn file_mime_pdf() {
        let mut fs = MemoryFs::new();
        let mut pdf = b"%PDF-1.7 ".to_vec();
        pdf.extend_from_slice(&[0; 20]);
        let h = fs.open("/doc.pdf", OpenOptions::write()).unwrap();
        fs.write_file(h, &pdf).unwrap();
        fs.close(h);
        let (status, out) = run_util(util_file, &["file", "-i", "/doc.pdf"], &mut fs, None);
        assert_eq!(status, 0);
        assert!(out.stdout_str().contains("application/pdf"));
    }
}
