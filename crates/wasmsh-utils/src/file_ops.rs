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
        // No arguments: list cwd
        let full = resolve_path(ctx.cwd, ctx.cwd);
        match ctx.fs.read_dir(&full) {
            Ok(entries) => {
                for entry in entries {
                    ctx.output.stdout(entry.name.as_bytes());
                    ctx.output.stdout(b"\n");
                }
                0
            }
            Err(e) => {
                emit_error(ctx.output, "ls", ctx.cwd, &e);
                1
            }
        }
    } else {
        let mut status = 0;
        for path in &args {
            let full = resolve_path(ctx.cwd, path);
            match ctx.fs.stat(&full) {
                Ok(meta) if meta.is_dir => {
                    // Directory argument: list its contents
                    match ctx.fs.read_dir(&full) {
                        Ok(entries) => {
                            for entry in entries {
                                ctx.output.stdout(entry.name.as_bytes());
                                ctx.output.stdout(b"\n");
                            }
                        }
                        Err(e) => {
                            emit_error(ctx.output, "ls", path, &e);
                            status = 1;
                        }
                    }
                }
                Ok(_) => {
                    // File argument: just print the name
                    ctx.output.stdout(path.as_bytes());
                    ctx.output.stdout(b"\n");
                }
                Err(e) => {
                    emit_error(ctx.output, "ls", path, &e);
                    status = 1;
                }
            }
        }
        status
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

pub(crate) fn util_rm(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut recursive = false;
    let mut force = false;

    // Parse flags
    while let Some(arg) = args.first() {
        if *arg == "--" {
            args = &args[1..];
            break;
        } else if arg.starts_with('-') && arg.len() > 1 {
            for ch in arg[1..].chars() {
                match ch {
                    'r' | 'R' => recursive = true,
                    'f' => force = true,
                    _ => {
                        let msg = format!("rm: unknown option '{ch}'\n");
                        ctx.output.stderr(msg.as_bytes());
                        return 1;
                    }
                }
            }
            args = &args[1..];
        } else {
            break;
        }
    }

    if args.is_empty() && !force {
        ctx.output.stderr(b"rm: missing operand\n");
        return 1;
    }

    let mut status = 0;
    for path in args {
        let full = resolve_path(ctx.cwd, path);
        match ctx.fs.stat(&full) {
            Ok(meta) if meta.is_dir => {
                if recursive {
                    if rm_recursive(ctx.fs, &full).is_err() {
                        emit_error(ctx.output, "rm", path, &"removal failed");
                        status = 1;
                    }
                } else {
                    let msg = format!("rm: cannot remove '{path}': Is a directory\n");
                    ctx.output.stderr(msg.as_bytes());
                    status = 1;
                }
            }
            Ok(_) => {
                if let Err(e) = ctx.fs.remove_file(&full) {
                    if !force {
                        emit_error(ctx.output, "rm", path, &e);
                        status = 1;
                    }
                }
            }
            Err(e) => {
                if !force {
                    emit_error(ctx.output, "rm", path, &e);
                    status = 1;
                }
            }
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

pub(crate) fn util_find(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    fn walk_find(
        fs: &MemoryFs,
        path: &str,
        name_pat: Option<&str>,
        type_f: Option<&str>,
        output: &mut dyn UtilOutput,
    ) {
        if let Ok(entries) = fs.read_dir(path) {
            for entry in entries {
                let child = child_path(path, &entry.name);
                let name_ok = name_pat.is_none_or(|p| {
                    if p.contains('*') || p.contains('?') {
                        simple_glob_match(p, &entry.name)
                    } else {
                        entry.name == p
                    }
                });
                let type_ok = type_f.is_none_or(|t| match t {
                    "f" => !entry.is_dir,
                    "d" => entry.is_dir,
                    _ => true,
                });
                if name_ok && type_ok {
                    output.stdout(child.as_bytes());
                    output.stdout(b"\n");
                }
                if entry.is_dir {
                    walk_find(fs, &child, name_pat, type_f, output);
                }
            }
        }
    }

    let mut args = &argv[1..];
    let dir = if !args.is_empty() && !args[0].starts_with('-') {
        let d = args[0];
        args = &args[1..];
        d
    } else {
        "."
    };
    let full = resolve_path(ctx.cwd, dir);

    let mut name_pattern: Option<&str> = None;
    let mut type_filter: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "-name" if i + 1 < args.len() => {
                name_pattern = Some(args[i + 1]);
                i += 2;
            }
            "-type" if i + 1 < args.len() => {
                type_filter = Some(args[i + 1]);
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    walk_find(ctx.fs, &full, name_pattern, type_filter, ctx.output);
    0
}

pub(crate) fn util_chmod(_ctx: &mut UtilContext<'_>, _argv: &[&str]) -> i32 {
    // VFS has no permission model — chmod is a no-op that succeeds
    0
}

/// Global counter for seeding the PRNG with varying values.
static MKTEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub(crate) fn util_mktemp(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut make_dir = false;

    // Parse flags
    while let Some(arg) = args.first() {
        if *arg == "-d" {
            make_dir = true;
            args = &args[1..];
        } else if arg.starts_with('-') && arg.len() > 1 {
            let msg = format!("mktemp: unknown option '{arg}'\n");
            ctx.output.stderr(msg.as_bytes());
            return 1;
        } else {
            break;
        }
    }

    let template = if args.is_empty() {
        "/tmp/tmp.XXXXXXXXXX"
    } else {
        args[0]
    };

    // Count trailing X's in the template
    let x_count = template.chars().rev().take_while(|&c| c == 'X').count();
    if x_count < 3 {
        ctx.output.stderr(b"mktemp: too few X's in template\n");
        return 1;
    }

    let prefix = &template[..template.len() - x_count];

    // Ensure /tmp exists
    if ctx.fs.stat("/tmp").is_err() {
        let _ = ctx.fs.create_dir("/tmp");
    }

    // Generate random suffix using XorShift PRNG
    let seed = MKTEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut rng = XorShift64::new(seed.wrapping_mul(0x517C_C1B7_2722_0A95));
    let alphabet = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

    // Try a few times in case of collision
    for _ in 0..10 {
        let suffix: String = (0..x_count)
            .map(|_| {
                let idx = (rng.next() % alphabet.len() as u64) as usize;
                alphabet[idx] as char
            })
            .collect();
        let path = format!("{prefix}{suffix}");
        let full = resolve_path(ctx.cwd, &path);

        // Check if path already exists
        if ctx.fs.stat(&full).is_ok() {
            continue;
        }

        if make_dir {
            if let Err(e) = ctx.fs.create_dir(&full) {
                emit_error(ctx.output, "mktemp", &path, &e);
                return 1;
            }
        } else {
            match ctx.fs.open(&full, OpenOptions::write()) {
                Ok(h) => {
                    // Create empty file
                    if let Err(e) = ctx.fs.write_file(h, &[]) {
                        ctx.fs.close(h);
                        emit_error(ctx.output, "mktemp", &path, &e);
                        return 1;
                    }
                    ctx.fs.close(h);
                }
                Err(e) => {
                    emit_error(ctx.output, "mktemp", &path, &e);
                    return 1;
                }
            }
        }

        ctx.output.stdout(full.as_bytes());
        ctx.output.stdout(b"\n");
        return 0;
    }

    ctx.output.stderr(b"mktemp: failed to create unique name\n");
    1
}
