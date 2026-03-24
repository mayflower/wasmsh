//! Binary utilities: xxd, dd, strings, split.

use wasmsh_fs::{OpenOptions, Vfs};

use crate::helpers::{emit_error, resolve_path};
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

pub(crate) fn util_xxd(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut reverse = false;
    let mut plain = false;
    let mut c_include = false;
    let mut limit: Option<usize> = None;
    let mut skip: usize = 0;
    let mut cols: usize = 16;

    while let Some(arg) = args.first() {
        match *arg {
            "-r" => {
                reverse = true;
                args = &args[1..];
            }
            "-p" => {
                plain = true;
                args = &args[1..];
            }
            "-i" => {
                c_include = true;
                args = &args[1..];
            }
            "-l" if args.len() > 1 => {
                limit = args[1].parse().ok();
                args = &args[2..];
            }
            "-s" if args.len() > 1 => {
                skip = args[1].parse().unwrap_or(0);
                args = &args[2..];
            }
            "-c" if args.len() > 1 => {
                cols = args[1].parse().unwrap_or(16);
                if cols == 0 {
                    cols = 16;
                }
                args = &args[2..];
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                let msg = format!("xxd: unknown option '{arg}'\n");
                ctx.output.stderr(msg.as_bytes());
                return 1;
            }
            _ => break,
        }
    }

    if reverse {
        return xxd_reverse(ctx, args);
    }

    // Get input data
    let data = if !args.is_empty() {
        let full = resolve_path(ctx.cwd, args[0]);
        match ctx.fs.open(&full, OpenOptions::read()) {
            Ok(h) => {
                let result = ctx.fs.read_file(h).unwrap_or_default();
                ctx.fs.close(h);
                result
            }
            Err(e) => {
                emit_error(ctx.output, "xxd", args[0], &e);
                return 1;
            }
        }
    } else if let Some(d) = ctx.stdin {
        d.to_vec()
    } else {
        Vec::new()
    };

    // Apply skip and limit
    let start = skip.min(data.len());
    let data = &data[start..];
    let data = match limit {
        Some(l) => &data[..l.min(data.len())],
        None => data,
    };

    if plain {
        xxd_plain(ctx, data);
    } else if c_include {
        xxd_c_include(ctx, data, args.first().copied().unwrap_or("stdin"));
    } else {
        xxd_default(ctx, data, start, cols);
    }

    0
}

fn xxd_default(ctx: &mut UtilContext<'_>, data: &[u8], base_offset: usize, cols: usize) {
    use std::fmt::Write;
    for (chunk_idx, chunk) in data.chunks(cols).enumerate() {
        let offset = base_offset + chunk_idx * cols;
        let mut line = format!("{offset:08x}:");

        // Hex bytes in pairs
        for (i, &b) in chunk.iter().enumerate() {
            if i % 2 == 0 {
                line.push(' ');
            }
            let _ = write!(line, "{b:02x}");
        }

        // Pad remaining hex space
        let remaining = cols - chunk.len();
        for i in 0..remaining {
            if (chunk.len() + i) % 2 == 0 {
                line.push(' ');
            }
            line.push_str("  ");
        }

        line.push_str("  ");

        // ASCII representation
        for &b in chunk {
            if b.is_ascii_graphic() || b == b' ' {
                line.push(b as char);
            } else {
                line.push('.');
            }
        }

        line.push('\n');
        ctx.output.stdout(line.as_bytes());
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

fn xxd_reverse(ctx: &mut UtilContext<'_>, file_args: &[&str]) -> i32 {
    let text = if !file_args.is_empty() {
        let full = resolve_path(ctx.cwd, file_args[0]);
        match crate::helpers::read_text(ctx.fs, &full) {
            Ok(t) => t,
            Err(e) => {
                emit_error(ctx.output, "xxd", file_args[0], &e);
                return 1;
            }
        }
    } else if let Some(d) = ctx.stdin {
        String::from_utf8_lossy(d).to_string()
    } else {
        return 0;
    };

    let mut result = Vec::new();
    for line in text.lines() {
        // Strip offset prefix (everything before the first colon, if present)
        let hex_part = if let Some(pos) = line.find(':') {
            &line[pos + 1..]
        } else {
            line
        };
        // Strip ASCII portion (after two spaces followed by printable chars at end)
        let hex_only = if let Some(pos) = hex_part.rfind("  ") {
            &hex_part[..pos]
        } else {
            hex_part
        };
        // Parse hex characters, ignoring spaces
        let clean: String = hex_only.chars().filter(char::is_ascii_hexdigit).collect();
        for pair in clean.as_bytes().chunks(2) {
            if pair.len() == 2 {
                let s = std::str::from_utf8(pair).unwrap_or("00");
                if let Ok(b) = u8::from_str_radix(s, 16) {
                    result.push(b);
                }
            }
        }
    }

    ctx.output.stdout(&result);
    0
}

// ---------------------------------------------------------------------------
// dd — data copy/convert
// ---------------------------------------------------------------------------

pub(crate) fn util_dd(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut input_file: Option<&str> = None;
    let mut output_file: Option<&str> = None;
    let mut block_size: u64 = 512;
    let mut count: Option<u64> = None;
    let mut skip_blocks: u64 = 0;
    let mut seek_blocks: u64 = 0;
    let mut conv_ucase = false;
    let mut conv_lcase = false;
    let mut conv_notrunc = false;

    for arg in &argv[1..] {
        if let Some(val) = arg.strip_prefix("if=") {
            input_file = Some(val);
        } else if let Some(val) = arg.strip_prefix("of=") {
            output_file = Some(val);
        } else if let Some(val) = arg.strip_prefix("bs=") {
            block_size = parse_size(val).unwrap_or(512);
        } else if let Some(val) = arg.strip_prefix("count=") {
            count = val.parse().ok();
        } else if let Some(val) = arg.strip_prefix("skip=") {
            skip_blocks = val.parse().unwrap_or(0);
        } else if let Some(val) = arg.strip_prefix("seek=") {
            seek_blocks = val.parse().unwrap_or(0);
        } else if let Some(val) = arg.strip_prefix("conv=") {
            for opt in val.split(',') {
                match opt {
                    "ucase" => conv_ucase = true,
                    "lcase" => conv_lcase = true,
                    "notrunc" => conv_notrunc = true,
                    _ => {
                        let msg = format!("dd: unknown conv option '{opt}'\n");
                        ctx.output.stderr(msg.as_bytes());
                        return 1;
                    }
                }
            }
        }
    }

    // Read input
    let input_data = if let Some(path) = input_file {
        let full = resolve_path(ctx.cwd, path);
        match ctx.fs.open(&full, OpenOptions::read()) {
            Ok(h) => {
                let data = ctx.fs.read_file(h).unwrap_or_default();
                ctx.fs.close(h);
                data
            }
            Err(e) => {
                emit_error(ctx.output, "dd", path, &e);
                return 1;
            }
        }
    } else if let Some(d) = ctx.stdin {
        d.to_vec()
    } else {
        Vec::new()
    };

    // Skip input blocks
    let skip_bytes = skip_blocks * block_size;
    let input_data = if skip_bytes as usize >= input_data.len() {
        &[]
    } else {
        &input_data[skip_bytes as usize..]
    };

    // Read blocks
    let bs = block_size as usize;
    let mut blocks_in_full = 0u64;
    let mut blocks_in_partial = 0u64;
    let mut output_data = Vec::new();

    // Seek: prepend zero bytes for seek blocks
    let seek_bytes = (seek_blocks * block_size) as usize;
    output_data.resize(seek_bytes, 0u8);

    let max_blocks = count.unwrap_or(u64::MAX);
    let mut offset = 0;
    let mut block_count = 0u64;
    while offset < input_data.len() && block_count < max_blocks {
        let end = (offset + bs).min(input_data.len());
        let chunk = &input_data[offset..end];
        if chunk.len() == bs {
            blocks_in_full += 1;
        } else {
            blocks_in_partial += 1;
        }
        output_data.extend_from_slice(chunk);
        offset += bs;
        block_count += 1;
    }

    // Apply conversions
    if conv_ucase {
        for b in &mut output_data {
            if b.is_ascii_lowercase() {
                *b = b.to_ascii_uppercase();
            }
        }
    }
    if conv_lcase {
        for b in &mut output_data {
            if b.is_ascii_uppercase() {
                *b = b.to_ascii_lowercase();
            }
        }
    }

    let total_bytes = output_data.len() - seek_bytes;

    // Compute output block stats (same logic applied to output side)
    let blocks_out_full = blocks_in_full;
    let blocks_out_partial = blocks_in_partial;

    // Write output
    if let Some(path) = output_file {
        let full = resolve_path(ctx.cwd, path);
        let opts = if conv_notrunc {
            OpenOptions::append()
        } else {
            OpenOptions::write()
        };
        match ctx.fs.open(&full, opts) {
            Ok(h) => {
                if let Err(e) = ctx.fs.write_file(h, &output_data) {
                    ctx.fs.close(h);
                    emit_error(ctx.output, "dd", path, &e);
                    return 1;
                }
                ctx.fs.close(h);
            }
            Err(e) => {
                emit_error(ctx.output, "dd", path, &e);
                return 1;
            }
        }
    } else {
        ctx.output.stdout(&output_data[seek_bytes..]);
    }

    // Stats to stderr
    let stats = format!(
        "{blocks_in_full}+{blocks_in_partial} records in\n\
         {blocks_out_full}+{blocks_out_partial} records out\n\
         {total_bytes} bytes transferred\n"
    );
    ctx.output.stderr(stats.as_bytes());

    0
}

// ---------------------------------------------------------------------------
// strings — extract printable strings
// ---------------------------------------------------------------------------

pub(crate) fn util_strings(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut min_len: usize = 4;

    while let Some(arg) = args.first() {
        if (*arg == "-n" || *arg == "--bytes") && args.len() > 1 {
            min_len = args[1].parse().unwrap_or(4);
            if min_len == 0 {
                min_len = 1;
            }
            args = &args[2..];
        } else if let Some(rest) = arg.strip_prefix("-n") {
            min_len = rest.parse().unwrap_or(4);
            if min_len == 0 {
                min_len = 1;
            }
            args = &args[1..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            args = &args[1..];
        } else {
            break;
        }
    }

    // Get input data (binary)
    let data = if !args.is_empty() {
        let full = resolve_path(ctx.cwd, args[0]);
        match ctx.fs.open(&full, OpenOptions::read()) {
            Ok(h) => {
                let result = ctx.fs.read_file(h).unwrap_or_default();
                ctx.fs.close(h);
                result
            }
            Err(e) => {
                emit_error(ctx.output, "strings", args[0], &e);
                return 1;
            }
        }
    } else if let Some(d) = ctx.stdin {
        d.to_vec()
    } else {
        Vec::new()
    };

    let mut current = String::new();
    for &b in &data {
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
    // Flush remaining
    if current.len() >= min_len {
        ctx.output.stdout(current.as_bytes());
        ctx.output.stdout(b"\n");
    }

    0
}

// ---------------------------------------------------------------------------
// split — split file into pieces
// ---------------------------------------------------------------------------

pub(crate) fn util_split(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut lines: Option<usize> = None;
    let mut byte_size: Option<u64> = None;
    let mut chunks: Option<usize> = None;
    let mut numeric_suffix = false;

    while let Some(arg) = args.first() {
        match *arg {
            "-l" if args.len() > 1 => {
                lines = args[1].parse().ok();
                args = &args[2..];
            }
            "-b" if args.len() > 1 => {
                byte_size = parse_size(args[1]);
                args = &args[2..];
            }
            "-n" if args.len() > 1 => {
                chunks = args[1].parse().ok();
                args = &args[2..];
            }
            "-d" => {
                numeric_suffix = true;
                args = &args[1..];
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                let msg = format!("split: unknown option '{arg}'\n");
                ctx.output.stderr(msg.as_bytes());
                return 1;
            }
            _ => break,
        }
    }

    // Remaining: [FILE [PREFIX]]
    let (input_data, prefix) = if !args.is_empty() && args[0] != "-" {
        let full = resolve_path(ctx.cwd, args[0]);
        let data = match ctx.fs.open(&full, OpenOptions::read()) {
            Ok(h) => {
                let d = ctx.fs.read_file(h).unwrap_or_default();
                ctx.fs.close(h);
                d
            }
            Err(e) => {
                emit_error(ctx.output, "split", args[0], &e);
                return 1;
            }
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

    let pieces: Vec<Vec<u8>> = if let Some(n) = chunks {
        // Split into N equal chunks
        if n == 0 {
            ctx.output.stderr(b"split: invalid number of chunks\n");
            return 1;
        }
        let chunk_size = input_data.len().div_ceil(n);
        input_data
            .chunks(chunk_size.max(1))
            .map(<[u8]>::to_vec)
            .collect()
    } else if let Some(bs) = byte_size {
        // Split by byte size
        let bs = bs as usize;
        if bs == 0 {
            ctx.output.stderr(b"split: invalid byte size\n");
            return 1;
        }
        input_data.chunks(bs).map(<[u8]>::to_vec).collect()
    } else {
        // Split by lines
        let line_count = lines.unwrap_or(1000);
        if line_count == 0 {
            ctx.output.stderr(b"split: invalid line count\n");
            return 1;
        }
        let text = String::from_utf8_lossy(&input_data);
        let all_lines: Vec<&str> = text.lines().collect();
        all_lines
            .chunks(line_count)
            .map(|chunk| {
                let mut buf = String::new();
                for (i, line) in chunk.iter().enumerate() {
                    buf.push_str(line);
                    if i + 1 < chunk.len() || text.ends_with('\n') {
                        buf.push('\n');
                    }
                }
                buf.into_bytes()
            })
            .collect()
    };

    // Write pieces
    for (i, piece) in pieces.iter().enumerate() {
        let suffix = if numeric_suffix {
            format!("{i:02}")
        } else {
            suffix_alpha(i)
        };
        let name = format!("{prefix}{suffix}");
        let full = resolve_path(ctx.cwd, &name);
        match ctx.fs.open(&full, OpenOptions::write()) {
            Ok(h) => {
                if let Err(e) = ctx.fs.write_file(h, piece) {
                    ctx.fs.close(h);
                    emit_error(ctx.output, "split", &name, &e);
                    return 1;
                }
                ctx.fs.close(h);
            }
            Err(e) => {
                emit_error(ctx.output, "split", &name, &e);
                return 1;
            }
        }
    }

    0
}

/// Generate alphabetic suffix: 0 -> "aa", 1 -> "ab", ..., 25 -> "az", 26 -> "ba", ...
fn suffix_alpha(n: usize) -> String {
    let first = (n / 26) as u8 + b'a';
    let second = (n % 26) as u8 + b'a';
    let mut s = String::with_capacity(2);
    s.push(first as char);
    s.push(second as char);
    s
}

// ---------------------------------------------------------------------------
// file — detect file type
// ---------------------------------------------------------------------------

pub(crate) fn util_file(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut brief = false;
    let mut mime_type = false;

    while let Some(arg) = args.first() {
        match *arg {
            "-b" | "--brief" => {
                brief = true;
                args = &args[1..];
            }
            "-i" | "--mime-type" => {
                mime_type = true;
                args = &args[1..];
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                // Combined short flags
                for ch in arg[1..].chars() {
                    match ch {
                        'b' => brief = true,
                        'i' => mime_type = true,
                        _ => {
                            let msg = format!("file: unknown option '-{ch}'\n");
                            ctx.output.stderr(msg.as_bytes());
                            return 1;
                        }
                    }
                }
                args = &args[1..];
            }
            _ => break,
        }
    }

    if args.is_empty() {
        ctx.output.stderr(b"file: missing operand\n");
        return 1;
    }

    let mut status = 0;
    for path in args {
        let full = resolve_path(ctx.cwd, path);

        // Check if it's a directory
        match ctx.fs.stat(&full) {
            Ok(meta) if meta.is_dir => {
                let desc = if mime_type {
                    "inode/directory"
                } else {
                    "directory"
                };
                if brief {
                    let line = format!("{desc}\n");
                    ctx.output.stdout(line.as_bytes());
                } else {
                    let line = format!("{path}: {desc}\n");
                    ctx.output.stdout(line.as_bytes());
                }
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                emit_error(ctx.output, "file", path, &e);
                status = 1;
                continue;
            }
        }

        // Read file content
        let data = match ctx.fs.open(&full, OpenOptions::read()) {
            Ok(h) => {
                let d = ctx.fs.read_file(h).unwrap_or_default();
                ctx.fs.close(h);
                d
            }
            Err(e) => {
                emit_error(ctx.output, "file", path, &e);
                status = 1;
                continue;
            }
        };

        let desc = if mime_type {
            detect_mime_type(&data, path)
        } else {
            detect_file_type(&data, path)
        };

        if brief {
            let line = format!("{desc}\n");
            ctx.output.stdout(line.as_bytes());
        } else {
            let line = format!("{path}: {desc}\n");
            ctx.output.stdout(line.as_bytes());
        }
    }

    status
}

fn detect_file_type(data: &[u8], path: &str) -> String {
    // Check magic bytes first
    if data.len() >= 4 && data[0] == 0x89 && &data[1..4] == b"PNG" {
        return "PNG image data".to_string();
    }
    if data.len() >= 6 && (&data[..6] == b"GIF87a" || &data[..6] == b"GIF89a") {
        return "GIF image data".to_string();
    }
    if data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF {
        return "JPEG image data".to_string();
    }
    if data.len() >= 4 && &data[..4] == b"%PDF" {
        return "PDF document".to_string();
    }
    if data.len() >= 4 && &data[..4] == b"PK\x03\x04" {
        return "Zip archive data".to_string();
    }
    if data.len() >= 2 && data[0] == 0x1F && data[1] == 0x8B {
        return "gzip compressed data".to_string();
    }
    if data.len() >= 4 && data[0] == 0x7F && &data[1..4] == b"ELF" {
        return "ELF executable".to_string();
    }
    if data.len() >= 4 && data[0] == 0x00 && &data[1..4] == b"asm" {
        return "WebAssembly (wasm) binary module".to_string();
    }

    // Check shebang
    if data.len() >= 2 && &data[..2] == b"#!" {
        let end = data
            .iter()
            .position(|&b| b == b'\n')
            .unwrap_or(data.len().min(128));
        let shebang = String::from_utf8_lossy(&data[..end]);
        return format!("script text executable ({shebang})");
    }

    // Check first non-whitespace byte for JSON/XML/HTML
    if let Some(first_nws) = data.iter().find(|b| !b.is_ascii_whitespace()) {
        if *first_nws == b'{' {
            return "JSON text data".to_string();
        }
        if *first_nws == b'<' {
            let text = String::from_utf8_lossy(data);
            let lower = text.to_lowercase();
            if lower.contains("<!doctype") || lower.contains("<html") {
                return "HTML document".to_string();
            }
            if lower.contains("<?xml") {
                return "XML document".to_string();
            }
        }
    }

    // Extension fallback
    if let Some(desc) = extension_type(path) {
        return desc.to_string();
    }

    // Content analysis
    if is_valid_utf8_text(data) {
        if data.is_ascii() {
            return "ASCII text".to_string();
        }
        return "UTF-8 Unicode text".to_string();
    }

    "data".to_string()
}

fn detect_mime_type(data: &[u8], path: &str) -> String {
    // Check magic bytes first
    if data.len() >= 4 && data[0] == 0x89 && &data[1..4] == b"PNG" {
        return "image/png".to_string();
    }
    if data.len() >= 6 && (&data[..6] == b"GIF87a" || &data[..6] == b"GIF89a") {
        return "image/gif".to_string();
    }
    if data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF {
        return "image/jpeg".to_string();
    }
    if data.len() >= 4 && &data[..4] == b"%PDF" {
        return "application/pdf".to_string();
    }
    if data.len() >= 4 && &data[..4] == b"PK\x03\x04" {
        return "application/zip".to_string();
    }
    if data.len() >= 2 && data[0] == 0x1F && data[1] == 0x8B {
        return "application/gzip".to_string();
    }
    if data.len() >= 4 && data[0] == 0x7F && &data[1..4] == b"ELF" {
        return "application/x-executable".to_string();
    }
    if data.len() >= 4 && data[0] == 0x00 && &data[1..4] == b"asm" {
        return "application/wasm".to_string();
    }

    // Check first non-whitespace byte for JSON/XML/HTML
    if let Some(first_nws) = data.iter().find(|b| !b.is_ascii_whitespace()) {
        if *first_nws == b'{' {
            return "application/json".to_string();
        }
        if *first_nws == b'<' {
            let text = String::from_utf8_lossy(data);
            let lower = text.to_lowercase();
            if lower.contains("<!doctype") || lower.contains("<html") {
                return "text/html".to_string();
            }
            if lower.contains("<?xml") {
                return "application/xml".to_string();
            }
        }
    }

    // Extension-based MIME
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

    #[test]
    fn parse_size_values() {
        assert_eq!(parse_size("512"), Some(512));
        assert_eq!(parse_size("1K"), Some(1024));
        assert_eq!(parse_size("2M"), Some(2 * 1024 * 1024));
        assert_eq!(parse_size("1G"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size("abc"), None);
    }
}
