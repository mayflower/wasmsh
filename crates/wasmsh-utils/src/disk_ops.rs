//! Disk utilities: du, df.

use wasmsh_fs::{MemoryFs, Vfs};

use crate::helpers::{child_path, emit_error, resolve_path};
use crate::UtilContext;

// ---------------------------------------------------------------------------
// du — estimate file space usage
// ---------------------------------------------------------------------------

#[allow(clippy::struct_excessive_bools)]
struct DuFlags {
    summary: bool,
    human: bool,
    all_files: bool,
    grand_total: bool,
    max_depth: Option<usize>,
}

fn parse_du_flags(ctx: &mut UtilContext<'_>, argv: &[&str]) -> Result<(DuFlags, usize), i32> {
    let mut args = &argv[1..];
    let mut flags = DuFlags {
        summary: false,
        human: false,
        all_files: false,
        grand_total: false,
        max_depth: None,
    };
    let mut consumed = 1;

    while let Some(arg) = args.first() {
        match *arg {
            "-s" => flags.summary = true,
            "-h" => flags.human = true,
            "-a" => flags.all_files = true,
            "-c" => flags.grand_total = true,
            "-d" if args.len() > 1 => {
                flags.max_depth = args[1].parse().ok();
                args = &args[2..];
                consumed += 2;
                continue;
            }
            _ if arg.starts_with("--max-depth=") => {
                flags.max_depth = arg
                    .strip_prefix("--max-depth=")
                    .and_then(|v| v.parse().ok());
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                for ch in arg[1..].chars() {
                    match ch {
                        's' => flags.summary = true,
                        'h' => flags.human = true,
                        'a' => flags.all_files = true,
                        'c' => flags.grand_total = true,
                        _ => {
                            let msg = format!("du: unknown option '-{ch}'\n");
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

pub(crate) fn util_du(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, consumed) = match parse_du_flags(ctx, argv) {
        Ok(v) => v,
        Err(status) => return status,
    };
    let args = &argv[consumed..];

    let targets: Vec<&str> = if args.is_empty() {
        vec!["."]
    } else {
        args.to_vec()
    };

    let mut total_size: u64 = 0;
    let mut status = 0;

    for target in &targets {
        let full = resolve_path(ctx.cwd, target);

        match ctx.fs.stat(&full) {
            Ok(meta) if meta.is_dir => {
                let (size, err) = du_walk(
                    ctx,
                    &full,
                    target,
                    0,
                    flags.max_depth,
                    flags.summary,
                    flags.all_files,
                    flags.human,
                );
                total_size += size;
                if err {
                    status = 1;
                }
            }
            Ok(meta) => {
                let size = meta.size;
                let blocks = to_blocks(size);
                let line = format!("{}\t{target}\n", format_blocks(blocks, flags.human));
                ctx.output.stdout(line.as_bytes());
                total_size += size;
            }
            Err(e) => {
                let msg = format!("du: cannot access '{target}': {e}\n");
                ctx.output.stderr(msg.as_bytes());
                status = 1;
            }
        }
    }

    if flags.grand_total {
        let blocks = to_blocks(total_size);
        let line = format!("{}\ttotal\n", format_blocks(blocks, flags.human));
        ctx.output.stdout(line.as_bytes());
    }

    status
}

/// Walk directory tree and return `(total_size_bytes, had_error)`. Prints lines as needed.
fn du_walk(
    ctx: &mut UtilContext<'_>,
    full_path: &str,
    display_path: &str,
    depth: usize,
    max_depth: Option<usize>,
    summary: bool,
    all_files: bool,
    human: bool,
) -> (u64, bool) {
    let mut total: u64 = 0;
    let mut had_error = false;

    let entries = match ctx.fs.read_dir(full_path) {
        Ok(e) => e,
        Err(e) => {
            emit_error(ctx.output, "du", display_path, &e);
            return (0, true);
        }
    };

    for entry in &entries {
        let child_full = child_path(full_path, &entry.name);
        let child_display = if display_path == "." {
            format!("./{}", entry.name)
        } else {
            format!("{display_path}/{}", entry.name)
        };

        if entry.is_dir {
            let (sub_size, sub_err) = du_walk(
                ctx,
                &child_full,
                &child_display,
                depth + 1,
                max_depth,
                summary,
                all_files,
                human,
            );
            total += sub_size;
            if sub_err {
                had_error = true;
            }
        } else {
            let size = match ctx.fs.stat(&child_full) {
                Ok(m) => m.size,
                Err(e) => {
                    emit_error(ctx.output, "du", &child_display, &e);
                    had_error = true;
                    0
                }
            };
            total += size;

            if all_files && !summary {
                let at_depth_limit = max_depth.is_some_and(|md| depth + 1 > md);
                if !at_depth_limit {
                    let blocks = to_blocks(size);
                    let line = format!("{}\t{child_display}\n", format_blocks(blocks, human));
                    ctx.output.stdout(line.as_bytes());
                }
            }
        }
    }

    // Print directory line
    if summary {
        // Only print top-level entries (depth 0)
        if depth == 0 {
            let blocks = to_blocks(total);
            let line = format!("{}\t{display_path}\n", format_blocks(blocks, human));
            ctx.output.stdout(line.as_bytes());
        }
    } else {
        let at_depth_limit = max_depth.is_some_and(|md| depth > md);
        if !at_depth_limit {
            let blocks = to_blocks(total);
            let line = format!("{}\t{display_path}\n", format_blocks(blocks, human));
            ctx.output.stdout(line.as_bytes());
        }
    }

    (total, had_error)
}

/// Convert bytes to 1K blocks (matching du default behavior).
fn to_blocks(bytes: u64) -> u64 {
    bytes.div_ceil(1024)
}

/// Format block count, optionally in human-readable form.
fn format_blocks(blocks: u64, human: bool) -> String {
    if !human {
        return blocks.to_string();
    }
    format_human(blocks * 1024)
}

/// Format bytes as human-readable string (K, M, G).
fn format_human(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        let val = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        format!("{val:.1}G")
    } else if bytes >= 1024 * 1024 {
        let val = bytes as f64 / (1024.0 * 1024.0);
        format!("{val:.1}M")
    } else if bytes >= 1024 {
        let val = bytes as f64 / 1024.0;
        format!("{val:.1}K")
    } else {
        format!("{bytes}")
    }
}

// ---------------------------------------------------------------------------
// df — report filesystem disk space usage
// ---------------------------------------------------------------------------

/// Default VFS capacity to report (64 MiB).
const VFS_CAPACITY: u64 = 64 * 1024 * 1024;

pub(crate) fn util_df(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut human = false;

    for arg in &argv[1..] {
        if *arg == "-h" {
            human = true;
        }
    }

    // Calculate total used space by walking the entire VFS
    let used = vfs_total_size(ctx.fs, "/");
    let total = VFS_CAPACITY;
    let avail = total.saturating_sub(used);
    let use_pct = if total > 0 {
        (used * 100 / total).min(100)
    } else {
        0
    };

    // Header
    if human {
        ctx.output
            .stdout(b"Filesystem      Size  Used Avail Use% Mounted on\n");
        let line = format!(
            "{:<15} {:>5} {:>5} {:>5} {:>3}% {}\n",
            "wasmsh-vfs",
            format_human(total),
            format_human(used),
            format_human(avail),
            use_pct,
            "/",
        );
        ctx.output.stdout(line.as_bytes());
    } else {
        ctx.output
            .stdout(b"Filesystem     1K-blocks  Used Available Use% Mounted on\n");
        let line = format!(
            "{:<14} {:>9} {:>5} {:>9} {:>3}% {}\n",
            "wasmsh-vfs",
            total / 1024,
            used / 1024,
            avail / 1024,
            use_pct,
            "/",
        );
        ctx.output.stdout(line.as_bytes());
    }

    0
}

/// Recursively sum all file sizes in the VFS.
fn vfs_total_size(fs: &MemoryFs, path: &str) -> u64 {
    let mut total: u64 = 0;
    if let Ok(entries) = fs.read_dir(path) {
        for entry in entries {
            let child = child_path(path, &entry.name);
            if entry.is_dir {
                total += vfs_total_size(fs, &child);
            } else if let Ok(meta) = fs.stat(&child) {
                total += meta.size;
            }
        }
    }
    total
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

    fn make_fs() -> MemoryFs {
        let mut fs = MemoryFs::new();
        fs.create_dir("/dir").unwrap();
        let h = fs.open("/dir/a.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"hello").unwrap(); // 5 bytes
        fs.close(h);
        let h = fs.open("/dir/b.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, &[0u8; 2048]).unwrap(); // 2048 bytes
        fs.close(h);
        fs
    }

    #[test]
    fn du_basic() {
        let mut fs = make_fs();
        let (status, out) = run_util(util_du, &["du", "/dir"], &mut fs, "/");
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("/dir"));
    }

    #[test]
    fn du_summary() {
        let mut fs = make_fs();
        let (status, out) = run_util(util_du, &["du", "-s", "/dir"], &mut fs, "/");
        assert_eq!(status, 0);
        let lines: Vec<&str> = out.stdout_str().trim().lines().collect();
        // Summary should have one line for the directory
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("/dir"));
    }

    #[test]
    fn du_all_files() {
        let mut fs = make_fs();
        let (status, out) = run_util(util_du, &["du", "-a", "/dir"], &mut fs, "/");
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("a.txt"));
        assert!(s.contains("b.txt"));
    }

    #[test]
    fn du_human_readable() {
        let mut fs = make_fs();
        let (status, out) = run_util(util_du, &["du", "-sh", "/dir"], &mut fs, "/");
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains('K') || s.contains('M'));
    }

    #[test]
    fn du_grand_total() {
        let mut fs = make_fs();
        let (status, out) = run_util(util_du, &["du", "-sc", "/dir"], &mut fs, "/");
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("total"));
    }

    #[test]
    fn du_single_file() {
        let mut fs = make_fs();
        let (status, out) = run_util(util_du, &["du", "/dir/a.txt"], &mut fs, "/");
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("/dir/a.txt"));
    }

    #[test]
    fn df_basic() {
        let mut fs = make_fs();
        let (status, out) = run_util(util_df, &["df"], &mut fs, "/");
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("wasmsh-vfs"));
        assert!(s.contains("Mounted on"));
    }

    #[test]
    fn df_human() {
        let mut fs = make_fs();
        let (status, out) = run_util(util_df, &["df", "-h"], &mut fs, "/");
        assert_eq!(status, 0);
        let s = out.stdout_str();
        assert!(s.contains("wasmsh-vfs"));
        assert!(s.contains('M')); // 64M capacity
    }

    #[test]
    fn format_human_values() {
        assert_eq!(format_human(500), "500");
        assert_eq!(format_human(1024), "1.0K");
        assert_eq!(format_human(1536), "1.5K");
        assert_eq!(format_human(1_048_576), "1.0M");
        assert_eq!(format_human(1_073_741_824), "1.0G");
    }
}
