//! Shared helper functions used across utility modules.

use std::io::Read;

use wasmsh_fs::{BackendFs, FsError, OpenOptions, Vfs};

use crate::{UtilContext, UtilOutput};

pub(crate) fn resolve_path(cwd: &str, path: &str) -> String {
    if path.starts_with('/') {
        wasmsh_fs::normalize_path(path)
    } else {
        wasmsh_fs::normalize_path(&format!("{cwd}/{path}"))
    }
}

pub(crate) fn emit_error(
    output: &mut dyn UtilOutput,
    cmd: &str,
    path: &str,
    err: &dyn std::fmt::Display,
) {
    let msg = format!("{cmd}: {path}: {err}\n");
    output.stderr(msg.as_bytes());
}

pub(crate) fn require_args(argv: &[&str], min: usize, output: &mut dyn UtilOutput) -> bool {
    if argv.len() < min {
        let msg = format!("{}: missing operand\n", argv[0]);
        output.stderr(msg.as_bytes());
        false
    } else {
        true
    }
}

pub(crate) fn copy_file_contents(fs: &mut BackendFs, src: &str, dst: &str) -> Result<(), String> {
    let h = fs
        .open(src, OpenOptions::read())
        .map_err(|e| e.to_string())?;
    let data = match fs.read_file(h) {
        Ok(d) => {
            fs.close(h);
            d
        }
        Err(e) => {
            fs.close(h);
            return Err(e.to_string());
        }
    };
    let wh = fs
        .open(dst, OpenOptions::write())
        .map_err(|e| e.to_string())?;
    if let Err(e) = fs.write_file(wh, &data) {
        fs.close(wh);
        return Err(e.to_string());
    }
    fs.close(wh);
    Ok(())
}

pub(crate) fn read_text(fs: &mut BackendFs, path: &str) -> Result<String, FsError> {
    let h = fs.open(path, OpenOptions::read())?;
    let data = match fs.read_file(h) {
        Ok(d) => {
            fs.close(h);
            d
        }
        Err(e) => {
            fs.close(h);
            return Err(e);
        }
    };
    String::from_utf8(data).map_err(|_| FsError::Io("invalid utf-8".into()))
}

/// Open an input reader from either the first file argument or the current stdin source.
pub(crate) fn open_input_reader<'a>(
    ctx: &mut UtilContext<'a>,
    file_args: &[&str],
    cmd: &str,
) -> Result<Option<Box<dyn Read + 'a>>, i32> {
    if !file_args.is_empty() {
        let full = resolve_path(ctx.cwd, file_args[0]);
        return open_reader_for_path(ctx, &full, file_args[0], cmd).map(Some);
    }
    Ok(ctx
        .stdin
        .take()
        .map(|stdin| Box::new(stdin) as Box<dyn Read + 'a>))
}

/// Open a streaming reader for a resolved path.
pub(crate) fn open_reader_for_path<'a>(
    ctx: &mut UtilContext<'a>,
    full_path: &str,
    display_name: &str,
    cmd: &str,
) -> Result<Box<dyn Read + 'a>, i32> {
    match ctx.fs.open(full_path, OpenOptions::read()) {
        Ok(handle) => {
            let reader = match ctx.fs.stream_file(handle) {
                Ok(reader) => reader,
                Err(err) => {
                    ctx.fs.close(handle);
                    emit_error(ctx.output, cmd, display_name, &err);
                    return Err(1);
                }
            };
            ctx.fs.close(handle);
            Ok(reader)
        }
        Err(err) => {
            emit_error(ctx.output, cmd, display_name, &err);
            Err(1)
        }
    }
}

fn read_reader_to_bytes(
    mut reader: Box<dyn Read + '_>,
    output: &mut dyn UtilOutput,
    cmd: &str,
) -> Result<Vec<u8>, i32> {
    let mut data = Vec::new();
    match reader.read_to_end(&mut data) {
        Ok(_) => Ok(data),
        Err(err) => {
            let msg = format!("{cmd}: stdin read error: {err}\n");
            output.stderr(msg.as_bytes());
            Err(1)
        }
    }
}

fn read_reader_to_string(
    reader: Box<dyn Read + '_>,
    output: &mut dyn UtilOutput,
    cmd: &str,
) -> Result<String, i32> {
    read_reader_to_bytes(reader, output, cmd).map(|data| String::from_utf8_lossy(&data).to_string())
}

pub(crate) fn read_next_line_from_reader(
    reader: &mut dyn Read,
    pending: &mut Vec<u8>,
    output: &mut dyn UtilOutput,
    cmd: &str,
) -> Result<Option<(String, bool)>, i32> {
    loop {
        if let Some(pos) = pending.iter().position(|&b| b == b'\n') {
            let mut line = pending.drain(..=pos).collect::<Vec<u8>>();
            let _ = line.pop();
            return Ok(Some((String::from_utf8_lossy(&line).to_string(), true)));
        }

        let mut buffer = [0u8; 4096];
        match reader.read(&mut buffer) {
            Ok(0) => {
                if pending.is_empty() {
                    return Ok(None);
                }
                let line = std::mem::take(pending);
                return Ok(Some((String::from_utf8_lossy(&line).to_string(), false)));
            }
            Ok(read) => pending.extend_from_slice(&buffer[..read]),
            Err(err) => {
                let msg = format!("{cmd}: stdin read error: {err}\n");
                output.stderr(msg.as_bytes());
                return Err(1);
            }
        }
    }
}

pub(crate) fn collect_input_bytes(
    ctx: &mut UtilContext<'_>,
    file_args: &[&str],
    cmd: &str,
) -> Result<Vec<u8>, i32> {
    let Some(reader) = open_input_reader(ctx, file_args, cmd)? else {
        return Ok(Vec::new());
    };
    read_reader_to_bytes(reader, ctx.output, cmd)
}

pub(crate) fn collect_input_text(
    ctx: &mut UtilContext<'_>,
    file_args: &[&str],
    cmd: &str,
) -> Result<String, i32> {
    let Some(reader) = open_input_reader(ctx, file_args, cmd)? else {
        return Ok(String::new());
    };
    read_reader_to_string(reader, ctx.output, cmd)
}

pub(crate) fn collect_input_lines(
    ctx: &mut UtilContext<'_>,
    file_args: &[&str],
    cmd: &str,
) -> Result<Vec<String>, i32> {
    let mut lines = Vec::new();
    stream_input_lines(ctx, file_args, cmd, |line, _had_newline, _ctx| {
        lines.push(line.to_string());
        Ok(())
    })?;
    Ok(lines)
}

pub(crate) fn collect_path_text(
    ctx: &mut UtilContext<'_>,
    full_path: &str,
    display_name: &str,
    cmd: &str,
) -> Result<String, i32> {
    let reader = open_reader_for_path(ctx, full_path, display_name, cmd)?;
    read_reader_to_string(reader, ctx.output, cmd)
}

pub(crate) fn stream_input_chunks(
    ctx: &mut UtilContext<'_>,
    file_args: &[&str],
    cmd: &str,
    mut f: impl FnMut(&[u8], &mut UtilContext<'_>) -> Result<(), i32>,
) -> Result<(), i32> {
    let Some(mut reader) = open_input_reader(ctx, file_args, cmd)? else {
        return Ok(());
    };
    let mut buffer = [0u8; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => f(&buffer[..read], ctx)?,
            Err(err) => {
                let msg = format!("{cmd}: stdin read error: {err}\n");
                ctx.output.stderr(msg.as_bytes());
                return Err(1);
            }
        }
    }
}

pub(crate) fn stream_input_lines(
    ctx: &mut UtilContext<'_>,
    file_args: &[&str],
    cmd: &str,
    mut f: impl FnMut(&str, bool, &mut UtilContext<'_>) -> Result<(), i32>,
) -> Result<(), i32> {
    let Some(mut reader) = open_input_reader(ctx, file_args, cmd)? else {
        return Ok(());
    };
    let mut pending = Vec::new();
    while let Some((line, had_newline)) =
        read_next_line_from_reader(reader.as_mut(), &mut pending, ctx.output, cmd)?
    {
        f(&line, had_newline, ctx)?;
    }
    Ok(())
}

pub(crate) fn stream_input_whitespace_tokens(
    ctx: &mut UtilContext<'_>,
    file_args: &[&str],
    cmd: &str,
    mut f: impl FnMut(&str, &mut UtilContext<'_>) -> Result<(), i32>,
) -> Result<(), i32> {
    let mut pending = String::new();
    stream_input_chunks(ctx, file_args, cmd, |chunk, ctx| {
        pending.push_str(&String::from_utf8_lossy(chunk));
        let mut split_at = 0usize;
        for (idx, ch) in pending.char_indices() {
            if ch.is_whitespace() {
                split_at = idx + ch.len_utf8();
            }
        }
        if split_at == 0 {
            return Ok(());
        }
        let completed = pending[..split_at].to_string();
        pending.drain(..split_at);
        for token in completed.split_whitespace() {
            f(token, ctx)?;
        }
        Ok(())
    })?;
    if !pending.trim().is_empty() {
        for token in pending.split_whitespace() {
            f(token, ctx)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// CRC-32 (ISO 3309, polynomial 0xEDB88320) — shared by cksum, gzip, etc.
// ---------------------------------------------------------------------------

/// Build CRC-32 lookup table at compile time.
pub(crate) const fn build_crc32_table() -> [u32; 256] {
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

pub(crate) const CRC32_TABLE: [u32; 256] = build_crc32_table();

pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        let index = ((crc ^ u32::from(byte)) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC32_TABLE[index];
    }
    !crc
}

pub(crate) fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        let index = ((crc ^ u32::from(byte)) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC32_TABLE[index];
    }
    crc
}

/// Basic grep pattern matching with `^` and `$` anchor support.
pub(crate) fn grep_matches(line: &str, pattern: &str, ignore_case: bool) -> bool {
    let (l, p) = if ignore_case {
        (line.to_lowercase(), pattern.to_lowercase())
    } else {
        (line.to_string(), pattern.to_string())
    };
    if let Some(rest) = p.strip_prefix('^') {
        if let Some(mid) = rest.strip_suffix('$') {
            l == mid
        } else {
            l.starts_with(rest)
        }
    } else if let Some(rest) = p.strip_suffix('$') {
        l.ends_with(rest)
    } else {
        l.contains(&p)
    }
}

/// Encode bytes as lowercase hexadecimal string.
///
/// Thin wrapper over the `hex` crate so callers have a single,
/// crate-local entry point that stays stable across dependency
/// upgrades.  See ADR-0023.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

pub(crate) struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0xDEAD_BEEF_CAFE_BABE
            } else {
                seed
            },
        }
    }

    pub(crate) fn next(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }
}

pub(crate) fn child_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

/// Generic hashsum utility: read files (or stdin), hash, format "HASH  path\n".
///
/// Deduplicates the identical boilerplate shared by md5sum, sha1sum, sha256sum, sha512sum.
pub(crate) fn hashsum_util(
    ctx: &mut UtilContext<'_>,
    argv: &[&str],
    cmd_name: &str,
    hash_fn: fn(&[u8]) -> String,
) -> i32 {
    let mut check_mode = false;
    let mut file_args: Vec<&str> = Vec::new();
    for arg in &argv[1..] {
        match *arg {
            "-c" | "--check" => check_mode = true,
            "-b" | "--binary" | "--tag" => {} // accept, no-op
            _ => file_args.push(arg),
        }
    }
    if check_mode {
        return hashsum_check(ctx, &file_args, cmd_name, hash_fn);
    }
    if file_args.is_empty() {
        let data = collect_input_bytes(ctx, &[], cmd_name).unwrap_or_default();
        let hash = hash_fn(&data);
        let line = format!("{hash}  -\n");
        ctx.output.stdout(line.as_bytes());
        return 0;
    }
    let mut status = 0;
    for path in &file_args {
        match read_file_bytes(ctx, path, cmd_name) {
            Ok(data) => {
                let hash = hash_fn(&data);
                let line = format!("{hash}  {path}\n");
                ctx.output.stdout(line.as_bytes());
            }
            Err(s) => status = s,
        }
    }
    status
}

fn hashsum_check(
    ctx: &mut UtilContext<'_>,
    file_args: &[&str],
    cmd_name: &str,
    hash_fn: fn(&[u8]) -> String,
) -> i32 {
    let text = if file_args.is_empty() {
        collect_input_text(ctx, &[], cmd_name).unwrap_or_default()
    } else {
        let full = resolve_path(ctx.cwd, file_args[0]);
        match collect_path_text(ctx, &full, file_args[0], cmd_name) {
            Ok(text) => text,
            Err(s) => return s,
        }
    };
    let mut failures = 0u32;
    for line in text.lines() {
        let (expected, filename) = if let Some((h, f)) = line.split_once("  ") {
            (h.trim(), f.trim())
        } else if let Some((h, f)) = line.split_once(' ') {
            (h.trim(), f.trim())
        } else {
            continue;
        };
        if filename.is_empty() {
            continue;
        }
        if let Ok(data) = read_file_bytes(ctx, filename, cmd_name) {
            let actual = hash_fn(&data);
            if actual == expected {
                let msg = format!("{filename}: OK\n");
                ctx.output.stdout(msg.as_bytes());
            } else {
                let msg = format!("{filename}: FAILED\n");
                ctx.output.stdout(msg.as_bytes());
                failures += 1;
            }
        } else {
            let msg = format!("{filename}: FAILED open or read\n");
            ctx.output.stdout(msg.as_bytes());
            failures += 1;
        }
    }
    if failures > 0 {
        let msg = format!("{cmd_name}: WARNING: {failures} computed checksum(s) did NOT match\n");
        ctx.output.stderr(msg.as_bytes());
        return 1;
    }
    0
}

/// Read a file from the VFS by path, returning its bytes.
///
/// Handles open/read/close and error emission. Returns `Err(1)` on failure.
pub(crate) fn read_file_bytes(
    ctx: &mut UtilContext<'_>,
    path: &str,
    cmd: &str,
) -> Result<Vec<u8>, i32> {
    let full = resolve_path(ctx.cwd, path);
    match ctx.fs.open(&full, OpenOptions::read()) {
        Ok(h) => match ctx.fs.read_file(h) {
            Ok(data) => {
                ctx.fs.close(h);
                Ok(data)
            }
            Err(e) => {
                ctx.fs.close(h);
                emit_error(ctx.output, cmd, path, &e);
                Err(1)
            }
        },
        Err(e) => {
            emit_error(ctx.output, cmd, path, &e);
            Err(1)
        }
    }
}

/// Read a file from the VFS by an already-resolved absolute path.
///
/// Like [`read_file_bytes`] but skips path resolution (caller already has the full path).
pub(crate) fn read_file_bytes_abs(
    ctx: &mut UtilContext<'_>,
    full_path: &str,
    display_name: &str,
    cmd: &str,
) -> Result<Vec<u8>, i32> {
    match ctx.fs.open(full_path, OpenOptions::read()) {
        Ok(h) => match ctx.fs.read_file(h) {
            Ok(data) => {
                ctx.fs.close(h);
                Ok(data)
            }
            Err(e) => {
                ctx.fs.close(h);
                emit_error(ctx.output, cmd, display_name, &e);
                Err(1)
            }
        },
        Err(e) => {
            emit_error(ctx.output, cmd, display_name, &e);
            Err(1)
        }
    }
}

/// Helper: write data to a VFS path, emitting errors on failure.
pub(crate) fn write_file_bytes(
    ctx: &mut UtilContext<'_>,
    cmd: &str,
    path: &str,
    data: &[u8],
) -> i32 {
    match ctx.fs.open(path, OpenOptions::write()) {
        Ok(h) => {
            if let Err(e) = ctx.fs.write_file(h, data) {
                ctx.fs.close(h);
                emit_error(ctx.output, cmd, path, &e);
                return 1;
            }
            ctx.fs.close(h);
            0
        }
        Err(e) => {
            emit_error(ctx.output, cmd, path, &e);
            1
        }
    }
}

/// Read data from a file arg or stdin, returning bytes.
///
/// Convenience for utilities that accept either a file path or piped stdin.
pub(crate) fn read_input_bytes(
    ctx: &mut UtilContext<'_>,
    file_args: &[&str],
    cmd: &str,
) -> Result<Vec<u8>, i32> {
    collect_input_bytes(ctx, file_args, cmd)
}

/// Simple glob matching: `*` matches any sequence, `?` matches one char.
pub(crate) fn simple_glob_match(pattern: &str, name: &str) -> bool {
    let p = pattern.as_bytes();
    let n = name.as_bytes();
    let mut pi = 0;
    let mut ni = 0;
    let mut star_p = usize::MAX;
    let mut star_n = 0;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star_p = pi;
            star_n = ni;
            pi += 1;
        } else if star_p != usize::MAX {
            pi = star_p + 1;
            star_n += 1;
            ni = star_n;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}
