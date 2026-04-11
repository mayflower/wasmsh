//! Archive utilities: tar, gzip, gunzip, zcat.

use wasmsh_fs::Vfs;

use crate::helpers::{
    child_path, crc32, emit_error, read_file_bytes, read_file_bytes_abs, resolve_path,
    write_file_bytes,
};
use crate::UtilContext;

// ---------------------------------------------------------------------------
// gzip / gunzip / zcat
// ---------------------------------------------------------------------------
//
// The gzip file format (RFC 1952) wraps a DEFLATE stream with a small
// header and an 8-byte trailer (CRC-32 + original size).  wasmsh keeps
// the handwritten header/trailer wrapping but delegates the actual
// compression and decompression to `miniz_oxide` — a pure Rust port of
// miniz, used by flate2 itself.  See ADR-0025.

/// Compression level passed to `miniz_oxide`.  Level 6 is the classic
/// zlib default and offers a good ratio/speed tradeoff.  Sandbox use
/// prioritises ratio over throughput because the input sizes are
/// small and the wall-clock budget is bounded by step limits, not by
/// gzip wall-time.
const GZIP_COMPRESSION_LEVEL: u8 = 6;

/// Compress `data` as a gzip file using real DEFLATE compression.
fn gzip_compress(data: &[u8]) -> Vec<u8> {
    // RFC 1952 header: 10 bytes, no optional fields.
    let mut out = vec![
        0x1F, // ID1
        0x8B, // ID2
        0x08, // CM: deflate
        0x00, // FLG: none
        0x00, 0x00, 0x00, 0x00, // MTIME: unset
        0x00, // XFL: none
        0xFF, // OS: unknown
    ];

    // Real DEFLATE compression via miniz_oxide.
    let compressed = miniz_oxide::deflate::compress_to_vec(data, GZIP_COMPRESSION_LEVEL);
    out.extend_from_slice(&compressed);

    // Trailer: CRC-32 of uncompressed data, then ISIZE mod 2^32.
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());

    out
}

/// Parse the gzip header and return the position of the DEFLATE stream start.
fn parse_gzip_header(data: &[u8]) -> Result<usize, String> {
    if data.len() < 10 {
        return Err("invalid gzip: too short".to_string());
    }
    if data[0] != 0x1F || data[1] != 0x8B {
        return Err("invalid gzip magic".to_string());
    }
    if data[2] != 0x08 {
        return Err("unsupported compression method".to_string());
    }

    let flags = data[3];
    let pos = skip_gzip_optional_fields(data, flags)?;
    Ok(pos)
}

/// Skip optional gzip header fields (FEXTRA, FNAME, FCOMMENT, FHCRC).
fn skip_gzip_optional_fields(data: &[u8], flags: u8) -> Result<usize, String> {
    let mut pos = 10;
    if flags & 0x04 != 0 {
        if pos + 2 > data.len() {
            return Err("truncated gzip header".to_string());
        }
        let xlen = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2 + xlen;
    }
    if flags & 0x08 != 0 {
        pos = skip_null_terminated(data, pos);
    }
    if flags & 0x10 != 0 {
        pos = skip_null_terminated(data, pos);
    }
    if flags & 0x02 != 0 {
        pos += 2;
    }
    Ok(pos)
}

/// Skip past a null-terminated string in `data` starting at `pos`.
fn skip_null_terminated(data: &[u8], mut pos: usize) -> usize {
    while pos < data.len() && data[pos] != 0 {
        pos += 1;
    }
    pos + 1
}

/// Decompress a gzip file.  Handles all DEFLATE block types (stored,
/// fixed Huffman, dynamic Huffman) via `miniz_oxide`, which is what
/// real-world gzip streams use — wasmsh's previous implementation
/// only handled stored blocks and therefore could not decompress
/// files produced by upstream `gzip`, `curl | tar -xz`, etc.
fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    let pos = parse_gzip_header(data)?;
    // The trailer is the last 8 bytes: CRC-32 || ISIZE.  Everything in
    // between the header and the trailer is the raw DEFLATE stream.
    if data.len() < pos + 8 {
        return Err("truncated gzip stream".to_string());
    }
    let deflate_end = data.len() - 8;
    let deflate_stream = &data[pos..deflate_end];

    let output = miniz_oxide::inflate::decompress_to_vec(deflate_stream)
        .map_err(|e| format!("inflate failed: {e}"))?;

    // Verify CRC-32 and declared ISIZE against the decoded bytes.
    let expected_crc = u32::from_le_bytes([
        data[deflate_end],
        data[deflate_end + 1],
        data[deflate_end + 2],
        data[deflate_end + 3],
    ]);
    if crc32(&output) != expected_crc {
        return Err("CRC-32 mismatch".to_string());
    }
    let expected_isize = u32::from_le_bytes([
        data[deflate_end + 4],
        data[deflate_end + 5],
        data[deflate_end + 6],
        data[deflate_end + 7],
    ]);
    if (output.len() as u32) != expected_isize {
        return Err("ISIZE mismatch".to_string());
    }

    Ok(output)
}

struct GzipFlags {
    to_stdout: bool,
    decompress: bool,
    keep: bool,
}

fn parse_gzip_flags(ctx: &mut UtilContext<'_>, argv: &[&str]) -> Result<(GzipFlags, usize), i32> {
    let mut args = &argv[1..];
    let mut flags = GzipFlags {
        to_stdout: false,
        decompress: false,
        keep: false,
    };
    let mut consumed = 1;

    while let Some(arg) = args.first() {
        match *arg {
            "-c" => flags.to_stdout = true,
            "-d" => flags.decompress = true,
            "-k" => flags.keep = true,
            _ if arg.starts_with('-') && arg.len() > 1 => {
                for ch in arg[1..].chars() {
                    match ch {
                        'c' => flags.to_stdout = true,
                        'd' => flags.decompress = true,
                        'k' => flags.keep = true,
                        _ => {
                            let msg = format!("gzip: unknown option '-{ch}'\n");
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

fn gzip_decompress_file(
    ctx: &mut UtilContext<'_>,
    path: &str,
    full: &str,
    data: &[u8],
    flags: &GzipFlags,
) -> i32 {
    let decompressed = match gzip_decompress(data) {
        Ok(d) => d,
        Err(e) => {
            emit_error(ctx.output, "gzip", path, &e);
            return 1;
        }
    };
    if flags.to_stdout {
        ctx.output.stdout(&decompressed);
        return 0;
    }
    let out_path = if std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gz"))
    {
        full[..full.len() - 3].to_string()
    } else {
        format!("{full}.out")
    };
    if write_file(ctx, "gzip", &out_path, &decompressed) != 0 {
        return 1;
    }
    remove_original_if_needed(ctx, full, path, flags.keep);
    0
}

fn gzip_compress_file(
    ctx: &mut UtilContext<'_>,
    path: &str,
    full: &str,
    data: &[u8],
    flags: &GzipFlags,
) -> i32 {
    let compressed = gzip_compress(data);
    if flags.to_stdout {
        ctx.output.stdout(&compressed);
        return 0;
    }
    let out_path = format!("{full}.gz");
    if write_file(ctx, "gzip", &out_path, &compressed) != 0 {
        return 1;
    }
    remove_original_if_needed(ctx, full, path, flags.keep);
    0
}

fn remove_original_if_needed(ctx: &mut UtilContext<'_>, full: &str, path: &str, keep: bool) {
    if !keep {
        if let Err(e) = ctx.fs.remove_file(full) {
            let msg = format!("gzip: warning: cannot remove '{path}': {e}\n");
            ctx.output.stderr(msg.as_bytes());
        }
    }
}

pub(crate) fn util_gzip(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, consumed) = match parse_gzip_flags(ctx, argv) {
        Ok(v) => v,
        Err(status) => return status,
    };
    let args = &argv[consumed..];

    if args.is_empty() {
        ctx.output.stderr(b"gzip: missing file operand\n");
        return 1;
    }

    let mut status = 0;
    for path in args {
        let full = resolve_path(ctx.cwd, path);
        let Ok(data) = read_file_bytes(ctx, path, "gzip") else {
            status = 1;
            continue;
        };

        let rc = if flags.decompress {
            gzip_decompress_file(ctx, path, &full, &data, &flags)
        } else {
            gzip_compress_file(ctx, path, &full, &data, &flags)
        };
        if rc != 0 {
            status = 1;
        }
    }

    status
}

pub(crate) fn util_gunzip(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    // gunzip is gzip -d
    let mut new_argv = vec!["gzip", "-d"];
    new_argv.extend_from_slice(&argv[1..]);
    util_gzip(ctx, &new_argv)
}

pub(crate) fn util_zcat(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    // zcat is gzip -dc
    let mut new_argv = vec!["gzip", "-d", "-c"];
    new_argv.extend_from_slice(&argv[1..]);
    util_gzip(ctx, &new_argv)
}

/// Helper: write data to a VFS path. Delegates to the shared helper in helpers.rs.
fn write_file(ctx: &mut UtilContext<'_>, cmd: &str, path: &str, data: &[u8]) -> i32 {
    write_file_bytes(ctx, cmd, path, data)
}

// ---------------------------------------------------------------------------
// tar — tape archive (USTAR format)
// ---------------------------------------------------------------------------

const TAR_BLOCK_SIZE: usize = 512;

#[allow(clippy::struct_excessive_bools)]
struct TarFlags<'a> {
    create: bool,
    extract: bool,
    list: bool,
    gzipped: bool,
    verbose: bool,
    archive: Option<&'a str>,
    change_dir: Option<&'a str>,
}

fn parse_tar_flags<'a>(
    ctx: &mut UtilContext<'_>,
    argv: &'a [&'a str],
) -> Result<(TarFlags<'a>, usize), i32> {
    let mut args = &argv[1..];
    let mut flags = TarFlags {
        create: false,
        extract: false,
        list: false,
        gzipped: false,
        verbose: false,
        archive: None,
        change_dir: None,
    };
    let mut consumed = 1;

    while let Some(arg) = args.first() {
        if *arg == "-f" && args.len() > 1 {
            flags.archive = Some(args[1]);
            args = &args[2..];
            consumed += 2;
        } else if *arg == "-C" && args.len() > 1 {
            flags.change_dir = Some(args[1]);
            args = &args[2..];
            consumed += 2;
        } else if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") {
            let skip_next = parse_tar_bundled_flags(ctx, arg, args, &mut flags)?;
            args = &args[1..];
            consumed += 1;
            if skip_next && !args.is_empty() {
                args = &args[1..];
                consumed += 1;
            }
        } else {
            break;
        }
    }
    Ok((flags, consumed))
}

/// Parse a bundled tar flag string like `-czvf`. Returns `true` if the next arg was consumed.
fn parse_tar_bundled_flags<'a>(
    ctx: &mut UtilContext<'_>,
    arg: &str,
    args: &[&'a str],
    flags: &mut TarFlags<'a>,
) -> Result<bool, i32> {
    let chars: Vec<char> = arg[1..].chars().collect();
    let mut skip_next = false;
    for (ci, ch) in chars.iter().enumerate() {
        match ch {
            'c' => flags.create = true,
            'x' => flags.extract = true,
            't' => flags.list = true,
            'z' => flags.gzipped = true,
            'v' => flags.verbose = true,
            'f' => {
                let _rest: String = chars[ci + 1..].iter().collect();
                if args.len() > 1 {
                    flags.archive = Some(args[1]);
                    skip_next = true;
                }
                break;
            }
            'C' => {
                if args.len() > 1 {
                    flags.change_dir = Some(args[1]);
                    skip_next = true;
                }
                break;
            }
            _ => {
                let msg = format!("tar: unknown option '-{ch}'\n");
                ctx.output.stderr(msg.as_bytes());
                return Err(1);
            }
        }
    }
    Ok(skip_next)
}

pub(crate) fn util_tar(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, consumed) = match parse_tar_flags(ctx, argv) {
        Ok(v) => v,
        Err(status) => return status,
    };
    let args = &argv[consumed..];

    let Some(archive_path) = flags.archive else {
        ctx.output.stderr(b"tar: no archive specified (use -f)\n");
        return 1;
    };

    let base_dir = flags.change_dir.unwrap_or(ctx.cwd);

    if flags.create {
        tar_create(
            ctx,
            archive_path,
            args,
            base_dir,
            flags.gzipped,
            flags.verbose,
        )
    } else if flags.extract {
        tar_extract(ctx, archive_path, base_dir, flags.gzipped, flags.verbose)
    } else if flags.list {
        tar_list(ctx, archive_path, flags.gzipped)
    } else {
        ctx.output.stderr(b"tar: must specify -c, -x, or -t\n");
        1
    }
}

fn tar_create(
    ctx: &mut UtilContext<'_>,
    archive_path: &str,
    files: &[&str],
    base_dir: &str,
    gzipped: bool,
    verbose: bool,
) -> i32 {
    let mut tar_data = Vec::new();

    for file in files {
        let full = resolve_path(base_dir, file);
        match ctx.fs.stat(&full) {
            Ok(meta) if meta.is_dir => {
                if tar_add_dir(ctx, &mut tar_data, &full, file, verbose) != 0 {
                    return 1;
                }
            }
            Ok(_) => {
                if tar_add_file(ctx, &mut tar_data, &full, file, verbose) != 0 {
                    return 1;
                }
            }
            Err(e) => {
                emit_error(ctx.output, "tar", file, &e);
                return 1;
            }
        }
    }

    // End-of-archive: two 512-byte zero blocks
    tar_data.extend_from_slice(&[0u8; TAR_BLOCK_SIZE * 2]);

    let output_data = if gzipped {
        gzip_compress(&tar_data)
    } else {
        tar_data
    };

    if archive_path == "-" {
        ctx.output.stdout(&output_data);
        return 0;
    }
    let full_archive = resolve_path(ctx.cwd, archive_path);
    write_file(ctx, "tar", &full_archive, &output_data)
}

fn tar_add_file(
    ctx: &mut UtilContext<'_>,
    tar_data: &mut Vec<u8>,
    full_path: &str,
    name: &str,
    verbose: bool,
) -> i32 {
    let data = match read_file_bytes_abs(ctx, full_path, name, "tar") {
        Ok(d) => d,
        Err(status) => return status,
    };

    if verbose {
        let msg = format!("{name}\n");
        ctx.output.stderr(msg.as_bytes());
    }

    let header = make_tar_header(name, data.len() as u64, b'0');
    tar_data.extend_from_slice(&header);
    tar_data.extend_from_slice(&data);

    // Pad to 512-byte boundary
    let remainder = data.len() % TAR_BLOCK_SIZE;
    if remainder != 0 {
        let padding = TAR_BLOCK_SIZE - remainder;
        tar_data.extend(std::iter::repeat_n(0u8, padding));
    }

    0
}

fn tar_add_dir(
    ctx: &mut UtilContext<'_>,
    tar_data: &mut Vec<u8>,
    full_path: &str,
    name: &str,
    verbose: bool,
) -> i32 {
    let dir_name = if name.ends_with('/') {
        name.to_string()
    } else {
        format!("{name}/")
    };

    if verbose {
        let msg = format!("{dir_name}\n");
        ctx.output.stderr(msg.as_bytes());
    }

    let header = make_tar_header(&dir_name, 0, b'5');
    tar_data.extend_from_slice(&header);

    let Ok(entries) = ctx.fs.read_dir(full_path) else {
        let msg = format!("tar: cannot read directory '{name}': I/O error\n");
        ctx.output.stderr(msg.as_bytes());
        return 1;
    };

    for entry in entries {
        let child_full = child_path(full_path, &entry.name);
        let child_name = format!("{dir_name}{}", entry.name);
        let rc = if entry.is_dir {
            tar_add_dir(ctx, tar_data, &child_full, &child_name, verbose)
        } else {
            tar_add_file(ctx, tar_data, &child_full, &child_name, verbose)
        };
        if rc != 0 {
            return 1;
        }
    }

    0
}

fn make_tar_header(name: &str, size: u64, typeflag: u8) -> [u8; TAR_BLOCK_SIZE] {
    let mut header = [0u8; TAR_BLOCK_SIZE];

    // Name (0..100)
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len().min(100);
    header[..name_len].copy_from_slice(&name_bytes[..name_len]);

    // Mode (100..108) — 0644 for files, 0755 for dirs
    let mode = if typeflag == b'5' {
        b"0000755\0"
    } else {
        b"0000644\0"
    };
    header[100..108].copy_from_slice(mode);

    // UID (108..116)
    header[108..116].copy_from_slice(b"0001000\0");

    // GID (116..124)
    header[116..124].copy_from_slice(b"0001000\0");

    // Size (124..136) — octal, 11 digits + null
    let size_str = format!("{size:011o}\0");
    let size_bytes = size_str.as_bytes();
    let sz_len = size_bytes.len().min(12);
    header[124..124 + sz_len].copy_from_slice(&size_bytes[..sz_len]);

    // Mtime (136..148)
    header[136..148].copy_from_slice(b"00000000000\0");

    // Typeflag (156)
    header[156] = typeflag;

    // USTAR magic (257..263)
    header[257..263].copy_from_slice(b"ustar\0");

    // USTAR version (263..265)
    header[263..265].copy_from_slice(b"00");

    // Username (265..297)
    header[265..269].copy_from_slice(b"user");

    // Groupname (297..329)
    header[297..302].copy_from_slice(b"group");

    // Checksum (148..156): sum of all header bytes treating checksum field as spaces
    header[148..156].copy_from_slice(b"        ");
    let checksum: u32 = header.iter().map(|&b| u32::from(b)).sum();
    let cksum_str = format!("{checksum:06o}\0 ");
    header[148..156].copy_from_slice(&cksum_str.as_bytes()[..8]);

    header
}

/// Parsed tar entry header.
struct TarEntry {
    name: String,
    size: usize,
    typeflag: u8,
}

/// Parse a tar header block. Returns `None` for end-of-archive.
fn parse_tar_entry_header(
    header: &[u8],
    ctx: &mut UtilContext<'_>,
) -> Result<Option<TarEntry>, i32> {
    if header.iter().all(|&b| b == 0) {
        return Ok(None);
    }

    let name_end = header[..100].iter().position(|&b| b == 0).unwrap_or(100);
    let name = String::from_utf8_lossy(&header[..name_end]).to_string();

    let size_str = String::from_utf8_lossy(&header[124..136])
        .trim_matches('\0')
        .trim()
        .to_string();
    let Ok(size) = u64::from_str_radix(&size_str, 8).map(|v| v as usize) else {
        let msg = format!("tar: invalid size in header for '{name}'\n");
        ctx.output.stderr(msg.as_bytes());
        return Err(1);
    };

    Ok(Some(TarEntry {
        name,
        size,
        typeflag: header[156],
    }))
}

fn extract_dir_entry(ctx: &mut UtilContext<'_>, full: &str, name: &str) {
    if ctx.fs.create_dir(full).is_err() && ctx.fs.stat(full).is_err() {
        let msg = format!("tar: cannot create directory '{name}'\n");
        ctx.output.stderr(msg.as_bytes());
    }
}

fn ensure_parent_dir(ctx: &mut UtilContext<'_>, full: &str, name: &str) {
    let Some(slash_pos) = full.rfind('/') else {
        return;
    };
    let parent = &full[..slash_pos];
    if !parent.is_empty()
        && ctx.fs.stat(parent).is_err()
        && ctx.fs.create_dir(parent).is_err()
        && ctx.fs.stat(parent).is_err()
    {
        let msg = format!("tar: cannot create directory for '{name}'\n");
        ctx.output.stderr(msg.as_bytes());
    }
}

fn extract_file_entry(
    ctx: &mut UtilContext<'_>,
    full: &str,
    name: &str,
    tar_data: &[u8],
    pos: usize,
    size: usize,
) -> i32 {
    ensure_parent_dir(ctx, full, name);
    let end = (pos + size).min(tar_data.len());
    let file_data = &tar_data[pos..end];
    write_file(ctx, "tar", full, file_data)
}

fn tar_load_data(
    ctx: &mut UtilContext<'_>,
    archive_path: &str,
    gzipped: bool,
) -> Result<Vec<u8>, i32> {
    let archive_data = if archive_path == "-" {
        read_stdin_bytes(ctx)
    } else {
        read_file_bytes(ctx, archive_path, "tar")?
    };
    if gzipped {
        gzip_decompress(&archive_data).map_err(|e| {
            emit_error(ctx.output, "tar", archive_path, &e);
            1
        })
    } else {
        Ok(archive_data)
    }
}

fn read_stdin_bytes(ctx: &mut UtilContext<'_>) -> Vec<u8> {
    let mut data = Vec::new();
    if let Some(mut stdin) = ctx.stdin.take() {
        use std::io::Read;
        let _ = stdin.read_to_end(&mut data);
    }
    data
}

fn tar_extract(
    ctx: &mut UtilContext<'_>,
    archive_path: &str,
    base_dir: &str,
    gzipped: bool,
    verbose: bool,
) -> i32 {
    let tar_data = match tar_load_data(ctx, archive_path, gzipped) {
        Ok(d) => d,
        Err(status) => return status,
    };

    let mut pos = 0;
    while pos + TAR_BLOCK_SIZE <= tar_data.len() {
        let entry = match parse_tar_entry_header(&tar_data[pos..pos + TAR_BLOCK_SIZE], ctx) {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(status) => return status,
        };

        pos += TAR_BLOCK_SIZE;
        let Some(next_pos) = tar_extract_entry(ctx, &entry, &tar_data, pos, base_dir, verbose)
        else {
            return 1;
        };
        pos = next_pos;
    }

    0
}

fn tar_extract_entry(
    ctx: &mut UtilContext<'_>,
    entry: &TarEntry,
    tar_data: &[u8],
    pos: usize,
    base_dir: &str,
    verbose: bool,
) -> Option<usize> {
    if verbose {
        let msg = format!("{}\n", entry.name);
        ctx.output.stderr(msg.as_bytes());
    }

    let full = resolve_path(base_dir, &entry.name);
    if entry.typeflag == b'5' || entry.name.ends_with('/') {
        extract_dir_entry(ctx, &full, &entry.name);
        return Some(pos);
    }

    if extract_file_entry(ctx, &full, &entry.name, tar_data, pos, entry.size) != 0 {
        return None;
    }
    Some(pos + entry.size.div_ceil(TAR_BLOCK_SIZE) * TAR_BLOCK_SIZE)
}

// ---------------------------------------------------------------------------
// unzip — extract ZIP archives
// ---------------------------------------------------------------------------

pub(crate) fn util_unzip(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (mut opts, args) = parse_unzip_args(argv);

    if args.is_empty() {
        ctx.output.stderr(b"unzip: missing archive operand\n");
        return 1;
    }

    let archive_path = args[0];
    unzip_apply_post_archive_dest(&mut opts.dest_dir, &args[1..]);

    let archive_data = match read_file_bytes(ctx, archive_path, "unzip") {
        Ok(d) => d,
        Err(status) => return status,
    };

    let base_dir = unzip_base_dir(ctx, opts.dest_dir.as_deref());

    unzip_emit_list_header(ctx, opts.list_only, opts.quiet);

    let list_only = opts.list_only;
    let quiet = opts.quiet;
    let data = &archive_data;
    let mut stats = UnzipStats::default();
    let mut status = 0;
    let extract_opts = UnzipOpts {
        overwrite: opts.overwrite,
        quiet,
        base_dir: &base_dir,
    };

    let mut pos = 0;
    while pos + 30 <= data.len() {
        match unzip_advance_to_header(data, &mut pos) {
            HeaderSearch::Found => {}
            HeaderSearch::End => break,
        }

        let Some(entry) = parse_zip_local_header(data, pos) else {
            break;
        };

        pos = entry.data_start;

        if list_only {
            unzip_list_entry(ctx, &entry, &mut stats, quiet);
            pos += entry.compressed_size;
            continue;
        }

        let rc = unzip_extract_entry(ctx, &entry, data, &extract_opts);
        if rc != 0 {
            status = rc;
        }

        stats.total_files += 1;
        stats.total_size += entry.uncompressed_size as u64;
        pos += entry.compressed_size;
    }

    unzip_emit_list_footer(ctx, &stats, quiet, list_only);

    status
}

struct ParsedUnzipOpts {
    list_only: bool,
    overwrite: bool,
    quiet: bool,
    dest_dir: Option<String>,
}

fn parse_unzip_args<'a>(argv: &'a [&'a str]) -> (ParsedUnzipOpts, &'a [&'a str]) {
    let mut args = &argv[1..];
    let mut opts = ParsedUnzipOpts {
        list_only: false,
        overwrite: false,
        quiet: false,
        dest_dir: None,
    };

    while let Some(arg) = args.first() {
        match *arg {
            "-l" => opts.list_only = true,
            "-o" => opts.overwrite = true,
            "-q" => opts.quiet = true,
            "-d" if args.len() > 1 => {
                opts.dest_dir = Some(args[1].to_string());
                args = &args[2..];
                continue;
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                if !apply_unzip_short_flags(arg, &mut opts) {
                    break;
                }
            }
            _ => break,
        }
        args = &args[1..];
    }

    (opts, args)
}

fn apply_unzip_short_flags(arg: &str, opts: &mut ParsedUnzipOpts) -> bool {
    for ch in arg[1..].chars() {
        match ch {
            'l' => opts.list_only = true,
            'o' => opts.overwrite = true,
            'q' => opts.quiet = true,
            _ => return false,
        }
    }
    true
}

fn unzip_apply_post_archive_dest(dest_dir: &mut Option<String>, rest: &[&str]) {
    if rest.len() >= 2 && rest[0] == "-d" {
        *dest_dir = Some(rest[1].to_string());
    }
}

fn unzip_base_dir(ctx: &mut UtilContext<'_>, dest_dir: Option<&str>) -> String {
    if let Some(dest) = dest_dir {
        let full = resolve_path(ctx.cwd, dest);
        let _ = ctx.fs.create_dir(&full);
        full
    } else {
        ctx.cwd.to_string()
    }
}

fn unzip_emit_list_header(ctx: &mut UtilContext<'_>, list_only: bool, quiet: bool) {
    if list_only && !quiet {
        ctx.output
            .stdout(b"  Length      Name\n---------  --------------------\n");
    }
}

fn unzip_emit_list_footer(
    ctx: &mut UtilContext<'_>,
    stats: &UnzipStats,
    quiet: bool,
    list_only: bool,
) {
    if list_only && !quiet {
        let footer = format!(
            "---------  --------------------\n{:>9}  {} file(s)\n",
            stats.total_size, stats.total_files
        );
        ctx.output.stdout(footer.as_bytes());
    }
}

struct UnzipOpts<'a> {
    overwrite: bool,
    quiet: bool,
    base_dir: &'a str,
}

#[derive(Default)]
struct UnzipStats {
    total_files: u32,
    total_size: u64,
}

enum HeaderSearch {
    Found,
    End,
}

fn unzip_advance_to_header(data: &[u8], pos: &mut usize) -> HeaderSearch {
    if &data[*pos..*pos + 4] == b"PK\x03\x04" {
        return HeaderSearch::Found;
    }
    if let Some(next) = find_pk_signature(&data[*pos..]) {
        *pos += next;
        HeaderSearch::Found
    } else {
        HeaderSearch::End
    }
}

struct ZipLocalEntry {
    raw_name: String,
    compression: u16,
    compressed_size: usize,
    uncompressed_size: usize,
    data_start: usize,
}

fn parse_zip_local_header(data: &[u8], pos: usize) -> Option<ZipLocalEntry> {
    let compression = u16_le(&data[pos + 8..pos + 10]);
    let compressed_size = u32_le(&data[pos + 18..pos + 22]) as usize;
    let uncompressed_size = u32_le(&data[pos + 22..pos + 26]) as usize;
    let name_len = u16_le(&data[pos + 26..pos + 28]) as usize;
    let extra_len = u16_le(&data[pos + 28..pos + 30]) as usize;

    let name_start = pos + 30;
    if name_start + name_len > data.len() {
        return None;
    }
    let raw_name = String::from_utf8_lossy(&data[name_start..name_start + name_len]).to_string();
    let data_start = name_start + name_len + extra_len;

    Some(ZipLocalEntry {
        raw_name,
        compression,
        compressed_size,
        uncompressed_size,
        data_start,
    })
}

fn unzip_list_entry(
    ctx: &mut UtilContext<'_>,
    entry: &ZipLocalEntry,
    stats: &mut UnzipStats,
    quiet: bool,
) {
    if !quiet {
        let line = format!("{:>9}  {}\n", entry.uncompressed_size, entry.raw_name);
        ctx.output.stdout(line.as_bytes());
    }
    stats.total_files += 1;
    stats.total_size += entry.uncompressed_size as u64;
}

/// Sanitize a zip entry name to prevent zip-slip attacks.
fn sanitize_zip_name(raw_name: &str) -> String {
    raw_name
        .trim_start_matches('/')
        .split('/')
        .filter(|c| *c != "..")
        .collect::<Vec<_>>()
        .join("/")
}

fn unzip_check_traversal(
    ctx: &mut UtilContext<'_>,
    full: &str,
    raw_name: &str,
    base_dir: &str,
) -> bool {
    if full.starts_with(base_dir) {
        return true;
    }
    let msg = format!("unzip: skipping '{raw_name}': path traversal detected\n");
    ctx.output.stderr(msg.as_bytes());
    false
}

fn unzip_ensure_parent(ctx: &mut UtilContext<'_>, full: &str) {
    if let Some(slash_pos) = full.rfind('/') {
        let parent = &full[..slash_pos];
        if !parent.is_empty() && ctx.fs.stat(parent).is_err() {
            let _ = ctx.fs.create_dir(parent);
        }
    }
}

fn unzip_extract_entry(
    ctx: &mut UtilContext<'_>,
    entry: &ZipLocalEntry,
    data: &[u8],
    opts: &UnzipOpts<'_>,
) -> i32 {
    let name = sanitize_zip_name(&entry.raw_name);
    let full = resolve_path(opts.base_dir, &name);

    if !unzip_check_traversal(ctx, &full, &entry.raw_name, opts.base_dir) {
        return 0;
    }

    if name.ends_with('/') {
        return unzip_extract_dir(ctx, &full, &name, opts.quiet);
    }

    match entry.compression {
        0 => unzip_extract_stored(ctx, entry, data, &full, &name, opts),
        8 => unzip_extract_deflated(ctx, entry, data, &full, &name, opts),
        other => {
            if !opts.quiet {
                let msg = format!("unzip: {name}: unsupported compression method {other}\n");
                ctx.output.stderr(msg.as_bytes());
            }
            1
        }
    }
}

/// Extract a DEFLATE-compressed zip entry via `miniz_oxide`.
fn unzip_extract_deflated(
    ctx: &mut UtilContext<'_>,
    entry: &ZipLocalEntry,
    data: &[u8],
    full: &str,
    name: &str,
    opts: &UnzipOpts<'_>,
) -> i32 {
    let end = (entry.data_start + entry.compressed_size).min(data.len());
    let compressed = &data[entry.data_start..end];

    let decompressed = match miniz_oxide::inflate::decompress_to_vec(compressed) {
        Ok(bytes) => bytes,
        Err(e) => {
            if !opts.quiet {
                let msg = format!("unzip: {name}: inflate failed: {e}\n");
                ctx.output.stderr(msg.as_bytes());
            }
            return 1;
        }
    };

    unzip_ensure_parent(ctx, full);

    if !opts.overwrite && ctx.fs.stat(full).is_ok() {
        if !opts.quiet {
            let msg = format!("unzip: {name}: already exists, skipping\n");
            ctx.output.stderr(msg.as_bytes());
        }
        return 0;
    }
    if write_file(ctx, "unzip", full, &decompressed) != 0 {
        return 1;
    }
    if !opts.quiet {
        let msg = format!("  inflating: {name}\n");
        ctx.output.stdout(msg.as_bytes());
    }
    0
}

fn unzip_extract_dir(ctx: &mut UtilContext<'_>, full: &str, name: &str, quiet: bool) -> i32 {
    let _ = ctx.fs.create_dir(full);
    if !quiet {
        let msg = format!("   creating: {name}\n");
        ctx.output.stdout(msg.as_bytes());
    }
    0
}

fn unzip_extract_stored(
    ctx: &mut UtilContext<'_>,
    entry: &ZipLocalEntry,
    data: &[u8],
    full: &str,
    name: &str,
    opts: &UnzipOpts<'_>,
) -> i32 {
    let end = (entry.data_start + entry.uncompressed_size).min(data.len());
    let file_data = &data[entry.data_start..end];

    unzip_ensure_parent(ctx, full);

    if !opts.overwrite && ctx.fs.stat(full).is_ok() {
        if !opts.quiet {
            let msg = format!("unzip: {name}: already exists, skipping\n");
            ctx.output.stderr(msg.as_bytes());
        }
        return 0;
    }
    if write_file(ctx, "unzip", full, file_data) != 0 {
        return 1;
    }
    if !opts.quiet {
        let msg = format!("  inflating: {name}\n");
        ctx.output.stdout(msg.as_bytes());
    }
    0
}

/// Find the next PK\x03\x04 signature in the data.
fn find_pk_signature(data: &[u8]) -> Option<usize> {
    for i in 0..data.len().saturating_sub(3) {
        if &data[i..i + 4] == b"PK\x03\x04" {
            return Some(i);
        }
    }
    None
}

/// Read a little-endian u16 from a 2-byte slice.
fn u16_le(data: &[u8]) -> u16 {
    u16::from_le_bytes([data[0], data[1]])
}

/// Read a little-endian u32 from a 4-byte slice.
fn u32_le(data: &[u8]) -> u32 {
    u32::from_le_bytes([data[0], data[1], data[2], data[3]])
}

fn tar_list(ctx: &mut UtilContext<'_>, archive_path: &str, gzipped: bool) -> i32 {
    let archive_data = match read_file_bytes(ctx, archive_path, "tar") {
        Ok(d) => d,
        Err(status) => return status,
    };

    let tar_data = if gzipped {
        match gzip_decompress(&archive_data) {
            Ok(d) => d,
            Err(e) => {
                emit_error(ctx.output, "tar", archive_path, &e);
                return 1;
            }
        }
    } else {
        archive_data
    };

    let mut pos = 0;
    while pos + TAR_BLOCK_SIZE <= tar_data.len() {
        let header = &tar_data[pos..pos + TAR_BLOCK_SIZE];

        if header.iter().all(|&b| b == 0) {
            break;
        }

        let name_end = header[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = String::from_utf8_lossy(&header[..name_end]).to_string();

        let size_str = String::from_utf8_lossy(&header[124..136])
            .trim_matches('\0')
            .trim()
            .to_string();
        let Ok(size) = u64::from_str_radix(&size_str, 8).map(|v| v as usize) else {
            let msg = format!("tar: invalid size in header for '{name}'\n");
            ctx.output.stderr(msg.as_bytes());
            return 1;
        };

        let line = format!("{name}\n");
        ctx.output.stdout(line.as_bytes());

        pos += TAR_BLOCK_SIZE;
        let blocks = size.div_ceil(TAR_BLOCK_SIZE);
        pos += blocks * TAR_BLOCK_SIZE;
    }

    0
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
        cwd: &str,
    ) -> (i32, VecOutput) {
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs,
                output: &mut output,
                cwd,
                stdin: None,
                state: None,
                network: None,
            };
            func(&mut ctx, argv)
        };
        (status, output)
    }

    #[test]
    fn gzip_roundtrip() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/test.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"Hello, World!").unwrap();
        fs.close(h);

        // Compress
        let (status, _) = run_util(util_gzip, &["gzip", "/test.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(fs.stat("/test.txt.gz").is_ok());
        assert!(fs.stat("/test.txt").is_err()); // original removed

        // Decompress
        let (status, _) = run_util(util_gunzip, &["gunzip", "/test.txt.gz"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(fs.stat("/test.txt").is_ok());
        assert!(fs.stat("/test.txt.gz").is_err());

        // Verify contents
        let h = fs.open("/test.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&data, b"Hello, World!");
    }

    #[test]
    fn gzip_keep() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/keep.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"data").unwrap();
        fs.close(h);

        let (status, _) = run_util(util_gzip, &["gzip", "-k", "/keep.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(fs.stat("/keep.txt").is_ok()); // original kept
        assert!(fs.stat("/keep.txt.gz").is_ok());
    }

    #[test]
    fn zcat_to_stdout() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/z.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"content").unwrap();
        fs.close(h);

        let (_, _) = run_util(util_gzip, &["gzip", "-k", "/z.txt"], &mut fs, "/");

        let (status, out) = run_util(util_zcat, &["zcat", "/z.txt.gz"], &mut fs, "/");
        assert_eq!(status, 0);
        assert_eq!(&out.stdout, b"content");
    }

    // Regression: prior to ADR-0025 wasmsh could only decode gzip
    // files it had produced itself (DEFLATE stored blocks).  Real
    // world files use fixed or dynamic Huffman blocks.  This test
    // builds a compressed gzip file via miniz_oxide directly (the
    // same way upstream `gzip` does) and verifies wasmsh's zcat can
    // decode it.
    #[test]
    fn zcat_decompresses_real_deflate_gzip() {
        let mut fs = MemoryFs::new();
        // ~6KB of highly redundant data so real DEFLATE has something
        // to compress (a stored-block implementation would produce an
        // output of the same size; a real implementation produces a
        // much smaller file).
        let original: Vec<u8> = (0..6000).map(|i| b'A' + (i % 26) as u8).collect();

        // Build a compressed gzip stream via miniz_oxide (same codec
        // path real upstream gzip uses).
        let mut bytes = vec![0x1F, 0x8B, 0x08, 0x00, 0, 0, 0, 0, 0, 0xFF];
        bytes.extend_from_slice(&miniz_oxide::deflate::compress_to_vec(&original, 6));
        bytes.extend_from_slice(&crc32(&original).to_le_bytes());
        bytes.extend_from_slice(&(original.len() as u32).to_le_bytes());
        // Make sure the compressed file is materially smaller than
        // the original — proves it's not a stored-blocks wrapper.
        assert!(
            bytes.len() < original.len() / 2,
            "expected real compression, got {} bytes for {}",
            bytes.len(),
            original.len()
        );

        let h = fs.open("/real.gz", OpenOptions::write()).unwrap();
        fs.write_file(h, &bytes).unwrap();
        fs.close(h);

        let (status, out) = run_util(util_zcat, &["zcat", "/real.gz"], &mut fs, "/");
        assert_eq!(status, 0);
        assert_eq!(out.stdout, original);
    }

    #[test]
    fn tar_create_and_extract() {
        let mut fs = MemoryFs::new();
        fs.create_dir("/src").unwrap();
        let h = fs.open("/src/a.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"file a").unwrap();
        fs.close(h);
        let h = fs.open("/src/b.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"file b").unwrap();
        fs.close(h);

        // Create tar
        let (status, _) = run_util(util_tar, &["tar", "-cf", "/test.tar", "src"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(fs.stat("/test.tar").is_ok());

        // Remove originals
        let _ = fs.remove_file("/src/a.txt");
        let _ = fs.remove_file("/src/b.txt");
        let _ = fs.remove_dir("/src");

        // Extract
        fs.create_dir("/out").unwrap();
        let (status, _) = run_util(
            util_tar,
            &["tar", "-xf", "/test.tar", "-C", "/out"],
            &mut fs,
            "/",
        );
        assert_eq!(status, 0);

        // Verify
        let h = fs.open("/out/src/a.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&data, b"file a");
    }

    #[test]
    fn tar_list() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/f.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"data").unwrap();
        fs.close(h);

        let (_, _) = run_util(
            util_tar,
            &["tar", "-cf", "/list.tar", "f.txt"],
            &mut fs,
            "/",
        );

        let (status, out) = run_util(util_tar, &["tar", "-tf", "/list.tar"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(out.stdout_str().contains("f.txt"));
    }

    #[test]
    fn tar_create_to_stdout_dash() {
        // `tar -cf - src` should write the archive bytes to stdout
        let mut fs = MemoryFs::new();
        fs.create_dir("/src").unwrap();
        let h = fs.open("/src/a.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"file a").unwrap();
        fs.close(h);

        let (status, out) = run_util(util_tar, &["tar", "-cf", "-", "src"], &mut fs, "/");
        assert_eq!(status, 0);
        // Tar stream should be non-empty and contain the filename in the header
        assert!(!out.stdout.is_empty(), "tar -cf - produced no output");
        assert!(
            out.stdout.windows(5).any(|w| w == b"src/a"),
            "tar stream missing expected header"
        );
        // No file named "-" should have been created
        assert!(fs.stat("/-").is_err());
    }

    #[test]
    fn tar_create_gzipped_to_stdout_dash() {
        // `tar -czf - src` should produce a gzip-wrapped tar on stdout
        let mut fs = MemoryFs::new();
        fs.create_dir("/src").unwrap();
        let h = fs.open("/src/a.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"file a").unwrap();
        fs.close(h);

        let (status, out) = run_util(util_tar, &["tar", "-czf", "-", "src"], &mut fs, "/");
        assert_eq!(status, 0);
        // Gzip magic bytes
        assert!(out.stdout.len() >= 2, "tar -czf - produced no output");
        assert_eq!(&out.stdout[0..2], &[0x1f, 0x8b], "missing gzip magic");
        assert!(fs.stat("/-").is_err());
    }

    #[test]
    fn tar_extract_from_stdin_dash() {
        // Build a tar.gz in memory, then feed it through stdin with `tar -xzf -`
        let mut fs = MemoryFs::new();
        fs.create_dir("/src").unwrap();
        let h = fs.open("/src/a.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"file a").unwrap();
        fs.close(h);

        let (_, out) = run_util(util_tar, &["tar", "-czf", "-", "src"], &mut fs, "/");
        let archive_bytes = out.stdout.clone();
        assert!(!archive_bytes.is_empty());

        // Blow away originals
        let _ = fs.remove_file("/src/a.txt");
        let _ = fs.remove_dir("/src");

        // Extract from stdin
        fs.create_dir("/out").unwrap();
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut output,
                cwd: "/",
                stdin: Some(crate::UtilStdin::from_bytes(&archive_bytes)),
                state: None,
                network: None,
            };
            util_tar(&mut ctx, &["tar", "-xzf", "-", "-C", "/out"])
        };
        assert_eq!(status, 0);

        let h = fs.open("/out/src/a.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&data, b"file a");
    }

    #[test]
    fn tar_gzipped_roundtrip() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/gz.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"gzipped content").unwrap();
        fs.close(h);

        // Create .tar.gz
        let (status, _) = run_util(
            util_tar,
            &["tar", "-czf", "/test.tar.gz", "gz.txt"],
            &mut fs,
            "/",
        );
        assert_eq!(status, 0);

        // Remove original
        let _ = fs.remove_file("/gz.txt");

        // Extract
        let (status, _) = run_util(util_tar, &["tar", "-xzf", "/test.tar.gz"], &mut fs, "/");
        assert_eq!(status, 0);

        let h = fs.open("/gz.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&data, b"gzipped content");
    }

    #[test]
    fn crc32_known() {
        // CRC-32 of empty string is 0x00000000
        assert_eq!(crc32(b""), 0x0000_0000);
        // CRC-32 of "123456789" is 0xCBF43926
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn gzip_empty_data() {
        let compressed = gzip_compress(b"");
        let decompressed = gzip_decompress(&compressed).unwrap();
        assert!(decompressed.is_empty());
    }

    // -----------------------------------------------------------------------
    // unzip tests
    // -----------------------------------------------------------------------

    /// Build a minimal valid stored ZIP archive with one file entry.
    fn make_stored_zip(name: &str, content: &[u8]) -> Vec<u8> {
        let mut zip = Vec::new();
        // Local file header
        zip.extend_from_slice(b"PK\x03\x04"); // signature
        zip.extend_from_slice(&[20, 0]); // version needed (2.0)
        zip.extend_from_slice(&[0, 0]); // flags
        zip.extend_from_slice(&[0, 0]); // compression method 0 (stored)
        zip.extend_from_slice(&[0, 0]); // mod time
        zip.extend_from_slice(&[0, 0]); // mod date
        zip.extend_from_slice(&[0, 0, 0, 0]); // crc32 (unused)
        zip.extend_from_slice(&(content.len() as u32).to_le_bytes()); // compressed size
        zip.extend_from_slice(&(content.len() as u32).to_le_bytes()); // uncompressed size
        zip.extend_from_slice(&(name.len() as u16).to_le_bytes()); // name length
        zip.extend_from_slice(&[0, 0]); // extra length
        zip.extend_from_slice(name.as_bytes()); // filename
        zip.extend_from_slice(content); // data
        zip
    }

    fn run_unzip(argv: &[&str], fs: &mut MemoryFs) -> (i32, VecOutput) {
        run_util(util_unzip, argv, fs, "/")
    }

    /// Build a minimal DEFLATE-compressed ZIP archive with one file
    /// entry.  Regression fixture for ADR-0025 which added real
    /// DEFLATE support to unzip.
    fn make_deflated_zip(name: &str, content: &[u8]) -> Vec<u8> {
        let compressed = miniz_oxide::deflate::compress_to_vec(content, 6);
        let crc = crc32(content);

        let mut zip = Vec::new();
        zip.extend_from_slice(b"PK\x03\x04"); // signature
        zip.extend_from_slice(&[20, 0]); // version needed (2.0)
        zip.extend_from_slice(&[0, 0]); // flags
        zip.extend_from_slice(&[8, 0]); // compression method 8 (deflate)
        zip.extend_from_slice(&[0, 0]); // mod time
        zip.extend_from_slice(&[0, 0]); // mod date
        zip.extend_from_slice(&crc.to_le_bytes());
        zip.extend_from_slice(&(compressed.len() as u32).to_le_bytes()); // compressed size
        zip.extend_from_slice(&(content.len() as u32).to_le_bytes()); // uncompressed size
        zip.extend_from_slice(&(name.len() as u16).to_le_bytes()); // name length
        zip.extend_from_slice(&[0, 0]); // extra length
        zip.extend_from_slice(name.as_bytes());
        zip.extend_from_slice(&compressed);
        zip
    }

    #[test]
    fn unzip_deflated_entry() {
        let mut fs = MemoryFs::new();
        // Make the payload large and redundant so DEFLATE actually
        // compresses it — a stored entry would be the same size.
        let payload: Vec<u8> = (0..4096).map(|i| b'A' + (i % 26) as u8).collect();
        let zip_data = make_deflated_zip("big.txt", &payload);
        // Sanity check: the deflated archive should be meaningfully
        // smaller than the raw payload.
        assert!(zip_data.len() < payload.len(), "expected compression");

        let h = fs.open("/d.zip", OpenOptions::write()).unwrap();
        fs.write_file(h, &zip_data).unwrap();
        fs.close(h);

        let (status, _) = run_unzip(&["unzip", "/d.zip"], &mut fs);
        assert_eq!(status, 0);

        let h = fs.open("/big.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(data, payload);
    }

    #[test]
    fn unzip_stored() {
        let mut fs = MemoryFs::new();
        let zip_data = make_stored_zip("hello.txt", b"Hello World!");
        let h = fs.open("/test.zip", OpenOptions::write()).unwrap();
        fs.write_file(h, &zip_data).unwrap();
        fs.close(h);

        let (status, _) = run_unzip(&["unzip", "/test.zip"], &mut fs);
        assert_eq!(status, 0);

        // Verify extracted file
        let h = fs.open("/hello.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&data, b"Hello World!");
    }

    #[test]
    fn unzip_list() {
        let mut fs = MemoryFs::new();
        let zip_data = make_stored_zip("data.csv", b"a,b,c\n1,2,3\n");
        let h = fs.open("/archive.zip", OpenOptions::write()).unwrap();
        fs.write_file(h, &zip_data).unwrap();
        fs.close(h);

        let (status, out) = run_unzip(&["unzip", "-l", "/archive.zip"], &mut fs);
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("data.csv"), "got: {s}");
        // Should NOT extract the file
        assert!(fs.stat("/data.csv").is_err());
    }

    #[test]
    fn unzip_to_dir() {
        let mut fs = MemoryFs::new();
        let zip_data = make_stored_zip("readme.txt", b"README content");
        let h = fs.open("/pkg.zip", OpenOptions::write()).unwrap();
        fs.write_file(h, &zip_data).unwrap();
        fs.close(h);

        let (status, _) = run_unzip(&["unzip", "/pkg.zip", "-d", "/out"], &mut fs);
        assert_eq!(status, 0);

        // File should be extracted into /out
        let h = fs.open("/out/readme.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&data, b"README content");
    }

    #[test]
    fn unzip_quiet() {
        let mut fs = MemoryFs::new();
        let zip_data = make_stored_zip("file.txt", b"data");
        let h = fs.open("/q.zip", OpenOptions::write()).unwrap();
        fs.write_file(h, &zip_data).unwrap();
        fs.close(h);

        let (status, out) = run_unzip(&["unzip", "-q", "/q.zip"], &mut fs);
        assert_eq!(status, 0);
        // Quiet mode should suppress "inflating:" messages
        assert!(
            !out.stdout_str().contains("inflating"),
            "stdout should be quiet, got: {}",
            out.stdout_str()
        );
    }

    #[test]
    fn unzip_missing_file() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_unzip(&["unzip", "/nonexistent.zip"], &mut fs);
        assert_eq!(status, 1);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.is_empty(), "expected error message on stderr");
    }

    #[test]
    fn unzip_creates_dirs() {
        let mut fs = MemoryFs::new();
        // ZIP entry with a path that includes a directory
        let zip_data = make_stored_zip("subdir/nested.txt", b"nested content");
        let h = fs.open("/dirs.zip", OpenOptions::write()).unwrap();
        fs.write_file(h, &zip_data).unwrap();
        fs.close(h);

        let (status, _) = run_unzip(&["unzip", "/dirs.zip"], &mut fs);
        assert_eq!(status, 0);

        // The parent directory should have been created
        let h = fs.open("/subdir/nested.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&data, b"nested content");
    }

    #[test]
    fn gzip_large_data() {
        // Test data larger than one stored block (65535 bytes)
        let data: Vec<u8> = (0u8..=255).cycle().take(70000).collect();
        let compressed = gzip_compress(&data);
        let decompressed = gzip_decompress(&compressed).unwrap();
        assert_eq!(data, decompressed);
    }

    // -------------------------------------------------------------------
    // tar -tf  list contents
    // -------------------------------------------------------------------

    #[test]
    fn tar_tf_lists_multiple_files() {
        let mut fs = MemoryFs::new();
        fs.create_dir("/src").unwrap();
        let h = fs.open("/src/one.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"one").unwrap();
        fs.close(h);
        let h = fs.open("/src/two.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"two").unwrap();
        fs.close(h);

        let (_, _) = run_util(util_tar, &["tar", "-cf", "/list.tar", "src"], &mut fs, "/");
        let (status, out) = run_util(util_tar, &["tar", "-tf", "/list.tar"], &mut fs, "/");
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("one.txt"), "expected one.txt in listing: {s}");
        assert!(s.contains("two.txt"), "expected two.txt in listing: {s}");
    }

    // -------------------------------------------------------------------
    // tar -v  verbose output
    // -------------------------------------------------------------------

    #[test]
    fn tar_verbose_create() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/v.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"verbose").unwrap();
        fs.close(h);

        let (status, out) = run_util(util_tar, &["tar", "-cvf", "/v.tar", "v.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("v.txt"),
            "expected verbose file name on stderr: {err}"
        );
    }

    #[test]
    fn tar_verbose_extract() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/v.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"data").unwrap();
        fs.close(h);
        let (_, _) = run_util(util_tar, &["tar", "-cf", "/v.tar", "v.txt"], &mut fs, "/");
        let _ = fs.remove_file("/v.txt");
        let (status, out) = run_util(util_tar, &["tar", "-xvf", "/v.tar"], &mut fs, "/");
        assert_eq!(status, 0);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("v.txt"), "expected verbose on extract: {err}");
    }

    // -------------------------------------------------------------------
    // tar -C /dir  change directory
    // -------------------------------------------------------------------

    #[test]
    fn tar_change_dir_create() {
        let mut fs = MemoryFs::new();
        fs.create_dir("/mydir").unwrap();
        let h = fs.open("/mydir/f.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"content").unwrap();
        fs.close(h);

        let (status, _) = run_util(
            util_tar,
            &["tar", "-cf", "/out.tar", "-C", "/mydir", "f.txt"],
            &mut fs,
            "/",
        );
        assert_eq!(status, 0);

        // Extract to /dest
        fs.create_dir("/dest").unwrap();
        let (status, _) = run_util(
            util_tar,
            &["tar", "-xf", "/out.tar", "-C", "/dest"],
            &mut fs,
            "/",
        );
        assert_eq!(status, 0);
        let h = fs.open("/dest/f.txt", OpenOptions::read()).unwrap();
        let d = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&d, b"content");
    }

    // -------------------------------------------------------------------
    // tar with nested directories
    // -------------------------------------------------------------------

    #[test]
    fn tar_nested_directories() {
        let mut fs = MemoryFs::new();
        fs.create_dir("/nest").unwrap();
        fs.create_dir("/nest/sub").unwrap();
        let h = fs.open("/nest/sub/deep.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"deep content").unwrap();
        fs.close(h);

        let (status, _) = run_util(
            util_tar,
            &["tar", "-cf", "/nested.tar", "nest"],
            &mut fs,
            "/",
        );
        assert_eq!(status, 0);

        let _ = fs.remove_file("/nest/sub/deep.txt");
        let _ = fs.remove_dir("/nest/sub");
        let _ = fs.remove_dir("/nest");

        let (status, _) = run_util(util_tar, &["tar", "-xf", "/nested.tar"], &mut fs, "/");
        assert_eq!(status, 0);
        let h = fs.open("/nest/sub/deep.txt", OpenOptions::read()).unwrap();
        let d = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&d, b"deep content");
    }

    // -------------------------------------------------------------------
    // gzip -k  keep original
    // -------------------------------------------------------------------

    #[test]
    fn gzip_keep_flag() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/kept.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"keep me").unwrap();
        fs.close(h);

        let (status, _) = run_util(util_gzip, &["gzip", "-k", "/kept.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        assert!(fs.stat("/kept.txt").is_ok(), "original should be kept");
        assert!(fs.stat("/kept.txt.gz").is_ok(), "compressed should exist");

        // Verify roundtrip
        let (status, _) = run_util(util_gunzip, &["gunzip", "-k", "/kept.txt.gz"], &mut fs, "/");
        assert_eq!(status, 0);
        let h = fs.open("/kept.txt", OpenOptions::read()).unwrap();
        let d = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&d, b"keep me");
    }

    // -------------------------------------------------------------------
    // gzip -c  write to stdout
    // -------------------------------------------------------------------

    #[test]
    fn gzip_to_stdout() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/stdout.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"stdout data").unwrap();
        fs.close(h);

        let (status, out) = run_util(util_gzip, &["gzip", "-c", "/stdout.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        // Output should be gzip format (magic bytes)
        assert!(out.stdout.len() > 2);
        assert_eq!(out.stdout[0], 0x1F);
        assert_eq!(out.stdout[1], 0x8B);
        // Original file should still exist (since output went to stdout)
        assert!(fs.stat("/stdout.txt").is_ok());
    }

    // -------------------------------------------------------------------
    // gunzip error on invalid data
    // -------------------------------------------------------------------

    #[test]
    fn gunzip_invalid_data() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/bad.gz", OpenOptions::write()).unwrap();
        fs.write_file(h, b"not gzip data at all").unwrap();
        fs.close(h);

        let (status, out) = run_util(util_gunzip, &["gunzip", "/bad.gz"], &mut fs, "/");
        assert_eq!(status, 1);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.is_empty(), "expected error on stderr");
    }

    // -------------------------------------------------------------------
    // zcat (decompress to stdout)
    // -------------------------------------------------------------------

    #[test]
    fn zcat_decompresses_to_stdout() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/zc.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"zcat content").unwrap();
        fs.close(h);

        let (_, _) = run_util(util_gzip, &["gzip", "-k", "/zc.txt"], &mut fs, "/");
        let (status, out) = run_util(util_zcat, &["zcat", "/zc.txt.gz"], &mut fs, "/");
        assert_eq!(status, 0);
        assert_eq!(&out.stdout, b"zcat content");
        // Compressed file should still exist
        assert!(fs.stat("/zc.txt.gz").is_ok());
    }

    // -------------------------------------------------------------------
    // unzip -o  overwrite
    // -------------------------------------------------------------------

    #[test]
    fn unzip_overwrite() {
        let mut fs = MemoryFs::new();
        // Pre-create file with old content
        let h = fs.open("/ow.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"old content").unwrap();
        fs.close(h);

        // Create zip with new content
        let zip_data = make_stored_zip("ow.txt", b"new content");
        let h = fs.open("/ow.zip", OpenOptions::write()).unwrap();
        fs.write_file(h, &zip_data).unwrap();
        fs.close(h);

        // Without -o, should skip the file
        let (status, out) = run_unzip(&["unzip", "/ow.zip"], &mut fs);
        assert_eq!(status, 0);
        let h = fs.open("/ow.txt", OpenOptions::read()).unwrap();
        let d = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&d, b"old content"); // not overwritten
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("already exists"));

        // With -o, should overwrite
        let (status, _) = run_unzip(&["unzip", "-o", "/ow.zip"], &mut fs);
        assert_eq!(status, 0);
        let h = fs.open("/ow.txt", OpenOptions::read()).unwrap();
        let d = fs.read_file(h).unwrap();
        fs.close(h);
        assert_eq!(&d, b"new content");
    }

    // -------------------------------------------------------------------
    // unzip with path traversal attempt (should be blocked)
    // -------------------------------------------------------------------

    #[test]
    fn unzip_path_traversal_blocked() {
        let mut fs = MemoryFs::new();
        // Create a zip entry with path traversal
        let zip_data = make_stored_zip("../../etc/passwd", b"evil");
        let h = fs.open("/evil.zip", OpenOptions::write()).unwrap();
        fs.write_file(h, &zip_data).unwrap();
        fs.close(h);

        // Extract to /safe dir
        fs.create_dir("/safe").unwrap();
        let (status, _) = run_unzip(&["unzip", "/evil.zip", "-d", "/safe"], &mut fs);
        // The '..' components should be stripped, not escape /safe
        // Verify the file does NOT exist outside /safe
        assert_eq!(status, 0);
        assert!(
            fs.stat("/etc/passwd").is_err(),
            "path traversal should be blocked"
        );
    }

    // -------------------------------------------------------------------
    // unzip with directory entries
    // -------------------------------------------------------------------

    #[test]
    fn unzip_directory_entries() {
        let mut fs = MemoryFs::new();
        // Create zip with a directory entry (name ends with /)
        let zip_data = make_stored_zip("mydir/", b"");
        let h = fs.open("/dirs.zip", OpenOptions::write()).unwrap();
        fs.write_file(h, &zip_data).unwrap();
        fs.close(h);

        let (status, out) = run_unzip(&["unzip", "/dirs.zip"], &mut fs);
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("creating"), "expected 'creating:' message: {s}");
        assert!(fs.stat("/mydir").is_ok());
    }

    // -------------------------------------------------------------------
    // Corrupt tar header handling
    // -------------------------------------------------------------------

    #[test]
    fn tar_corrupt_header() {
        let mut fs = MemoryFs::new();
        // Create garbage data that looks like a tar header but has invalid size
        let mut corrupt = vec![0u8; 512];
        corrupt[0..5].copy_from_slice(b"test\0"); // name
                                                  // Leave size field as zeros (valid, 0 size)
                                                  // But make it look like a non-zero header by setting some bytes
        corrupt[100..108].copy_from_slice(b"0000644\0"); // mode
        corrupt[156] = b'0'; // typeflag: regular file
                             // Checksum field is wrong, but our implementation doesn't validate it
                             // Put invalid octal in size field
        corrupt[124..136].copy_from_slice(b"zzzzzzzzzzz\0");

        let h = fs.open("/corrupt.tar", OpenOptions::write()).unwrap();
        fs.write_file(h, &corrupt).unwrap();
        fs.close(h);

        let (status, out) = run_util(util_tar, &["tar", "-tf", "/corrupt.tar"], &mut fs, "/");
        assert_eq!(status, 1);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.is_empty(), "expected error for corrupt tar");
    }
}
