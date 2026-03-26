//! File utilities: cat, ls, mkdir, rm, touch, mv, cp, ln, readlink, realpath, stat, find, chmod,
//! mktemp.

use wasmsh_fs::{MemoryFs, OpenOptions, Vfs};

use crate::helpers::{
    child_path, copy_file_contents, emit_error, require_args, resolve_path, simple_glob_match,
    XorShift64,
};
use crate::{UtilContext, UtilOutput};

pub(crate) fn util_cat(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    if argv.len() < 2 {
        // No file arguments — read from stdin if available
        if let Some(data) = ctx.stdin {
            ctx.output.stdout(data);
            return 0;
        }
        ctx.output.stderr(b"cat: missing operand\n");
        return 1;
    }
    let mut status = 0;
    for path in &argv[1..] {
        let full = resolve_path(ctx.cwd, path);
        match ctx.fs.open(&full, OpenOptions::read()) {
            Ok(h) => {
                match ctx.fs.read_file(h) {
                    Ok(data) => ctx.output.stdout(&data),
                    Err(e) => {
                        emit_error(ctx.output, "cat", path, &e);
                        status = 1;
                    }
                }
                ctx.fs.close(h);
            }
            Err(e) => {
                emit_error(ctx.output, "cat", path, &e);
                status = 1;
            }
        }
    }
    status
}

pub(crate) fn util_ls(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let args: Vec<&str> = argv[1..]
        .iter()
        .copied()
        .filter(|a| !a.starts_with('-'))
        .collect();
    if args.is_empty() {
        return i32::from(ls_emit_dir(ctx, ctx.cwd, &resolve_path(ctx.cwd, ctx.cwd)).is_err());
    } else {
        let mut status = 0;
        for path in &args {
            if ls_path(ctx, path) != 0 {
                status = 1;
            }
        }
        status
    }
}

fn ls_emit_entries(
    ctx: &mut UtilContext<'_>,
    entries: impl IntoIterator<Item = wasmsh_fs::DirEntry>,
) {
    for entry in entries {
        ctx.output.stdout(entry.name.as_bytes());
        ctx.output.stdout(b"\n");
    }
}

fn ls_emit_dir(ctx: &mut UtilContext<'_>, display: &str, full: &str) -> Result<(), ()> {
    match ctx.fs.read_dir(full) {
        Ok(entries) => {
            ls_emit_entries(ctx, entries);
            Ok(())
        }
        Err(e) => {
            emit_error(ctx.output, "ls", display, &e);
            Err(())
        }
    }
}

fn ls_path(ctx: &mut UtilContext<'_>, path: &str) -> i32 {
    let full = resolve_path(ctx.cwd, path);
    match ctx.fs.stat(&full) {
        Ok(meta) if meta.is_dir => i32::from(ls_emit_dir(ctx, path, &full).is_err()),
        Ok(_) => {
            ctx.output.stdout(path.as_bytes());
            ctx.output.stdout(b"\n");
            0
        }
        Err(e) => {
            emit_error(ctx.output, "ls", path, &e);
            1
        }
    }
}

pub(crate) fn util_mkdir(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    if !require_args(argv, 2, ctx.output) {
        return 1;
    }
    let mut status = 0;
    for path in &argv[1..] {
        let full = resolve_path(ctx.cwd, path);
        if let Err(e) = ctx.fs.create_dir(&full) {
            emit_error(ctx.output, "mkdir", path, &e);
            status = 1;
        }
    }
    status
}

struct RmFlags {
    recursive: bool,
    force: bool,
}

fn parse_rm_flags(ctx: &mut UtilContext<'_>, argv: &[&str]) -> Result<(RmFlags, usize), i32> {
    let mut args = &argv[1..];
    let mut flags = RmFlags {
        recursive: false,
        force: false,
    };
    let mut consumed = 1;

    while let Some(arg) = args.first() {
        if *arg == "--" {
            consumed += 1;
            break;
        } else if arg.starts_with('-') && arg.len() > 1 {
            for ch in arg[1..].chars() {
                match ch {
                    'r' | 'R' => flags.recursive = true,
                    'f' => flags.force = true,
                    _ => {
                        let msg = format!("rm: unknown option '{ch}'\n");
                        ctx.output.stderr(msg.as_bytes());
                        return Err(1);
                    }
                }
            }
            args = &args[1..];
            consumed += 1;
        } else {
            break;
        }
    }
    Ok((flags, consumed))
}

fn rm_one(ctx: &mut UtilContext<'_>, path: &str, flags: &RmFlags) -> i32 {
    let full = resolve_path(ctx.cwd, path);
    match ctx.fs.stat(&full) {
        Ok(meta) if meta.is_dir => {
            if !flags.recursive {
                let msg = format!("rm: cannot remove '{path}': Is a directory\n");
                ctx.output.stderr(msg.as_bytes());
                return 1;
            }
            if rm_recursive(ctx.fs, &full).is_err() {
                emit_error(ctx.output, "rm", path, &"removal failed");
                return 1;
            }
            0
        }
        Ok(_) => {
            if let Err(e) = ctx.fs.remove_file(&full) {
                if !flags.force {
                    emit_error(ctx.output, "rm", path, &e);
                    return 1;
                }
            }
            0
        }
        Err(e) => {
            if !flags.force {
                emit_error(ctx.output, "rm", path, &e);
                return 1;
            }
            0
        }
    }
}

pub(crate) fn util_rm(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, consumed) = match parse_rm_flags(ctx, argv) {
        Ok(v) => v,
        Err(status) => return status,
    };
    let args = &argv[consumed..];

    if args.is_empty() && !flags.force {
        ctx.output.stderr(b"rm: missing operand\n");
        return 1;
    }

    let mut status = 0;
    for path in args {
        if rm_one(ctx, path, &flags) != 0 {
            status = 1;
        }
    }
    status
}

/// Recursively remove a directory and all its contents.
fn rm_recursive(fs: &mut MemoryFs, path: &str) -> Result<(), ()> {
    // First remove all children
    if let Ok(entries) = fs.read_dir(path) {
        for entry in entries {
            let child = child_path(path, &entry.name);
            if let Ok(meta) = fs.stat(&child) {
                if meta.is_dir {
                    rm_recursive(fs, &child)?;
                } else {
                    fs.remove_file(&child).map_err(|_| ())?;
                }
            }
        }
    }
    fs.remove_dir(path).map_err(|_| ())
}

pub(crate) fn util_touch(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    if !require_args(argv, 2, ctx.output) {
        return 1;
    }
    let mut status = 0;
    for path in &argv[1..] {
        let full = resolve_path(ctx.cwd, path);
        match ctx.fs.open(&full, OpenOptions::append()) {
            Ok(h) => ctx.fs.close(h),
            Err(e) => {
                emit_error(ctx.output, "touch", path, &e);
                status = 1;
            }
        }
    }
    status
}

pub(crate) fn util_mv(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    if !require_args(argv, 3, ctx.output) {
        return 1;
    }
    let src = resolve_path(ctx.cwd, argv[1]);
    let dst = resolve_path(ctx.cwd, argv[2]);
    if let Err(e) = copy_file_contents(ctx.fs, &src, &dst) {
        emit_error(ctx.output, "mv", argv[1], &e);
        return 1;
    }
    if let Err(e) = ctx.fs.remove_file(&src) {
        emit_error(ctx.output, "mv", argv[1], &e);
        return 1;
    }
    0
}

pub(crate) fn util_cp(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    if !require_args(argv, 3, ctx.output) {
        return 1;
    }
    let src = resolve_path(ctx.cwd, argv[1]);
    let dst = resolve_path(ctx.cwd, argv[2]);
    if let Err(e) = copy_file_contents(ctx.fs, &src, &dst) {
        emit_error(ctx.output, "cp", argv[1], &e);
        return 1;
    }
    0
}

pub(crate) fn util_ln(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    // VFS doesn't support real links, so ln creates a copy
    if !require_args(argv, 3, ctx.output) {
        return 1;
    }
    let src = resolve_path(ctx.cwd, argv[argv.len() - 2]);
    let dst = resolve_path(ctx.cwd, argv[argv.len() - 1]);
    if let Err(e) = copy_file_contents(ctx.fs, &src, &dst) {
        emit_error(ctx.output, "ln", argv[argv.len() - 2], &e);
        return 1;
    }
    0
}

pub(crate) fn util_readlink(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    if !require_args(argv, 2, ctx.output) {
        return 1;
    }
    // VFS has no symlinks — just output the canonical path
    let full = resolve_path(ctx.cwd, argv[1]);
    ctx.output.stdout(full.as_bytes());
    ctx.output.stdout(b"\n");
    0
}

pub(crate) fn util_realpath(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    if !require_args(argv, 2, ctx.output) {
        return 1;
    }
    let full = resolve_path(ctx.cwd, argv[1]);
    ctx.output.stdout(full.as_bytes());
    ctx.output.stdout(b"\n");
    0
}

pub(crate) fn util_stat(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    if !require_args(argv, 2, ctx.output) {
        return 1;
    }
    let full = resolve_path(ctx.cwd, argv[1]);
    match ctx.fs.stat(&full) {
        Ok(meta) => {
            let kind = if meta.is_dir {
                "directory"
            } else {
                "regular file"
            };
            let out = format!(
                "  File: {}\n  Size: {}\n  Type: {kind}\n",
                argv[1], meta.size
            );
            ctx.output.stdout(out.as_bytes());
            0
        }
        Err(e) => {
            emit_error(ctx.output, "stat", argv[1], &e);
            1
        }
    }
}

struct FindFilters<'a> {
    name_pattern: Option<&'a str>,
    type_filter: Option<&'a str>,
}

fn parse_find_args<'a>(argv: &'a [&'a str]) -> (&'a str, FindFilters<'a>) {
    let mut args = &argv[1..];
    let dir = if !args.is_empty() && !args[0].starts_with('-') {
        let d = args[0];
        args = &args[1..];
        d
    } else {
        "."
    };

    let mut filters = FindFilters {
        name_pattern: None,
        type_filter: None,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "-name" if i + 1 < args.len() => {
                filters.name_pattern = Some(args[i + 1]);
                i += 2;
            }
            "-type" if i + 1 < args.len() => {
                filters.type_filter = Some(args[i + 1]);
                i += 2;
            }
            _ => i += 1,
        }
    }
    (dir, filters)
}

fn find_name_matches(pat: &str, name: &str) -> bool {
    if pat.contains('*') || pat.contains('?') {
        simple_glob_match(pat, name)
    } else {
        name == pat
    }
}

fn find_type_matches(filter: &str, is_dir: bool) -> bool {
    match filter {
        "f" => !is_dir,
        "d" => is_dir,
        _ => true,
    }
}

fn walk_find(fs: &MemoryFs, path: &str, filters: &FindFilters<'_>, output: &mut dyn UtilOutput) {
    let Ok(entries) = fs.read_dir(path) else {
        return;
    };
    for entry in entries {
        let child = child_path(path, &entry.name);
        let name_ok = filters
            .name_pattern
            .is_none_or(|p| find_name_matches(p, &entry.name));
        let type_ok = filters
            .type_filter
            .is_none_or(|t| find_type_matches(t, entry.is_dir));
        if name_ok && type_ok {
            output.stdout(child.as_bytes());
            output.stdout(b"\n");
        }
        if entry.is_dir {
            walk_find(fs, &child, filters, output);
        }
    }
}

pub(crate) fn util_find(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (dir, filters) = parse_find_args(argv);
    let full = resolve_path(ctx.cwd, dir);
    walk_find(ctx.fs, &full, &filters, ctx.output);
    0
}

pub(crate) fn util_chmod(_ctx: &mut UtilContext<'_>, _argv: &[&str]) -> i32 {
    // VFS has no permission model — chmod is a no-op that succeeds
    0
}

/// Global counter for seeding the PRNG with varying values.
static MKTEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn parse_mktemp_flags<'a>(
    ctx: &mut UtilContext<'_>,
    argv: &'a [&'a str],
) -> Result<(bool, &'a str), i32> {
    let mut args = &argv[1..];
    let mut make_dir = false;

    while let Some(arg) = args.first() {
        if *arg == "-d" {
            make_dir = true;
            args = &args[1..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            let msg = format!("mktemp: unknown option '{arg}'\n");
            ctx.output.stderr(msg.as_bytes());
            return Err(1);
        } else {
            break;
        }
    }

    let template = if args.is_empty() {
        "/tmp/tmp.XXXXXXXXXX"
    } else {
        args[0]
    };
    Ok((make_dir, template))
}

fn mktemp_gen_suffix(rng: &mut XorShift64, x_count: usize) -> String {
    let alphabet = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    (0..x_count)
        .map(|_| {
            let idx = (rng.next() % alphabet.len() as u64) as usize;
            alphabet[idx] as char
        })
        .collect()
}

fn mktemp_create_file(ctx: &mut UtilContext<'_>, full: &str, path: &str) -> Result<(), i32> {
    match ctx.fs.open(full, OpenOptions::write()) {
        Ok(h) => {
            if let Err(e) = ctx.fs.write_file(h, &[]) {
                ctx.fs.close(h);
                emit_error(ctx.output, "mktemp", path, &e);
                return Err(1);
            }
            ctx.fs.close(h);
            Ok(())
        }
        Err(e) => {
            emit_error(ctx.output, "mktemp", path, &e);
            Err(1)
        }
    }
}

pub(crate) fn util_mktemp(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (make_dir, template) = match parse_mktemp_flags(ctx, argv) {
        Ok(v) => v,
        Err(status) => return status,
    };

    let x_count = template.chars().rev().take_while(|&c| c == 'X').count();
    if x_count < 3 {
        ctx.output.stderr(b"mktemp: too few X's in template\n");
        return 1;
    }

    let prefix = &template[..template.len() - x_count];

    if ctx.fs.stat("/tmp").is_err() {
        let _ = ctx.fs.create_dir("/tmp");
    }

    let seed = MKTEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut rng = XorShift64::new(seed.wrapping_mul(0x517C_C1B7_2722_0A95));

    for _ in 0..10 {
        let suffix = mktemp_gen_suffix(&mut rng, x_count);
        let path = format!("{prefix}{suffix}");
        let full = resolve_path(ctx.cwd, &path);

        if ctx.fs.stat(&full).is_ok() {
            continue;
        }

        if make_dir {
            if let Err(e) = ctx.fs.create_dir(&full) {
                emit_error(ctx.output, "mktemp", &path, &e);
                return 1;
            }
        } else if mktemp_create_file(ctx, &full, &path).is_err() {
            return 1;
        }

        ctx.output.stdout(full.as_bytes());
        ctx.output.stdout(b"\n");
        return 0;
    }

    ctx.output.stderr(b"mktemp: failed to create unique name\n");
    1
}
