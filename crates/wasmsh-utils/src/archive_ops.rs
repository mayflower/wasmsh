//! Archive utilities: tar, gzip, gunzip, zcat.

use wasmsh_fs::{OpenOptions, Vfs};

use crate::helpers::{crc32, emit_error, resolve_path};
use crate::UtilContext;

// ---------------------------------------------------------------------------
// gzip / gunzip / zcat
// ---------------------------------------------------------------------------

/// Create a valid gzip file using DEFLATE stored blocks (no actual compression).
fn gzip_compress(data: &[u8]) -> Vec<u8> {
    // Gzip header (RFC 1952)
    let mut out = vec![
        0x1F, // magic
        0x8B, // magic
        0x08, // compression method: deflate
        0x00, // flags: none
    ];
    out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // mtime
    out.push(0x00); // extra flags
    out.push(0xFF); // OS: unknown

    // DEFLATE stored blocks — each block max 65535 bytes
    let mut offset = 0;
    while offset < data.len() {
        let remaining = data.len() - offset;
        let block_size = remaining.min(65535);
        let is_last = offset + block_size >= data.len();

        // Block header byte: BFINAL (1 bit) | BTYPE=00 (2 bits) = stored
        out.push(u8::from(is_last));

        // LEN (2 bytes, little-endian)
        let len = block_size as u16;
        out.push((len & 0xFF) as u8);
        out.push((len >> 8) as u8);

        // NLEN (one's complement of LEN)
        let nlen = !len;
        out.push((nlen & 0xFF) as u8);
        out.push((nlen >> 8) as u8);

        // Raw data
        out.extend_from_slice(&data[offset..offset + block_size]);
        offset += block_size;
    }

    // Handle empty data: need at least one stored block
    if data.is_empty() {
        out.push(0x01); // final block
        out.extend_from_slice(&[0x00, 0x00, 0xFF, 0xFF]); // len=0, nlen=0xFFFF
    }

    // Gzip trailer: CRC-32 + original size (both little-endian)
    let checksum = crc32(data);
    out.extend_from_slice(&checksum.to_le_bytes());
    let orig_size = data.len() as u32;
    out.extend_from_slice(&orig_size.to_le_bytes());

    out
}

/// Decompress a gzip file. Supports DEFLATE stored blocks only.
fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>, String> {
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
    let mut pos = 10;

    // Skip optional fields based on flags
    if flags & 0x04 != 0 {
        // FEXTRA
        if pos + 2 > data.len() {
            return Err("truncated gzip header".to_string());
        }
        let xlen = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2 + xlen;
    }
    if flags & 0x08 != 0 {
        // FNAME: null-terminated string
        while pos < data.len() && data[pos] != 0 {
            pos += 1;
        }
        pos += 1; // skip null
    }
    if flags & 0x10 != 0 {
        // FCOMMENT: null-terminated string
        while pos < data.len() && data[pos] != 0 {
            pos += 1;
        }
        pos += 1;
    }
    if flags & 0x02 != 0 {
        // FHCRC
        pos += 2;
    }

    // Parse DEFLATE stored blocks
    let mut output = Vec::new();
    loop {
        if pos >= data.len() {
            break;
        }
        let header = data[pos];
        let is_final = header & 0x01 != 0;
        let btype = (header >> 1) & 0x03;
        pos += 1;

        if btype == 0 {
            // Stored block
            if pos + 4 > data.len() {
                return Err("truncated stored block header".to_string());
            }
            let len = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
            // Skip nlen (pos+2, pos+3)
            pos += 4;
            if pos + len > data.len() {
                return Err("truncated stored block data".to_string());
            }
            output.extend_from_slice(&data[pos..pos + len]);
            pos += len;
        } else {
            return Err(format!("unsupported DEFLATE block type {btype}"));
        }

        if is_final {
            break;
        }
    }

    // Verify CRC-32 and size from trailer (if present)
    if pos + 8 <= data.len() {
        let expected_crc =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        let actual_crc = crc32(&output);
        if expected_crc != actual_crc {
            return Err("CRC-32 mismatch".to_string());
        }
    }

    Ok(output)
}

pub(crate) fn util_gzip(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut to_stdout = false;
    let mut decompress = false;
    let mut keep = false;

    while let Some(arg) = args.first() {
        match *arg {
            "-c" => {
                to_stdout = true;
                args = &args[1..];
            }
            "-d" => {
                decompress = true;
                args = &args[1..];
            }
            "-k" => {
                keep = true;
                args = &args[1..];
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                // Parse combined flags like -cd, -ck
                for ch in arg[1..].chars() {
                    match ch {
                        'c' => to_stdout = true,
                        'd' => decompress = true,
                        'k' => keep = true,
                        _ => {
                            let msg = format!("gzip: unknown option '-{ch}'\n");
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
        ctx.output.stderr(b"gzip: missing file operand\n");
        return 1;
    }

    let mut status = 0;
    for path in args {
        let full = resolve_path(ctx.cwd, path);
        let data = match ctx.fs.open(&full, OpenOptions::read()) {
            Ok(h) => match ctx.fs.read_file(h) {
                Ok(d) => {
                    ctx.fs.close(h);
                    d
                }
                Err(e) => {
                    ctx.fs.close(h);
                    emit_error(ctx.output, "gzip", path, &e);
                    status = 1;
                    continue;
                }
            },
            Err(e) => {
                emit_error(ctx.output, "gzip", path, &e);
                status = 1;
                continue;
            }
        };

        if decompress {
            let decompressed = match gzip_decompress(&data) {
                Ok(d) => d,
                Err(e) => {
                    emit_error(ctx.output, "gzip", path, &e);
                    status = 1;
                    continue;
                }
            };
            if to_stdout {
                ctx.output.stdout(&decompressed);
            } else {
                let out_path = if std::path::Path::new(path)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("gz"))
                {
                    full[..full.len() - 3].to_string()
                } else {
                    format!("{full}.out")
                };
                if write_file(ctx, "gzip", &out_path, &decompressed) != 0 {
                    status = 1;
                    continue;
                }
                if !keep {
                    if let Err(e) = ctx.fs.remove_file(&full) {
                        let msg = format!("gzip: warning: cannot remove '{path}': {e}\n");
                        ctx.output.stderr(msg.as_bytes());
                    }
                }
            }
        } else {
            let compressed = gzip_compress(&data);
            if to_stdout {
                ctx.output.stdout(&compressed);
            } else {
                let out_path = format!("{full}.gz");
                if write_file(ctx, "gzip", &out_path, &compressed) != 0 {
                    status = 1;
                    continue;
                }
                if !keep {
                    if let Err(e) = ctx.fs.remove_file(&full) {
                        let msg = format!("gzip: warning: cannot remove '{path}': {e}\n");
                        ctx.output.stderr(msg.as_bytes());
                    }
                }
            }
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

/// Helper: write data to a VFS path.
fn write_file(ctx: &mut UtilContext<'_>, cmd: &str, path: &str, data: &[u8]) -> i32 {
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

// ---------------------------------------------------------------------------
// tar — tape archive (USTAR format)
// ---------------------------------------------------------------------------

const TAR_BLOCK_SIZE: usize = 512;

pub(crate) fn util_tar(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut create = false;
    let mut extract = false;
    let mut list = false;
    let mut gzipped = false;
    let mut verbose = false;
    let mut archive: Option<&str> = None;
    let mut change_dir: Option<&str> = None;

    // Parse flags — support both combined (-czf) and separate (-c -z -f) forms
    while let Some(arg) = args.first() {
        if *arg == "-f" && args.len() > 1 {
            archive = Some(args[1]);
            args = &args[2..];
        } else if *arg == "-C" && args.len() > 1 {
            change_dir = Some(args[1]);
            args = &args[2..];
        } else if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") {
            let chars: Vec<char> = arg[1..].chars().collect();
            let mut skip_next = false;
            for (ci, ch) in chars.iter().enumerate() {
                match ch {
                    'c' => create = true,
                    'x' => extract = true,
                    't' => list = true,
                    'z' => gzipped = true,
                    'v' => verbose = true,
                    'f' => {
                        // The archive name is either the rest of the flag or the next arg
                        let rest: String = chars[ci + 1..].iter().collect();
                        if !rest.is_empty() {
                            // Archive name is embedded in the flags (rare)
                            // Skip for simplicity; expect next arg
                        }
                        if args.len() > 1 {
                            archive = Some(args[1]);
                            skip_next = true;
                        }
                        break;
                    }
                    'C' => {
                        if args.len() > 1 {
                            change_dir = Some(args[1]);
                            skip_next = true;
                        }
                        break;
                    }
                    _ => {
                        let msg = format!("tar: unknown option '-{ch}'\n");
                        ctx.output.stderr(msg.as_bytes());
                        return 1;
                    }
                }
            }
            args = &args[1..];
            if skip_next && !args.is_empty() {
                args = &args[1..];
            }
        } else {
            break;
        }
    }

    let Some(archive_path) = archive else {
        ctx.output.stderr(b"tar: no archive specified (use -f)\n");
        return 1;
    };

    let base_dir = change_dir.unwrap_or(ctx.cwd);

    if create {
        tar_create(ctx, archive_path, args, base_dir, gzipped, verbose)
    } else if extract {
        tar_extract(ctx, archive_path, base_dir, gzipped, verbose)
    } else if list {
        tar_list(ctx, archive_path, gzipped)
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
    let data = match ctx.fs.open(full_path, OpenOptions::read()) {
        Ok(h) => match ctx.fs.read_file(h) {
            Ok(d) => {
                ctx.fs.close(h);
                d
            }
            Err(e) => {
                ctx.fs.close(h);
                emit_error(ctx.output, "tar", name, &e);
                return 1;
            }
        },
        Err(e) => {
            emit_error(ctx.output, "tar", name, &e);
            return 1;
        }
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
    // Add directory entry
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

    // Recursively add entries
    let Ok(entries) = ctx.fs.read_dir(full_path) else {
        let msg = format!("tar: cannot read directory '{name}': I/O error\n");
        ctx.output.stderr(msg.as_bytes());
        return 1;
    };

    for entry in entries {
        let child_full = if full_path == "/" {
            format!("/{}", entry.name)
        } else {
            format!("{full_path}/{}", entry.name)
        };
        let child_name = format!("{dir_name}{}", entry.name);
        if entry.is_dir {
            if tar_add_dir(ctx, tar_data, &child_full, &child_name, verbose) != 0 {
                return 1;
            }
        } else if tar_add_file(ctx, tar_data, &child_full, &child_name, verbose) != 0 {
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

fn tar_extract(
    ctx: &mut UtilContext<'_>,
    archive_path: &str,
    base_dir: &str,
    gzipped: bool,
    verbose: bool,
) -> i32 {
    let full_archive = resolve_path(ctx.cwd, archive_path);
    let archive_data = match ctx.fs.open(&full_archive, OpenOptions::read()) {
        Ok(h) => match ctx.fs.read_file(h) {
            Ok(d) => {
                ctx.fs.close(h);
                d
            }
            Err(e) => {
                ctx.fs.close(h);
                emit_error(ctx.output, "tar", archive_path, &e);
                return 1;
            }
        },
        Err(e) => {
            emit_error(ctx.output, "tar", archive_path, &e);
            return 1;
        }
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

        // Check for end-of-archive (all zeros)
        if header.iter().all(|&b| b == 0) {
            break;
        }

        // Parse name
        let name_end = header[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = String::from_utf8_lossy(&header[..name_end]).to_string();

        // Parse size (octal)
        let size_str = String::from_utf8_lossy(&header[124..136])
            .trim_matches('\0')
            .trim()
            .to_string();
        let Ok(size) = u64::from_str_radix(&size_str, 8).map(|v| v as usize) else {
            let msg = format!("tar: invalid size in header for '{name}'\n");
            ctx.output.stderr(msg.as_bytes());
            return 1;
        };

        // Parse typeflag
        let typeflag = header[156];

        pos += TAR_BLOCK_SIZE;

        if verbose {
            let msg = format!("{name}\n");
            ctx.output.stderr(msg.as_bytes());
        }

        let full = resolve_path(base_dir, &name);

        if typeflag == b'5' || name.ends_with('/') {
            // Directory
            if ctx.fs.create_dir(&full).is_err() && ctx.fs.stat(&full).is_err() {
                let msg = format!("tar: cannot create directory '{name}'\n");
                ctx.output.stderr(msg.as_bytes());
            }
        } else {
            // Regular file
            // Ensure parent directory exists
            if let Some(slash_pos) = full.rfind('/') {
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

            let end = (pos + size).min(tar_data.len());
            let file_data = &tar_data[pos..end];

            if write_file(ctx, "tar", &full, file_data) != 0 {
                return 1;
            }

            // Advance past data + padding
            let blocks = size.div_ceil(TAR_BLOCK_SIZE);
            pos += blocks * TAR_BLOCK_SIZE;
        }
    }

    0
}

// ---------------------------------------------------------------------------
// unzip — extract ZIP archives
// ---------------------------------------------------------------------------

pub(crate) fn util_unzip(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut list_only = false;
    let mut overwrite = false;
    let mut dest_dir: Option<String> = None;
    let mut quiet = false;

    while let Some(arg) = args.first() {
        match *arg {
            "-l" => {
                list_only = true;
                args = &args[1..];
            }
            "-o" => {
                overwrite = true;
                args = &args[1..];
            }
            "-d" if args.len() > 1 => {
                dest_dir = Some(args[1].to_string());
                args = &args[2..];
            }
            "-q" => {
                quiet = true;
                args = &args[1..];
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                let mut recognized = true;
                for ch in arg[1..].chars() {
                    match ch {
                        'l' => list_only = true,
                        'o' => overwrite = true,
                        'q' => quiet = true,
                        _ => {
                            recognized = false;
                            break;
                        }
                    }
                }
                if recognized {
                    args = &args[1..];
                } else {
                    break;
                }
            }
            _ => break,
        }
    }

    if args.is_empty() {
        ctx.output.stderr(b"unzip: missing archive operand\n");
        return 1;
    }

    let archive_path = args[0];

    // Check for -d after the filename
    let rest = &args[1..];
    if rest.len() >= 2 && rest[0] == "-d" {
        dest_dir = Some(rest[1].to_string());
    }

    let full_archive = resolve_path(ctx.cwd, archive_path);
    let archive_data = match ctx.fs.open(&full_archive, OpenOptions::read()) {
        Ok(h) => match ctx.fs.read_file(h) {
            Ok(d) => {
                ctx.fs.close(h);
                d
            }
            Err(e) => {
                ctx.fs.close(h);
                emit_error(ctx.output, "unzip", archive_path, &e);
                return 1;
            }
        },
        Err(e) => {
            emit_error(ctx.output, "unzip", archive_path, &e);
            return 1;
        }
    };

    let base_dir = if let Some(ref d) = dest_dir {
        let full = resolve_path(ctx.cwd, d);
        // Ensure directory exists
        let _ = ctx.fs.create_dir(&full);
        full
    } else {
        ctx.cwd.to_string()
    };

    if list_only && !quiet {
        ctx.output
            .stdout(b"  Length      Name\n---------  --------------------\n");
    }

    let mut pos = 0;
    let data = &archive_data;
    let mut total_files = 0u32;
    let mut total_size = 0u64;
    let mut status = 0;

    while pos + 30 <= data.len() {
        // Look for local file header signature: PK\x03\x04
        if &data[pos..pos + 4] != b"PK\x03\x04" {
            // Try to find next signature
            if let Some(next) = find_pk_signature(&data[pos..]) {
                pos += next;
                continue;
            }
            break;
        }

        // Parse local file header
        let compression = u16_le(&data[pos + 8..pos + 10]);
        let compressed_size = u32_le(&data[pos + 18..pos + 22]) as usize;
        let uncompressed_size = u32_le(&data[pos + 22..pos + 26]) as usize;
        let name_len = u16_le(&data[pos + 26..pos + 28]) as usize;
        let extra_len = u16_le(&data[pos + 28..pos + 30]) as usize;

        let name_start = pos + 30;
        if name_start + name_len > data.len() {
            break;
        }
        let name = String::from_utf8_lossy(&data[name_start..name_start + name_len]).to_string();

        let data_start = name_start + name_len + extra_len;

        if list_only {
            let line = format!("{uncompressed_size:>9}  {name}\n");
            ctx.output.stdout(line.as_bytes());
            total_files += 1;
            total_size += uncompressed_size as u64;
            pos = data_start + compressed_size;
            continue;
        }

        let is_directory = name.ends_with('/');

        if is_directory {
            let full = resolve_path(&base_dir, &name);
            let _ = ctx.fs.create_dir(&full);
            if !quiet {
                let msg = format!("   creating: {name}\n");
                ctx.output.stdout(msg.as_bytes());
            }
        } else if compression == 0 {
            // Stored (no compression)
            let end = (data_start + uncompressed_size).min(data.len());
            let file_data = &data[data_start..end];

            let full = resolve_path(&base_dir, &name);

            // Ensure parent directory exists
            if let Some(slash_pos) = full.rfind('/') {
                let parent = &full[..slash_pos];
                if !parent.is_empty() && ctx.fs.stat(parent).is_err() {
                    let _ = ctx.fs.create_dir(parent);
                }
            }

            // Check if file exists and overwrite flag
            if !overwrite && ctx.fs.stat(&full).is_ok() {
                if !quiet {
                    let msg = format!("unzip: {name}: already exists, skipping\n");
                    ctx.output.stderr(msg.as_bytes());
                }
            } else {
                if write_file(ctx, "unzip", &full, file_data) != 0 {
                    return 1;
                }
                if !quiet {
                    let msg = format!("  inflating: {name}\n");
                    ctx.output.stdout(msg.as_bytes());
                }
            }
        } else if compression == 8 {
            if !quiet {
                let msg = format!("unzip: {name}: deflate not supported in sandbox\n");
                ctx.output.stderr(msg.as_bytes());
            }
            status = 1;
        } else {
            if !quiet {
                let msg = format!("unzip: {name}: unsupported compression method {compression}\n");
                ctx.output.stderr(msg.as_bytes());
            }
            status = 1;
        }

        total_files += 1;
        total_size += uncompressed_size as u64;
        pos = data_start + compressed_size;
    }

    if list_only && !quiet {
        let footer =
            format!("---------  --------------------\n{total_size:>9}  {total_files} file(s)\n");
        ctx.output.stdout(footer.as_bytes());
    }

    status
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
    let full_archive = resolve_path(ctx.cwd, archive_path);
    let archive_data = match ctx.fs.open(&full_archive, OpenOptions::read()) {
        Ok(h) => match ctx.fs.read_file(h) {
            Ok(d) => {
                ctx.fs.close(h);
                d
            }
            Err(e) => {
                ctx.fs.close(h);
                emit_error(ctx.output, "tar", archive_path, &e);
                return 1;
            }
        },
        Err(e) => {
            emit_error(ctx.output, "tar", archive_path, &e);
            return 1;
        }
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
}
