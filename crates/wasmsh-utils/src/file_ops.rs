//! File utilities: cat, ls, mkdir, rm, touch, mv, cp, ln, readlink, realpath, stat, find, chmod.

use wasmsh_fs::{MemoryFs, OpenOptions, Vfs};

use crate::helpers::*;
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
    let path = if argv.len() < 2 { ctx.cwd } else { argv[1] };
    let full = resolve_path(ctx.cwd, path);
    match ctx.fs.read_dir(&full) {
        Ok(entries) => {
            for entry in entries {
                ctx.output.stdout(entry.name.as_bytes());
                ctx.output.stdout(b"\n");
            }
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

pub(crate) fn util_rm(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    if !require_args(argv, 2, ctx.output) {
        return 1;
    }
    let mut status = 0;
    for path in &argv[1..] {
        let full = resolve_path(ctx.cwd, path);
        if let Err(e) = ctx.fs.remove_file(&full) {
            emit_error(ctx.output, "rm", path, &e);
            status = 1;
        }
    }
    status
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
    let _ = ctx.fs.remove_file(&src);
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

    fn walk_find(
        fs: &MemoryFs,
        path: &str,
        name_pat: Option<&str>,
        type_f: Option<&str>,
        output: &mut dyn UtilOutput,
    ) {
        if let Ok(entries) = fs.read_dir(path) {
            for entry in entries {
                let child = if path == "/" {
                    format!("/{}", entry.name)
                } else {
                    format!("{}/{}", path, entry.name)
                };
                let name_ok = name_pat.map_or(true, |p| {
                    if p.contains('*') || p.contains('?') {
                        simple_glob_match(p, &entry.name)
                    } else {
                        entry.name == p
                    }
                });
                let type_ok = type_f.map_or(true, |t| match t {
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

    walk_find(ctx.fs, &full, name_pattern, type_filter, ctx.output);
    0
}

pub(crate) fn util_chmod(_ctx: &mut UtilContext<'_>, _argv: &[&str]) -> i32 {
    // VFS has no permission model — chmod is a no-op that succeeds
    0
}
