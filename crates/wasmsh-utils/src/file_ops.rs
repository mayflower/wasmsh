//! File utilities: cat, ls, mkdir, rm, touch, mv, cp, ln, readlink, realpath, stat, find, chmod,
//! mktemp.

use std::fmt::Write;

use wasmsh_fs::{BackendFs, OpenOptions, Vfs};

use crate::helpers::{
    child_path, copy_file_contents, emit_error, require_args, resolve_path, simple_glob_match,
    XorShift64,
};
use crate::{UtilContext, UtilOutput};

#[allow(clippy::struct_excessive_bools)]
struct CatFlags {
    number_all: bool,
    number_nonblank: bool,
    squeeze_blank: bool,
    show_ends: bool,
    show_tabs: bool,
}

fn parse_cat_flags<'a>(argv: &'a [&'a str]) -> (CatFlags, Vec<&'a str>) {
    let mut flags = CatFlags {
        number_all: false,
        number_nonblank: false,
        squeeze_blank: false,
        show_ends: false,
        show_tabs: false,
    };
    let mut files = Vec::new();
    for arg in &argv[1..] {
        if arg.starts_with('-') && arg.len() > 1 && *arg != "--" {
            for ch in arg[1..].chars() {
                match ch {
                    'n' => flags.number_all = true,
                    'b' => flags.number_nonblank = true,
                    's' => flags.squeeze_blank = true,
                    'E' | 'e' => flags.show_ends = true,
                    'T' | 't' => flags.show_tabs = true,
                    'A' => {
                        flags.show_ends = true;
                        flags.show_tabs = true;
                    }
                    _ => {} // 'v' etc. accepted, no-op in VFS
                }
            }
        } else if *arg != "--" {
            files.push(*arg);
        }
    }
    if flags.number_nonblank {
        flags.number_all = false;
    }
    (flags, files)
}

fn cat_has_flags(f: &CatFlags) -> bool {
    f.number_all || f.number_nonblank || f.squeeze_blank || f.show_ends || f.show_tabs
}

fn cat_emit_lines(output: &mut dyn UtilOutput, text: &str, flags: &CatFlags, line_num: &mut usize) {
    let mut prev_blank = false;
    for line in text.split('\n') {
        let is_blank = line.is_empty();
        if flags.squeeze_blank && is_blank && prev_blank {
            continue;
        }
        prev_blank = is_blank;

        if (flags.number_nonblank && !is_blank) || flags.number_all {
            *line_num += 1;
            let prefix = format!("{:>6}\t", *line_num);
            output.stdout(prefix.as_bytes());
        }

        if flags.show_tabs {
            output.stdout(line.replace('\t', "^I").as_bytes());
        } else {
            output.stdout(line.as_bytes());
        }

        if flags.show_ends {
            output.stdout(b"$");
        }
        output.stdout(b"\n");
    }
}

pub(crate) fn util_cat(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, files) = parse_cat_flags(argv);

    if files.is_empty() {
        if let Some(data) = ctx.stdin {
            if !cat_has_flags(&flags) {
                ctx.output.stdout(data);
            } else {
                let text = String::from_utf8_lossy(data);
                let trimmed = text.strip_suffix('\n').unwrap_or(&text);
                let mut line_num = 0usize;
                cat_emit_lines(ctx.output, trimmed, &flags, &mut line_num);
            }
            return 0;
        }
        ctx.output.stderr(b"cat: missing operand\n");
        return 1;
    }

    let mut status = 0;
    let mut line_num = 0usize;
    for path in &files {
        let full = resolve_path(ctx.cwd, path);
        match ctx.fs.open(&full, OpenOptions::read()) {
            Ok(h) => {
                match ctx.fs.read_file(h) {
                    Ok(data) => {
                        if !cat_has_flags(&flags) {
                            ctx.output.stdout(&data);
                        } else {
                            let text = String::from_utf8_lossy(&data);
                            let trimmed = text.strip_suffix('\n').unwrap_or(&text);
                            cat_emit_lines(ctx.output, trimmed, &flags, &mut line_num);
                        }
                    }
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

#[allow(clippy::struct_excessive_bools)]
struct LsFlags {
    long: bool,
    all: bool,
    recursive: bool,
    reverse: bool,
    sort_size: bool,
    dir_only: bool,
    human: bool,
    classify: bool,
}

fn parse_ls_flags<'a>(argv: &'a [&'a str]) -> (LsFlags, Vec<&'a str>) {
    let mut flags = LsFlags {
        long: false,
        all: false,
        recursive: false,
        reverse: false,
        sort_size: false,
        dir_only: false,
        human: false,
        classify: false,
    };
    let mut paths = Vec::new();
    for arg in &argv[1..] {
        if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") {
            for ch in arg[1..].chars() {
                match ch {
                    'l' => flags.long = true,
                    'a' => flags.all = true,
                    'R' => flags.recursive = true,
                    'h' => flags.human = true,
                    'r' => flags.reverse = true,
                    'S' => flags.sort_size = true,
                    'd' => flags.dir_only = true,
                    'F' => flags.classify = true,
                    // '1' (already one-per-line), 't' (no real mtime in VFS),
                    // 'i' (inode, ignore) — all accepted as no-ops
                    _ => {}
                }
            }
        } else if arg.starts_with("--") {
            // accept --color etc.
        } else {
            paths.push(*arg);
        }
    }
    (flags, paths)
}

fn ls_human_size(size: u64) -> String {
    if size >= 1_073_741_824 {
        format!("{:.1}G", size as f64 / 1_073_741_824.0)
    } else if size >= 1_048_576 {
        format!("{:.1}M", size as f64 / 1_048_576.0)
    } else if size >= 1024 {
        format!("{:.1}K", size as f64 / 1024.0)
    } else {
        format!("{size}")
    }
}

struct LsEntry {
    name: String,
    is_dir: bool,
    size: u64,
}

fn ls_collect_entries(fs: &mut BackendFs, dir: &str, flags: &LsFlags) -> Result<Vec<LsEntry>, ()> {
    let entries = fs.read_dir(dir).map_err(|_| ())?;
    let mut result: Vec<LsEntry> = entries
        .into_iter()
        .filter(|e| flags.all || !e.name.starts_with('.'))
        .map(|e| {
            let child = child_path(dir, &e.name);
            let size = fs.stat(&child).map(|m| m.size).unwrap_or(0);
            LsEntry {
                name: e.name,
                is_dir: e.is_dir,
                size,
            }
        })
        .collect();
    if flags.sort_size {
        result.sort_by(|a, b| b.size.cmp(&a.size));
    } else {
        result.sort_by(|a, b| a.name.cmp(&b.name));
    }
    if flags.reverse {
        result.reverse();
    }
    Ok(result)
}

fn ls_emit_entry(output: &mut dyn UtilOutput, e: &LsEntry, flags: &LsFlags) {
    if flags.long {
        let mode = if e.is_dir { "drwxr-xr-x" } else { "-rw-r--r--" };
        let sz = if flags.human {
            ls_human_size(e.size)
        } else {
            format!("{}", e.size)
        };
        let suffix = if flags.classify && e.is_dir { "/" } else { "" };
        let line = format!(
            "{mode}  1 user user {sz:>5} Jan  1 00:00 {}{suffix}\n",
            e.name
        );
        output.stdout(line.as_bytes());
    } else {
        let suffix = if flags.classify && e.is_dir { "/" } else { "" };
        output.stdout(e.name.as_bytes());
        output.stdout(suffix.as_bytes());
        output.stdout(b"\n");
    }
}

fn ls_dir(
    ctx: &mut UtilContext<'_>,
    display: &str,
    full: &str,
    flags: &LsFlags,
    show_header: bool,
) -> i32 {
    if show_header {
        let hdr = format!("{display}:\n");
        ctx.output.stdout(hdr.as_bytes());
    }
    let Ok(entries) = ls_collect_entries(ctx.fs, full, flags) else {
        emit_error(ctx.output, "ls", display, &"cannot open directory");
        return 1;
    };
    if flags.long {
        let total = format!("total {}\n", entries.iter().map(|e| e.size).sum::<u64>());
        ctx.output.stdout(total.as_bytes());
    }
    for e in &entries {
        ls_emit_entry(ctx.output, e, flags);
    }
    if flags.recursive {
        for e in &entries {
            if e.is_dir && e.name != "." && e.name != ".." {
                let child_full = child_path(full, &e.name);
                let child_display = if display == "." {
                    format!("./{}", e.name)
                } else {
                    format!("{}/{}", display, e.name)
                };
                ctx.output.stdout(b"\n");
                ls_dir(ctx, &child_display, &child_full, flags, true);
            }
        }
    }
    0
}

pub(crate) fn util_ls(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, paths) = parse_ls_flags(argv);
    let targets: Vec<&str> = if paths.is_empty() { vec!["."] } else { paths };
    let multi = targets.len() > 1;

    let mut status = 0;
    for (i, path) in targets.iter().enumerate() {
        let full = resolve_path(ctx.cwd, path);
        if flags.dir_only {
            let e = LsEntry {
                name: path.to_string(),
                is_dir: true,
                size: 0,
            };
            ls_emit_entry(ctx.output, &e, &flags);
            continue;
        }
        match ctx.fs.stat(&full) {
            Ok(meta) if meta.is_dir => {
                if i > 0 && multi {
                    ctx.output.stdout(b"\n");
                }
                if ls_dir(ctx, path, &full, &flags, multi || flags.recursive) != 0 {
                    status = 1;
                }
            }
            Ok(meta) => {
                let e = LsEntry {
                    name: path.to_string(),
                    is_dir: false,
                    size: meta.size,
                };
                ls_emit_entry(ctx.output, &e, &flags);
            }
            Err(e) => {
                emit_error(ctx.output, "ls", path, &e);
                status = 1;
            }
        }
    }
    status
}

pub(crate) fn util_mkdir(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    if !require_args(argv, 2, ctx.output) {
        return 1;
    }
    let mut parents = false;
    let mut dirs = Vec::new();
    let mut skip_next = false;
    for (idx, arg) in argv[1..].iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if *arg == "--" {
            dirs.extend(argv[idx + 2..].iter().copied());
            break;
        } else if *arg == "-p" || *arg == "--parents" {
            parents = true;
        } else if *arg == "-m" {
            skip_next = true; // skip the mode argument
        } else if arg.starts_with('-') && arg.len() > 1 {
            for ch in arg[1..].chars() {
                // 'v', 'm' etc. accepted, no-op
                if ch == 'p' {
                    parents = true;
                }
            }
        } else {
            dirs.push(*arg);
        }
    }
    let mut status = 0;
    for path in &dirs {
        let full = resolve_path(ctx.cwd, path);
        if parents {
            if let Err(e) = mkdir_parents(ctx.fs, &full) {
                emit_error(ctx.output, "mkdir", path, &e);
                status = 1;
            }
        } else if let Err(e) = ctx.fs.create_dir(&full) {
            emit_error(ctx.output, "mkdir", path, &e);
            status = 1;
        }
    }
    status
}

fn mkdir_parents(fs: &mut BackendFs, path: &str) -> Result<(), wasmsh_fs::FsError> {
    // Build each ancestor and create if missing
    let mut current = String::new();
    for component in path.split('/').filter(|c| !c.is_empty()) {
        current.push('/');
        current.push_str(component);
        if fs.stat(&current).is_err() {
            fs.create_dir(&current)?;
        }
    }
    Ok(())
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
fn rm_recursive(fs: &mut BackendFs, path: &str) -> Result<(), ()> {
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
    let mut no_create = false;
    let mut files = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        match argv[i] {
            "-c" | "-a" | "-m" => {} // time flags, no-op in VFS
            "-d" | "-t" | "-r" if i + 1 < argv.len() => {
                i += 1; // skip argument, no-op
            }
            arg if arg.starts_with('-') && arg.len() > 1 && arg != "--" => {
                for ch in arg[1..].chars() {
                    // 'a', 'm' etc. — no-op
                    if ch == 'c' {
                        no_create = true;
                    }
                }
            }
            "--" => {
                files.extend(argv[i + 1..].iter().copied());
                break;
            }
            _ => files.push(argv[i]),
        }
        i += 1;
    }
    if files.is_empty() {
        ctx.output.stderr(b"touch: missing operand\n");
        return 1;
    }
    let mut status = 0;
    for path in &files {
        let full = resolve_path(ctx.cwd, path);
        if no_create && ctx.fs.stat(&full).is_err() {
            continue;
        }
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
    let mut no_clobber = false;
    let mut verbose = false;
    let mut args = Vec::new();
    for arg in &argv[1..] {
        if arg.starts_with('-') && arg.len() > 1 && *arg != "--" {
            for ch in arg[1..].chars() {
                match ch {
                    'f' => no_clobber = false,
                    'n' => no_clobber = true,
                    'v' => verbose = true,
                    // 'i' (interactive) etc. — no-op in sandbox
                    _ => {}
                }
            }
        } else if *arg != "--" {
            args.push(*arg);
        }
    }
    if args.len() < 2 {
        ctx.output.stderr(b"mv: missing operand\n");
        return 1;
    }
    let src_arg = args[0];
    let dst_arg = args[1];
    let src = resolve_path(ctx.cwd, src_arg);
    let dst_base = resolve_path(ctx.cwd, dst_arg);
    let dst = if ctx.fs.stat(&dst_base).map(|m| m.is_dir).unwrap_or(false) {
        let name = src_arg.rsplit('/').next().unwrap_or(src_arg);
        child_path(&dst_base, name)
    } else {
        dst_base
    };
    if no_clobber && ctx.fs.stat(&dst).is_ok() {
        return 0;
    }
    if let Err(e) = copy_file_contents(ctx.fs, &src, &dst) {
        emit_error(ctx.output, "mv", src_arg, &e);
        return 1;
    }
    if let Err(e) = ctx.fs.remove_file(&src) {
        emit_error(ctx.output, "mv", src_arg, &e);
        return 1;
    }
    if verbose {
        let msg = format!("'{src_arg}' -> '{dst_arg}'\n");
        ctx.output.stdout(msg.as_bytes());
    }
    0
}

#[allow(clippy::struct_excessive_bools)]
struct CpFlags {
    recursive: bool,
    force: bool,
    no_clobber: bool,
    verbose: bool,
}

fn parse_cp_flags<'a>(argv: &'a [&'a str]) -> (CpFlags, Vec<&'a str>) {
    let mut flags = CpFlags {
        recursive: false,
        force: false,
        no_clobber: false,
        verbose: false,
    };
    let mut args = Vec::new();
    for arg in &argv[1..] {
        if arg.starts_with('-') && arg.len() > 1 && *arg != "--" {
            for ch in arg[1..].chars() {
                match ch {
                    'r' | 'R' | 'a' => flags.recursive = true,
                    'f' => flags.force = true,
                    'n' => flags.no_clobber = true,
                    'v' => flags.verbose = true,
                    // 'p' (preserve attrs) etc. — no-op in VFS
                    _ => {}
                }
            }
        } else if *arg != "--" {
            args.push(*arg);
        }
    }
    (flags, args)
}

fn cp_recursive(fs: &mut BackendFs, src: &str, dst: &str) -> Result<(), String> {
    if let Err(e) = fs.create_dir(dst) {
        return Err(e.to_string());
    }
    let entries = fs.read_dir(src).map_err(|e| e.to_string())?;
    for entry in entries {
        let src_child = child_path(src, &entry.name);
        let dst_child = child_path(dst, &entry.name);
        if entry.is_dir {
            cp_recursive(fs, &src_child, &dst_child)?;
        } else {
            copy_file_contents(fs, &src_child, &dst_child)?;
        }
    }
    Ok(())
}

pub(crate) fn util_cp(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, args) = parse_cp_flags(argv);
    if args.len() < 2 {
        ctx.output.stderr(b"cp: missing operand\n");
        return 1;
    }
    let dst_arg = args[args.len() - 1];
    let sources = &args[..args.len() - 1];
    let dst_base = resolve_path(ctx.cwd, dst_arg);

    let mut status = 0;
    for src_arg in sources {
        let src = resolve_path(ctx.cwd, src_arg);
        let is_dir = ctx.fs.stat(&src).map(|m| m.is_dir).unwrap_or(false);
        let dst = if ctx.fs.stat(&dst_base).map(|m| m.is_dir).unwrap_or(false) {
            let name = src_arg.rsplit('/').next().unwrap_or(src_arg);
            child_path(&dst_base, name)
        } else {
            dst_base.clone()
        };

        if flags.no_clobber && ctx.fs.stat(&dst).is_ok() {
            continue;
        }
        if flags.force && ctx.fs.stat(&dst).is_ok() {
            let _ = ctx.fs.remove_file(&dst);
        }

        if is_dir {
            if !flags.recursive {
                let msg = format!("cp: -r not specified; omitting directory '{src_arg}'\n");
                ctx.output.stderr(msg.as_bytes());
                status = 1;
                continue;
            }
            if let Err(e) = cp_recursive(ctx.fs, &src, &dst) {
                emit_error(ctx.output, "cp", src_arg, &e);
                status = 1;
                continue;
            }
        } else if let Err(e) = copy_file_contents(ctx.fs, &src, &dst) {
            emit_error(ctx.output, "cp", src_arg, &e);
            status = 1;
            continue;
        }

        if flags.verbose {
            let msg = format!("'{src_arg}' -> '{dst_arg}'\n");
            ctx.output.stdout(msg.as_bytes());
        }
    }
    status
}

pub(crate) fn util_ln(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    // VFS doesn't support real links/symlinks, so ln creates a copy
    let mut force = false;
    let mut verbose = false;
    let mut args = Vec::new();
    for arg in &argv[1..] {
        if arg.starts_with('-') && arg.len() > 1 && *arg != "--" {
            for ch in arg[1..].chars() {
                match ch {
                    'f' => force = true,
                    'v' => verbose = true,
                    // 's' (symbolic), 'n' (no-dereference) etc. — accepted, VFS always copies
                    _ => {}
                }
            }
        } else if *arg != "--" {
            args.push(*arg);
        }
    }
    if args.len() < 2 {
        ctx.output.stderr(b"ln: missing operand\n");
        return 1;
    }
    let src_arg = args[0];
    let dst_arg = args[1];
    let src = resolve_path(ctx.cwd, src_arg);
    let dst = resolve_path(ctx.cwd, dst_arg);
    if force {
        let _ = ctx.fs.remove_file(&dst);
    }
    if let Err(e) = copy_file_contents(ctx.fs, &src, &dst) {
        emit_error(ctx.output, "ln", src_arg, &e);
        return 1;
    }
    if verbose {
        let msg = format!("'{src_arg}' -> '{dst_arg}'\n");
        ctx.output.stdout(msg.as_bytes());
    }
    0
}

pub(crate) fn util_readlink(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    // VFS has no symlinks — just output the canonical path
    let mut canonicalize = false;
    let mut must_exist = false;
    let mut paths = Vec::new();
    for arg in &argv[1..] {
        match *arg {
            "-f" | "-m" => canonicalize = true,
            "-e" => {
                canonicalize = true;
                must_exist = true;
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                for ch in arg[1..].chars() {
                    match ch {
                        'f' | 'm' => canonicalize = true,
                        'e' => {
                            canonicalize = true;
                            must_exist = true;
                        }
                        _ => {}
                    }
                }
            }
            _ => paths.push(*arg),
        }
    }
    if paths.is_empty() {
        ctx.output.stderr(b"readlink: missing operand\n");
        return 1;
    }
    let _ = canonicalize; // always canonicalize in VFS
    let mut status = 0;
    for path in &paths {
        let full = resolve_path(ctx.cwd, path);
        if must_exist && ctx.fs.stat(&full).is_err() {
            emit_error(ctx.output, "readlink", path, &"No such file or directory");
            status = 1;
            continue;
        }
        ctx.output.stdout(full.as_bytes());
        ctx.output.stdout(b"\n");
    }
    status
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

fn stat_format(fmt: &str, name: &str, size: u64, is_dir: bool) -> String {
    let mut result = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            match chars.next() {
                Some('n') => result.push_str(name),
                Some('s') => {
                    let _ = write!(result, "{size}");
                }
                Some('F') => result.push_str(if is_dir { "directory" } else { "regular file" }),
                Some('a') => result.push_str(if is_dir { "755" } else { "644" }),
                Some('A') => {
                    result.push_str(if is_dir { "drwxr-xr-x" } else { "-rw-r--r--" });
                }
                Some('U' | 'G') => result.push_str("user"),
                Some('h') => result.push('1'),
                Some('f') => result.push_str(if is_dir { "41ed" } else { "81a4" }),
                Some('Y') => result.push('0'),
                Some('%') | None => result.push('%'),
                Some(c) => {
                    result.push('%');
                    result.push(c);
                }
            }
        } else if ch == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some('\\') | None => result.push('\\'),
                Some(c) => {
                    result.push('\\');
                    result.push(c);
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
}

pub(crate) fn util_stat(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut format_str: Option<&str> = None;
    let mut printf_mode = false;
    let mut files = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i];
        if arg == "-c" && i + 1 < argv.len() {
            format_str = Some(argv[i + 1]);
            i += 2;
        } else if let Some(fmt) = arg.strip_prefix("--format=") {
            format_str = Some(fmt);
            i += 1;
        } else if arg == "--printf" && i + 1 < argv.len() {
            format_str = Some(argv[i + 1]);
            printf_mode = true;
            i += 2;
        } else if let Some(fmt) = arg.strip_prefix("--printf=") {
            format_str = Some(fmt);
            printf_mode = true;
            i += 1;
        } else if arg.starts_with('-') && arg.len() > 1 {
            i += 1; // skip unknown flags
        } else {
            files.push(arg);
            i += 1;
        }
    }
    if files.is_empty() {
        ctx.output.stderr(b"stat: missing operand\n");
        return 1;
    }
    let mut status = 0;
    for path in &files {
        let full = resolve_path(ctx.cwd, path);
        match ctx.fs.stat(&full) {
            Ok(meta) => {
                if let Some(fmt) = format_str {
                    let out = stat_format(fmt, path, meta.size, meta.is_dir);
                    ctx.output.stdout(out.as_bytes());
                    if !printf_mode {
                        ctx.output.stdout(b"\n");
                    }
                } else {
                    let kind = if meta.is_dir {
                        "directory"
                    } else {
                        "regular file"
                    };
                    let out = format!("  File: {path}\n  Size: {}\n  Type: {kind}\n", meta.size);
                    ctx.output.stdout(out.as_bytes());
                }
            }
            Err(e) => {
                emit_error(ctx.output, "stat", path, &e);
                status = 1;
            }
        }
    }
    status
}

#[allow(clippy::struct_excessive_bools)]
struct FindFilters<'a> {
    name_pattern: Option<&'a str>,
    iname_pattern: Option<&'a str>,
    type_filter: Option<&'a str>,
    path_pattern: Option<&'a str>,
    maxdepth: Option<usize>,
    mindepth: Option<usize>,
    delete: bool,
    empty: bool,
    negate_next: bool,
    print0: bool,
    printf_format: Option<&'a str>,
    size_filter: Option<&'a str>,
}

fn parse_find_args<'a>(argv: &'a [&'a str]) -> (&'a str, FindFilters<'a>) {
    let mut args = &argv[1..];
    let dir = if !args.is_empty() && !args[0].starts_with('-') && args[0] != "!" && args[0] != "(" {
        let d = args[0];
        args = &args[1..];
        d
    } else {
        "."
    };

    let mut filters = FindFilters {
        name_pattern: None,
        iname_pattern: None,
        type_filter: None,
        path_pattern: None,
        maxdepth: None,
        mindepth: None,
        delete: false,
        empty: false,
        negate_next: false,
        print0: false,
        printf_format: None,
        size_filter: None,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "-name" if i + 1 < args.len() => {
                filters.name_pattern = Some(args[i + 1]);
                i += 2;
            }
            "-iname" if i + 1 < args.len() => {
                filters.iname_pattern = Some(args[i + 1]);
                i += 2;
            }
            "-type" if i + 1 < args.len() => {
                filters.type_filter = Some(args[i + 1]);
                i += 2;
            }
            "-path" | "-ipath" if i + 1 < args.len() => {
                filters.path_pattern = Some(args[i + 1]);
                i += 2;
            }
            "-maxdepth" if i + 1 < args.len() => {
                filters.maxdepth = args[i + 1].parse().ok();
                i += 2;
            }
            "-mindepth" if i + 1 < args.len() => {
                filters.mindepth = args[i + 1].parse().ok();
                i += 2;
            }
            "-size" if i + 1 < args.len() => {
                filters.size_filter = Some(args[i + 1]);
                i += 2;
            }
            "-delete" => {
                filters.delete = true;
                i += 1;
            }
            "-empty" => {
                filters.empty = true;
                i += 1;
            }
            "-printf" if i + 1 < args.len() => {
                filters.printf_format = Some(args[i + 1]);
                i += 2;
            }
            "-print" | "-and" | "-a" | "-o" | "-or" => {
                i += 1;
            }
            "-print0" => {
                filters.print0 = true;
                i += 1;
            }
            "!" | "-not" => {
                filters.negate_next = true;
                i += 1;
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

fn find_size_matches(spec: &str, actual_size: u64) -> bool {
    let (cmp, rest) = if let Some(r) = spec.strip_prefix('+') {
        (1i8, r)
    } else if let Some(r) = spec.strip_prefix('-') {
        (-1i8, r)
    } else {
        (0i8, spec)
    };
    let multiplier: u64 = if rest.ends_with('c') {
        1
    } else if rest.ends_with('k') {
        1024
    } else if rest.ends_with('M') {
        1_048_576
    } else if rest.ends_with('G') {
        1_073_741_824
    } else {
        512 // default: 512-byte blocks
    };
    let num_str = rest.trim_end_matches(|c: char| c.is_alphabetic());
    let threshold = num_str
        .parse::<u64>()
        .unwrap_or(0)
        .saturating_mul(multiplier);
    match cmp {
        1 => actual_size > threshold,
        -1 => actual_size < threshold,
        _ => actual_size == threshold,
    }
}

fn find_entry_matches(
    filters: &FindFilters<'_>,
    name: &str,
    full_path: &str,
    is_dir: bool,
    size: u64,
    fs: &BackendFs,
) -> bool {
    let mut matched = true;
    if let Some(p) = filters.name_pattern {
        matched = matched && find_name_matches(p, name);
    }
    if let Some(p) = filters.iname_pattern {
        matched = matched && find_name_matches(&p.to_lowercase(), &name.to_lowercase());
    }
    if let Some(t) = filters.type_filter {
        matched = matched && find_type_matches(t, is_dir);
    }
    if let Some(p) = filters.path_pattern {
        matched = matched && find_name_matches(p, full_path);
    }
    if filters.empty {
        if is_dir {
            matched = matched
                && fs
                    .read_dir(full_path)
                    .map(|e| e.is_empty())
                    .unwrap_or(false);
        } else {
            matched = matched && size == 0;
        }
    }
    if let Some(s) = filters.size_filter {
        matched = matched && find_size_matches(s, size);
    }
    if filters.negate_next {
        !matched
    } else {
        matched
    }
}

/// Format a find -printf format string for a single entry.
fn find_printf_format(fmt: &str, path: &str, is_dir: bool, size: u64) -> String {
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '%' => match chars.next() {
                Some('s') => out.push_str(&size.to_string()),
                Some('p') => out.push_str(path),
                Some('f') => {
                    let name = path.rsplit('/').next().unwrap_or(path);
                    out.push_str(name);
                }
                Some('y') => out.push(if is_dir { 'd' } else { 'f' }),
                Some('T') => {
                    // %T@ = mtime as epoch seconds (VFS has no mtime, emit 0)
                    if chars.peek() == Some(&'@') {
                        chars.next();
                        out.push('0');
                    } else {
                        out.push('0');
                    }
                }
                Some('d') => out.push_str(&path.matches('/').count().to_string()),
                Some('%') | None => out.push('%'),
                Some(other) => {
                    out.push('%');
                    out.push(other);
                }
            },
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('0') => out.push('\0'),
                Some('\\') | None => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
            },
            _ => out.push(c),
        }
    }
    out
}

fn walk_find(
    fs: &mut BackendFs,
    path: &str,
    filters: &FindFilters<'_>,
    output: &mut dyn UtilOutput,
    depth: usize,
    to_delete: &mut Vec<(String, bool)>,
) {
    if let Some(max) = filters.maxdepth {
        if depth > max {
            return;
        }
    }
    let Ok(entries) = fs.read_dir(path) else {
        return;
    };
    for entry in entries {
        let child = child_path(path, &entry.name);
        let size = fs.stat(&child).map(|m| m.size).unwrap_or(0);
        let emit_depth = filters.mindepth.unwrap_or(0);
        if depth + 1 >= emit_depth
            && find_entry_matches(filters, &entry.name, &child, entry.is_dir, size, fs)
        {
            if filters.delete {
                to_delete.push((child.clone(), entry.is_dir));
            } else if let Some(fmt) = filters.printf_format {
                let formatted = find_printf_format(fmt, &child, entry.is_dir, size);
                output.stdout(formatted.as_bytes());
            } else if filters.print0 {
                output.stdout(child.as_bytes());
                output.stdout(b"\0");
            } else {
                output.stdout(child.as_bytes());
                output.stdout(b"\n");
            }
        }
        if entry.is_dir {
            let can_recurse = filters.maxdepth.is_none_or(|max| depth + 1 < max);
            if can_recurse {
                walk_find(fs, &child, filters, output, depth + 1, to_delete);
            }
        }
    }
}

pub(crate) fn util_find(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (dir, filters) = parse_find_args(argv);
    let full = resolve_path(ctx.cwd, dir);
    let mut to_delete = Vec::new();
    walk_find(ctx.fs, &full, &filters, ctx.output, 0, &mut to_delete);
    // Process deletions in reverse order (deepest first)
    for (path, is_dir) in to_delete.into_iter().rev() {
        if is_dir {
            let _ = ctx.fs.remove_dir(&path);
        } else {
            let _ = ctx.fs.remove_file(&path);
        }
    }
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
