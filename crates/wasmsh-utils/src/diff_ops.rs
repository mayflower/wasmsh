//! Diff utilities: diff, patch.

use std::fmt::Write;

use wasmsh_fs::{OpenOptions, Vfs};

use crate::helpers::{child_path, emit_error, read_text, resolve_path};
use crate::UtilContext;

// ---------------------------------------------------------------------------
// Shared constants
// ---------------------------------------------------------------------------

/// Maximum product of line counts before we bail out of detailed diff.
const MAX_DP_CELLS: usize = 10_000_000;

// ---------------------------------------------------------------------------
// LCS / edit-script computation
// ---------------------------------------------------------------------------

/// An individual edit operation derived from LCS backtracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditOp {
    /// Line present in both files (context).
    Equal,
    /// Line present only in the old file (deletion).
    Delete,
    /// Line present only in the new file (insertion).
    Insert,
}

/// Compute the LCS-based edit script between two line sequences.
///
/// Returns a vector of `(EditOp, old_idx, new_idx)` triples where
/// the indices point into the respective line arrays. For `Delete`
/// entries `new_idx` is meaningless and vice versa for `Insert`.
fn compute_edit_script(old: &[&str], new: &[&str]) -> Vec<(EditOp, usize, usize)> {
    let m = old.len();
    let n = new.len();

    // Build DP table: dp[i][j] = LCS length of old[i..] vs new[j..]
    // We need the full table for backtracking.
    let mut dp = vec![vec![0u32; n + 1]; m + 1];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            dp[i][j] = if old[i] == new[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    // Backtrack to produce the edit script.
    let mut ops = Vec::with_capacity(m + n);
    let mut i = 0;
    let mut j = 0;
    while i < m && j < n {
        if old[i] == new[j] {
            ops.push((EditOp::Equal, i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push((EditOp::Delete, i, j));
            i += 1;
        } else {
            ops.push((EditOp::Insert, i, j));
            j += 1;
        }
    }
    while i < m {
        ops.push((EditOp::Delete, i, j));
        i += 1;
    }
    while j < n {
        ops.push((EditOp::Insert, i, j));
        j += 1;
    }
    ops
}

// ---------------------------------------------------------------------------
// Edit-script grouping into hunks
// ---------------------------------------------------------------------------

/// A contiguous group of changes with surrounding context lines.
struct Hunk<'a> {
    old_start: usize, // 1-based
    old_count: usize,
    new_start: usize, // 1-based
    new_count: usize,
    lines: Vec<(EditOp, &'a str)>,
}

/// Build a single hunk from a slice of edit operations.
fn build_hunk<'a>(ops: &[(EditOp, usize, usize)], old: &[&'a str], new: &[&'a str]) -> Hunk<'a> {
    let mut hunk_lines: Vec<(EditOp, &str)> = Vec::new();
    let mut old_line_start = usize::MAX;
    let mut new_line_start = usize::MAX;
    let mut old_count = 0usize;
    let mut new_count = 0usize;

    for &(op, oi, ni) in ops {
        if old_line_start == usize::MAX {
            old_line_start = oi;
            new_line_start = ni;
        }
        match op {
            EditOp::Equal => {
                hunk_lines.push((EditOp::Equal, old[oi]));
                old_count += 1;
                new_count += 1;
            }
            EditOp::Delete => {
                hunk_lines.push((EditOp::Delete, old[oi]));
                old_count += 1;
            }
            EditOp::Insert => {
                hunk_lines.push((EditOp::Insert, new[ni]));
                new_count += 1;
            }
        }
    }

    if old_line_start == usize::MAX {
        old_line_start = 0;
    }
    if new_line_start == usize::MAX {
        new_line_start = 0;
    }

    Hunk {
        old_start: old_line_start + 1,
        old_count,
        new_start: new_line_start + 1,
        new_count,
        lines: hunk_lines,
    }
}

/// Group an edit script into hunks with `ctx` lines of surrounding context.
fn group_hunks<'a>(
    ops: &[(EditOp, usize, usize)],
    old: &[&'a str],
    new: &[&'a str],
    ctx: usize,
) -> Vec<Hunk<'a>> {
    if ops.is_empty() {
        return Vec::new();
    }

    // Collect runs of changes, noting which ops-indices are non-Equal.
    let change_indices: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, (op, _, _))| *op != EditOp::Equal)
        .map(|(idx, _)| idx)
        .collect();

    if change_indices.is_empty() {
        return Vec::new();
    }

    // Group change indices into clusters where context regions overlap.
    let mut groups: Vec<(usize, usize)> = Vec::new(); // (first_change_idx, last_change_idx)
    let mut grp_start = change_indices[0];
    let mut grp_end = change_indices[0];

    for &ci in &change_indices[1..] {
        // If the gap between this change and the previous one is small enough
        // that their context regions overlap, merge them.
        if ci <= grp_end + 2 * ctx + 1 {
            grp_end = ci;
        } else {
            groups.push((grp_start, grp_end));
            grp_start = ci;
            grp_end = ci;
        }
    }
    groups.push((grp_start, grp_end));

    let mut hunks = Vec::new();

    for (gs, ge) in groups {
        let start = gs.saturating_sub(ctx);
        let end = (ge + ctx + 1).min(ops.len());
        hunks.push(build_hunk(&ops[start..end], old, new));
    }

    hunks
}

// ---------------------------------------------------------------------------
// Output formatters
// ---------------------------------------------------------------------------

/// Format a range for normal diff output (e.g. "3", "3,5").
fn normal_range(start: usize, count: usize) -> String {
    if count <= 1 {
        format!("{start}")
    } else {
        format!("{},{}", start, start + count - 1)
    }
}

/// Track line positions through a hunk to find the change range.
struct ChangeRange {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    found: bool,
}

fn compute_change_range(hunk: &Hunk<'_>) -> ChangeRange {
    let mut cr = ChangeRange {
        old_start: 0,
        old_count: 0,
        new_start: 0,
        new_count: 0,
        found: false,
    };
    let mut old_pos = hunk.old_start;
    let mut new_pos = hunk.new_start;

    for &(op, _) in &hunk.lines {
        match op {
            EditOp::Equal => {
                old_pos += 1;
                new_pos += 1;
            }
            EditOp::Delete => {
                if !cr.found {
                    cr.old_start = old_pos;
                    cr.new_start = new_pos;
                    cr.found = true;
                }
                cr.old_count += 1;
                old_pos += 1;
            }
            EditOp::Insert => {
                if !cr.found {
                    cr.old_start = old_pos;
                    cr.new_start = new_pos;
                    cr.found = true;
                }
                cr.new_count += 1;
                new_pos += 1;
            }
        }
    }
    cr
}

fn format_normal_ranges(cr: &ChangeRange) -> (String, String) {
    let old_range = if cr.old_count == 0 {
        format!("{}", cr.old_start.saturating_sub(1))
    } else {
        normal_range(cr.old_start, cr.old_count)
    };
    let new_range = if cr.new_count == 0 {
        format!("{}", cr.new_start.saturating_sub(1))
    } else {
        normal_range(cr.new_start, cr.new_count)
    };
    (old_range, new_range)
}

fn emit_normal_hunk(
    out: &mut String,
    deleted: &[&str],
    inserted: &[&str],
    old_range: &str,
    new_range: &str,
) {
    if !deleted.is_empty() && !inserted.is_empty() {
        let _ = writeln!(out, "{old_range}c{new_range}");
        for line in deleted {
            let _ = writeln!(out, "< {line}");
        }
        out.push_str("---\n");
        for line in inserted {
            let _ = writeln!(out, "> {line}");
        }
    } else if !deleted.is_empty() {
        let _ = writeln!(out, "{old_range}d{new_range}");
        for line in deleted {
            let _ = writeln!(out, "< {line}");
        }
    } else {
        let _ = writeln!(out, "{old_range}a{new_range}");
        for line in inserted {
            let _ = writeln!(out, "> {line}");
        }
    }
}

/// Produce normal (ed-style) diff output.
fn format_normal(hunks: &[Hunk<'_>]) -> String {
    let mut out = String::new();
    for hunk in hunks {
        let deleted: Vec<&str> = hunk
            .lines
            .iter()
            .filter(|(op, _)| *op == EditOp::Delete)
            .map(|(_, l)| *l)
            .collect();
        let inserted: Vec<&str> = hunk
            .lines
            .iter()
            .filter(|(op, _)| *op == EditOp::Insert)
            .map(|(_, l)| *l)
            .collect();

        if deleted.is_empty() && inserted.is_empty() {
            continue;
        }

        let cr = compute_change_range(hunk);
        if !cr.found {
            continue;
        }

        let (old_range, new_range) = format_normal_ranges(&cr);
        emit_normal_hunk(&mut out, &deleted, &inserted, &old_range, &new_range);
    }
    out
}

/// Produce unified diff output.
fn format_unified(
    hunks: &[Hunk<'_>],
    old_label: &str,
    new_label: &str,
    context_lines: usize,
) -> String {
    if hunks.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let _ = writeln!(out, "--- {old_label}");
    let _ = writeln!(out, "+++ {new_label}");

    // Re-group with the requested context size — hunks were already built
    // with context so we just emit them.
    for hunk in hunks {
        // Emit hunk header.
        let old_start = hunk.old_start;
        let new_start = hunk.new_start;
        // Unified format uses count 0 for empty side but still prints start.
        let _ = context_lines; // context already baked into hunk
        let _ = writeln!(
            out,
            "@@ -{},{} +{},{} @@",
            old_start, hunk.old_count, new_start, hunk.new_count
        );
        for &(op, line) in &hunk.lines {
            match op {
                EditOp::Equal => {
                    out.push(' ');
                    out.push_str(line);
                    out.push('\n');
                }
                EditOp::Delete => {
                    out.push('-');
                    out.push_str(line);
                    out.push('\n');
                }
                EditOp::Insert => {
                    out.push('+');
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// Produce context diff output.
fn format_context(hunks: &[Hunk<'_>], old_label: &str, new_label: &str) -> String {
    if hunks.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let _ = writeln!(out, "*** {old_label}");
    let _ = writeln!(out, "--- {new_label}");

    for hunk in hunks {
        let old_end = hunk.old_start + hunk.old_count.saturating_sub(1);
        let new_end = hunk.new_start + hunk.new_count.saturating_sub(1);

        // Old section.
        let _ = writeln!(out, "*** {},{} ***", hunk.old_start, old_end);
        for &(op, line) in &hunk.lines {
            match op {
                EditOp::Equal => {
                    out.push_str("  ");
                    out.push_str(line);
                    out.push('\n');
                }
                EditOp::Delete => {
                    out.push_str("- ");
                    out.push_str(line);
                    out.push('\n');
                }
                EditOp::Insert => {
                    // Insertions don't appear in old section.
                }
            }
        }

        // New section.
        let _ = writeln!(out, "--- {},{} ---", hunk.new_start, new_end);
        for &(op, line) in &hunk.lines {
            match op {
                EditOp::Equal => {
                    out.push_str("  ");
                    out.push_str(line);
                    out.push('\n');
                }
                EditOp::Delete => {
                    // Deletions don't appear in new section.
                }
                EditOp::Insert => {
                    out.push_str("+ ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Line pre-processing helpers
// ---------------------------------------------------------------------------

/// Normalise a line for comparison when ignore-all-space is active.
fn strip_all_space(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Check whether a line is blank (empty or all whitespace).
fn is_blank(s: &str) -> bool {
    s.chars().all(char::is_whitespace)
}

// ---------------------------------------------------------------------------
// Recursive directory diff
// ---------------------------------------------------------------------------

/// Collect sorted list of relative paths under `dir` in the VFS.
fn collect_dir_entries(
    fs: &wasmsh_fs::BackendFs,
    dir: &str,
    prefix: &str,
    out: &mut Vec<(String, bool)>,
) {
    if let Ok(entries) = fs.read_dir(dir) {
        let mut entries = entries;
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        for entry in entries {
            let cp = child_path(dir, &entry.name);
            let rel = if prefix.is_empty() {
                entry.name.clone()
            } else {
                format!("{}/{}", prefix, entry.name)
            };
            out.push((rel.clone(), entry.is_dir));
            if entry.is_dir {
                collect_dir_entries(fs, &cp, &rel, out);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// diff flags
// ---------------------------------------------------------------------------

#[allow(clippy::struct_excessive_bools)]
struct DiffFlags {
    unified: bool,
    context_fmt: bool,
    brief: bool,
    ignore_blank_lines: bool,
    ignore_all_space: bool,
    recursive: bool,
    new_file: bool,
    context_lines: usize,
}

impl Default for DiffFlags {
    fn default() -> Self {
        Self {
            unified: false,
            context_fmt: false,
            brief: false,
            ignore_blank_lines: false,
            ignore_all_space: false,
            recursive: false,
            new_file: false,
            context_lines: 3,
        }
    }
}

// ---------------------------------------------------------------------------
// Core diff between two line slices
// ---------------------------------------------------------------------------

/// Build comparison lines, optionally stripping whitespace.
fn build_comparison_lines(lines: &[&str], strip_space: bool) -> Vec<String> {
    if strip_space {
        lines.iter().map(|l| strip_all_space(l)).collect()
    } else {
        lines.iter().map(|l| (*l).to_string()).collect()
    }
}

/// Filter blank-line-only changes back to Equal ops if `ignore_blank_lines` is set.
fn filter_blank_lines(
    ops: Vec<(EditOp, usize, usize)>,
    old_lines: &[&str],
    new_lines: &[&str],
) -> Vec<(EditOp, usize, usize)> {
    ops.into_iter()
        .map(|(op, oi, ni)| match op {
            EditOp::Delete if is_blank(old_lines.get(oi).unwrap_or(&"")) => (EditOp::Equal, oi, ni),
            EditOp::Insert if is_blank(new_lines.get(ni).unwrap_or(&"")) => (EditOp::Equal, oi, ni),
            _ => (op, oi, ni),
        })
        .collect()
}

/// Compare two texts and return the formatted diff string.
/// Returns `(output, files_differ)`.
fn diff_texts(
    old_lines: &[&str],
    new_lines: &[&str],
    old_label: &str,
    new_label: &str,
    flags: &DiffFlags,
) -> (String, bool) {
    let m = old_lines.len();
    let n = new_lines.len();

    if (m as u64) * (n as u64) > MAX_DP_CELLS as u64 {
        let msg = format!("Files {old_label} and {new_label} differ\n");
        return (msg, true);
    }

    let cmp_old = build_comparison_lines(old_lines, flags.ignore_all_space);
    let cmp_new = build_comparison_lines(new_lines, flags.ignore_all_space);
    let cmp_old_refs: Vec<&str> = cmp_old.iter().map(String::as_str).collect();
    let cmp_new_refs: Vec<&str> = cmp_new.iter().map(String::as_str).collect();

    let ops = compute_edit_script(&cmp_old_refs, &cmp_new_refs);
    let ops = if flags.ignore_blank_lines {
        filter_blank_lines(ops, old_lines, new_lines)
    } else {
        ops
    };

    let has_changes = ops.iter().any(|(op, _, _)| *op != EditOp::Equal);
    if !has_changes {
        return (String::new(), false);
    }
    if flags.brief {
        let msg = format!("Files {old_label} and {new_label} differ\n");
        return (msg, true);
    }

    let hunks = group_hunks(&ops, old_lines, new_lines, flags.context_lines);
    let output = if flags.unified {
        format_unified(&hunks, old_label, new_label, flags.context_lines)
    } else if flags.context_fmt {
        format_context(&hunks, old_label, new_label)
    } else {
        format_normal(&hunks)
    };

    (output, true)
}

// ---------------------------------------------------------------------------
// diff utility entry point
// ---------------------------------------------------------------------------

fn parse_diff_flags<'a>(
    ctx: &mut UtilContext<'_>,
    argv: &'a [&'a str],
) -> Result<(DiffFlags, Vec<&'a str>), i32> {
    let mut flags = DiffFlags::default();
    let mut positional: Vec<&str> = Vec::new();
    let mut args = &argv[1..];

    while let Some(&arg) = args.first() {
        if arg == "--" {
            args = &args[1..];
            positional.extend_from_slice(args);
            break;
        } else if try_set_diff_long_flag(arg, &mut flags) {
            args = &args[1..];
        } else if arg.starts_with("-U") && arg.len() > 2 {
            if let Ok(n) = arg[2..].parse::<usize>() {
                flags.unified = true;
                flags.context_lines = n;
            }
            args = &args[1..];
        } else if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") {
            parse_diff_bundled_flags(ctx, arg, &mut flags)?;
            args = &args[1..];
        } else {
            positional.push(arg);
            args = &args[1..];
        }
    }
    Ok((flags, positional))
}

/// Try to match a long/single-char diff flag. Returns `true` if consumed.
fn try_set_diff_long_flag(arg: &str, flags: &mut DiffFlags) -> bool {
    match arg {
        "-u" | "--unified" => flags.unified = true,
        "-c" => flags.context_fmt = true,
        "-q" | "--brief" => flags.brief = true,
        "-B" | "--ignore-blank-lines" => flags.ignore_blank_lines = true,
        "-w" | "--ignore-all-space" => flags.ignore_all_space = true,
        "-r" | "--recursive" => flags.recursive = true,
        "-N" | "--new-file" => flags.new_file = true,
        _ => return false,
    }
    true
}

fn parse_diff_bundled_flags(
    ctx: &mut UtilContext<'_>,
    arg: &str,
    flags: &mut DiffFlags,
) -> Result<(), i32> {
    for ch in arg[1..].chars() {
        match ch {
            'u' => flags.unified = true,
            'c' => flags.context_fmt = true,
            'q' => flags.brief = true,
            'B' => flags.ignore_blank_lines = true,
            'w' => flags.ignore_all_space = true,
            'r' => flags.recursive = true,
            'N' => flags.new_file = true,
            _ => {
                let msg = format!("diff: unknown option '-{ch}'\n");
                ctx.output.stderr(msg.as_bytes());
                return Err(2);
            }
        }
    }
    Ok(())
}

/// Read a file for diff, returning its text. Handles directories and missing files.
fn diff_read_file(
    ctx: &mut UtilContext<'_>,
    stat: &Result<wasmsh_fs::Metadata, wasmsh_fs::FsError>,
    path: &str,
    display: &str,
    new_file_flag: bool,
) -> Result<String, i32> {
    match stat {
        Ok(m) if m.is_dir => {
            let msg = format!("diff: {display}: Is a directory\n");
            ctx.output.stderr(msg.as_bytes());
            Err(2)
        }
        Ok(_) => read_text(ctx.fs, path).map_err(|e| {
            emit_error(ctx.output, "diff", display, &e);
            2
        }),
        Err(_) => {
            if new_file_flag {
                Ok(String::new())
            } else {
                emit_error(ctx.output, "diff", display, &"No such file or directory");
                Err(2)
            }
        }
    }
}

pub(crate) fn util_diff(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (flags, positional) = match parse_diff_flags(ctx, argv) {
        Ok(v) => v,
        Err(status) => return status,
    };

    if positional.len() < 2 {
        ctx.output.stderr(b"diff: missing operand\n");
        return 2;
    }

    let path_a = resolve_path(ctx.cwd, positional[0]);
    let path_b = resolve_path(ctx.cwd, positional[1]);

    let stat_a = ctx.fs.stat(&path_a);
    let stat_b = ctx.fs.stat(&path_b);

    let a_is_dir = stat_a.as_ref().is_ok_and(|m| m.is_dir);
    let b_is_dir = stat_b.as_ref().is_ok_and(|m| m.is_dir);

    if a_is_dir && b_is_dir {
        if !flags.recursive {
            let msg = format!("diff: {} is a directory\n", positional[0]);
            ctx.output.stderr(msg.as_bytes());
            return 2;
        }
        return diff_dirs(ctx, &path_a, &path_b, positional[0], positional[1], &flags);
    }

    let old_text = match diff_read_file(ctx, &stat_a, &path_a, positional[0], flags.new_file) {
        Ok(t) => t,
        Err(status) => return status,
    };
    let new_text = match diff_read_file(ctx, &stat_b, &path_b, positional[1], flags.new_file) {
        Ok(t) => t,
        Err(status) => return status,
    };

    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();

    let (output, differs) =
        diff_texts(&old_lines, &new_lines, positional[0], positional[1], &flags);

    if !output.is_empty() {
        ctx.output.stdout(output.as_bytes());
    }

    i32::from(differs)
}

/// Collect and merge unique file entries from two directories.
fn merge_dir_files(fs: &wasmsh_fs::BackendFs, dir_a: &str, dir_b: &str) -> Vec<String> {
    let mut entries_a: Vec<(String, bool)> = Vec::new();
    let mut entries_b: Vec<(String, bool)> = Vec::new();
    collect_dir_entries(fs, dir_a, "", &mut entries_a);
    collect_dir_entries(fs, dir_b, "", &mut entries_b);

    let files_a: Vec<&str> = entries_a
        .iter()
        .filter(|(_, is_dir)| !is_dir)
        .map(|(name, _)| name.as_str())
        .collect();
    let files_b: Vec<&str> = entries_b
        .iter()
        .filter(|(_, is_dir)| !is_dir)
        .map(|(name, _)| name.as_str())
        .collect();

    let mut all_files: Vec<String> = files_a.iter().map(|s| (*s).to_string()).collect();
    for f in &files_b {
        if !files_a.contains(f) {
            all_files.push((*f).to_string());
        }
    }
    all_files.sort_unstable();
    all_files
}

/// Read a file or return empty string if it doesn't exist.
fn read_text_or_empty(
    ctx: &mut UtilContext<'_>,
    path: &str,
    display: &str,
    exists: bool,
) -> Result<String, i32> {
    if !exists {
        return Ok(String::new());
    }
    read_text(ctx.fs, path).map_err(|e| {
        emit_error(ctx.output, "diff", display, &e);
        2
    })
}

/// Recursively diff two directory trees.
fn diff_dirs(
    ctx: &mut UtilContext<'_>,
    dir_a: &str,
    dir_b: &str,
    label_a: &str,
    label_b: &str,
    flags: &DiffFlags,
) -> i32 {
    let all_files = merge_dir_files(ctx.fs, dir_a, dir_b);

    let mut any_diff = false;
    let mut status = 0;

    for rel in &all_files {
        match diff_dir_entry(ctx, dir_a, dir_b, label_a, label_b, rel, flags) {
            Ok(differs) => any_diff |= differs,
            Err(entry_status) => status = entry_status,
        }
    }

    if status != 0 {
        status
    } else {
        i32::from(any_diff)
    }
}

fn diff_dir_entry(
    ctx: &mut UtilContext<'_>,
    dir_a: &str,
    dir_b: &str,
    label_a: &str,
    label_b: &str,
    rel: &str,
    flags: &DiffFlags,
) -> Result<bool, i32> {
    let full_a = format!("{}/{rel}", dir_a.trim_end_matches('/'));
    let full_b = format!("{}/{rel}", dir_b.trim_end_matches('/'));
    let lab_a = format!("{}/{rel}", label_a.trim_end_matches('/'));
    let lab_b = format!("{}/{rel}", label_b.trim_end_matches('/'));
    let a_exists = ctx.fs.stat(&full_a).is_ok();
    let b_exists = ctx.fs.stat(&full_b).is_ok();

    if let Some(differs) = diff_missing_dir_entry(
        ctx,
        rel,
        label_a,
        label_b,
        a_exists,
        b_exists,
        flags.new_file,
    ) {
        return Ok(differs);
    }

    let old_text = read_text_or_empty(ctx, &full_a, rel, a_exists)?;
    let new_text = read_text_or_empty(ctx, &full_b, rel, b_exists)?;
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();
    let (output, differs) = diff_texts(&old_lines, &new_lines, &lab_a, &lab_b, flags);
    if differs && !output.is_empty() {
        ctx.output.stdout(output.as_bytes());
    }
    Ok(differs)
}

fn diff_missing_dir_entry(
    ctx: &mut UtilContext<'_>,
    rel: &str,
    label_a: &str,
    label_b: &str,
    a_exists: bool,
    b_exists: bool,
    new_file: bool,
) -> Option<bool> {
    if !a_exists && !new_file {
        let msg = format!("Only in {label_b}: {rel}\n");
        ctx.output.stdout(msg.as_bytes());
        return Some(true);
    }
    if !b_exists && !new_file {
        let msg = format!("Only in {label_a}: {rel}\n");
        ctx.output.stdout(msg.as_bytes());
        return Some(true);
    }
    None
}

// ---------------------------------------------------------------------------
// patch utility
// ---------------------------------------------------------------------------

/// Tag for a line in a patch hunk body, preserving the original interleaving.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PatchOp {
    Context(String),
    Remove(String),
    Add(String),
}

/// A single hunk parsed from a unified diff.
struct PatchHunk {
    /// 1-based start line in the target file.
    old_start: usize,
    /// Lines to remove (without the leading `-`).
    remove: Vec<String>,
    /// Lines to add (without the leading `+`).
    add: Vec<String>,
    /// Body lines in their original unified-diff order.
    body: Vec<PatchOp>,
}

/// A single file's set of hunks from a unified diff patch.
struct PatchFile {
    /// Target path (after stripping).
    path: String,
    hunks: Vec<PatchHunk>,
}

/// Parse a unified diff from text into a list of per-file patch descriptions.
fn parse_unified_diff(text: &str) -> Vec<PatchFile> {
    let mut files: Vec<PatchFile> = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let Some((old_path, new_path)) = parse_patch_header(&lines, i) else {
            i += 1;
            continue;
        };
        i += 2;
        let hunks = parse_patch_hunks(&lines, &mut i);
        files.push(PatchFile {
            path: pick_patch_path(old_path, new_path).to_string(),
            hunks,
        });
    }

    files
}

fn parse_patch_header<'a>(lines: &'a [&'a str], index: usize) -> Option<(&'a str, &'a str)> {
    if !(lines.get(index)?.starts_with("--- ") && lines.get(index + 1)?.starts_with("+++ ")) {
        return None;
    }
    let old_path = lines[index][4..]
        .split('\t')
        .next()
        .unwrap_or(&lines[index][4..]);
    let new_path = lines[index + 1][4..]
        .split('\t')
        .next()
        .unwrap_or(&lines[index + 1][4..]);
    Some((old_path, new_path))
}

fn parse_patch_hunks(lines: &[&str], index: &mut usize) -> Vec<PatchHunk> {
    let mut hunks = Vec::new();
    while *index < lines.len() && lines[*index].starts_with("@@ ") {
        hunks.push(parse_patch_hunk(lines, index));
    }
    hunks
}

fn parse_patch_hunk(lines: &[&str], index: &mut usize) -> PatchHunk {
    let old_start = parse_hunk_header_old(lines[*index]);
    *index += 1;
    let mut remove = Vec::new();
    let mut add = Vec::new();
    let mut body = Vec::new();

    while *index < lines.len() {
        let line = lines[*index];
        if line.starts_with("@@ ") || line.starts_with("--- ") {
            break;
        }
        if !parse_patch_body_line(line, &mut remove, &mut add, &mut body) {
            break;
        }
        *index += 1;
    }

    PatchHunk {
        old_start,
        remove,
        add,
        body,
    }
}

fn parse_patch_body_line(
    line: &str,
    remove: &mut Vec<String>,
    add: &mut Vec<String>,
    body: &mut Vec<PatchOp>,
) -> bool {
    if let Some(rest) = line.strip_prefix('-') {
        remove.push(rest.to_string());
        body.push(PatchOp::Remove(rest.to_string()));
        return true;
    }
    if let Some(rest) = line.strip_prefix('+') {
        add.push(rest.to_string());
        body.push(PatchOp::Add(rest.to_string()));
        return true;
    }
    if let Some(rest) = line.strip_prefix(' ') {
        body.push(PatchOp::Context(rest.to_string()));
        return true;
    }
    if line.is_empty() {
        body.push(PatchOp::Context(String::new()));
        return true;
    }
    false
}

fn pick_patch_path<'a>(old_path: &'a str, new_path: &'a str) -> &'a str {
    if old_path == "/dev/null" {
        new_path
    } else {
        old_path
    }
}

/// Extract the old-file start line from a unified hunk header.
/// Format: `@@ -OLD_START[,OLD_COUNT] +NEW_START[,NEW_COUNT] @@`
fn parse_hunk_header_old(header: &str) -> usize {
    // Find the part between `@@` markers.
    let inner = header
        .strip_prefix("@@ ")
        .and_then(|s| s.split(" @@").next())
        .unwrap_or("");
    // Parse -a,b part.
    for part in inner.split_whitespace() {
        if let Some(rest) = part.strip_prefix('-') {
            let num_str = rest.split(',').next().unwrap_or(rest);
            return num_str.parse().unwrap_or(1);
        }
    }
    1
}

/// Strip `count` leading path components from a path.
fn strip_path_components(path: &str, count: usize) -> &str {
    if count == 0 {
        return path;
    }
    let mut remaining = path;
    for _ in 0..count {
        if let Some(pos) = remaining.find('/') {
            remaining = &remaining[pos + 1..];
        } else {
            return remaining;
        }
    }
    remaining
}

/// Apply a single hunk to a vector of lines. Returns `Ok(new_lines)` or `Err` on failure.
/// `old_start` is 1-based.
#[allow(clippy::cast_possible_wrap)]
fn apply_hunk(lines: &[String], hunk: &PatchHunk, fuzz: usize) -> Result<Vec<String>, String> {
    // Try at the expected position first, then fuzz +/- lines.
    let target_line = hunk.old_start.saturating_sub(1); // 0-based

    for offset in 0..=fuzz {
        for &direction in &[0i64, -(offset as i64), offset as i64] {
            if offset == 0 && direction != 0 {
                continue;
            }
            let try_pos = (target_line as i64 + direction) as usize;
            if try_pos > lines.len() {
                continue;
            }
            if try_apply_at(lines, hunk, try_pos).is_ok() {
                return try_apply_at(lines, hunk, try_pos);
            }
        }
    }

    Err(format!("hunk at line {} failed to apply", hunk.old_start))
}

/// Try to apply a hunk at a specific 0-based position in the line array.
fn try_apply_at(lines: &[String], hunk: &PatchHunk, pos: usize) -> Result<Vec<String>, String> {
    // Build the expected old-file segment from the body: context + remove lines in order.
    let mut expected_old: Vec<&str> = Vec::new();
    for op in &hunk.body {
        match op {
            PatchOp::Context(s) | PatchOp::Remove(s) => expected_old.push(s),
            PatchOp::Add(_) => {}
        }
    }

    let old_segment_len = expected_old.len();

    // Verify the expected old segment against the file.
    if pos + old_segment_len > lines.len() {
        return Err("hunk extends beyond end of file".into());
    }

    for (i, expected) in expected_old.iter().enumerate() {
        if lines[pos + i] != **expected {
            return Err(format!(
                "mismatch at line {}: expected {:?}, got {:?}",
                pos + i + 1,
                expected,
                lines[pos + i]
            ));
        }
    }

    // Build the result: lines before the segment, then the new-file segment
    // (context + add lines in their original interleaved order), then lines after.
    let mut result = Vec::with_capacity(lines.len() + hunk.add.len());
    result.extend_from_slice(&lines[..pos]);

    // Walk the body in order: emit context and add lines, skip remove lines.
    for op in &hunk.body {
        match op {
            PatchOp::Context(s) | PatchOp::Add(s) => result.push(s.clone()),
            PatchOp::Remove(_) => {}
        }
    }

    result.extend_from_slice(&lines[pos + old_segment_len..]);

    Ok(result)
}

struct PatchFlags<'a> {
    strip_count: usize,
    dry_run: bool,
    reverse: bool,
    patch_file: Option<&'a str>,
}

fn parse_patch_flags<'a>(
    ctx: &mut UtilContext<'_>,
    argv: &'a [&'a str],
) -> Result<PatchFlags<'a>, i32> {
    let mut flags = PatchFlags {
        strip_count: 0,
        dry_run: false,
        reverse: false,
        patch_file: None,
    };
    let mut args = &argv[1..];

    while let Some(&arg) = args.first() {
        let advance = parse_single_patch_flag(ctx, arg, args, &mut flags)?;
        if advance == 0 {
            break;
        }
        args = &args[advance..];
    }
    Ok(flags)
}

/// Parse one patch flag. Returns number of args consumed, or 0 to stop.
fn parse_single_patch_flag<'a>(
    ctx: &mut UtilContext<'_>,
    arg: &'a str,
    args: &[&'a str],
    flags: &mut PatchFlags<'a>,
) -> Result<usize, i32> {
    match arg {
        "--dry-run" => {
            flags.dry_run = true;
            Ok(1)
        }
        "-R" | "--reverse" => {
            flags.reverse = true;
            Ok(1)
        }
        "-i" => {
            if args.len() < 2 {
                ctx.output.stderr(b"patch: -i requires an argument\n");
                return Err(2);
            }
            flags.patch_file = Some(args[1]);
            Ok(2)
        }
        "--" => Ok(0),
        _ if arg.starts_with("-p") => {
            if let Ok(n) = arg[2..].parse::<usize>() {
                flags.strip_count = n;
            }
            Ok(1)
        }
        _ if arg.starts_with('-') && arg.len() > 1 => {
            let msg = format!("patch: unknown option '{arg}'\n");
            ctx.output.stderr(msg.as_bytes());
            Err(2)
        }
        _ => Ok(0),
    }
}

fn reverse_patch_hunks(patch_files: &mut [PatchFile]) {
    for pf in patch_files {
        for hunk in &mut pf.hunks {
            std::mem::swap(&mut hunk.remove, &mut hunk.add);
            for op in &mut hunk.body {
                *op = match op {
                    PatchOp::Remove(s) => PatchOp::Add(std::mem::take(s)),
                    PatchOp::Add(s) => PatchOp::Remove(std::mem::take(s)),
                    PatchOp::Context(s) => PatchOp::Context(std::mem::take(s)),
                };
            }
        }
    }
}

fn apply_patch_to_file(
    ctx: &mut UtilContext<'_>,
    pf: &PatchFile,
    strip_count: usize,
    dry_run: bool,
) -> i32 {
    let stripped = strip_path_components(&pf.path, strip_count);
    let target_path = resolve_path(ctx.cwd, stripped);

    let mut lines: Vec<String> = match read_text(ctx.fs, &target_path) {
        Ok(text) => text.lines().map(String::from).collect(),
        Err(_) => Vec::new(),
    };

    let mut hunk_failed = false;
    let mut status = 0;

    for (hi, hunk) in pf.hunks.iter().enumerate() {
        match apply_hunk(&lines, hunk, 3) {
            Ok(new_lines) => {
                lines = new_lines;
                let msg = format!("patching file {stripped} (hunk {} succeeded)\n", hi + 1);
                ctx.output.stdout(msg.as_bytes());
            }
            Err(e) => {
                let msg = format!("patching file {stripped} (hunk {} FAILED -- {e})\n", hi + 1);
                ctx.output.stderr(msg.as_bytes());
                hunk_failed = true;
                status = 1;
            }
        }
    }

    if dry_run || hunk_failed {
        return status;
    }

    let content = if lines.is_empty() {
        String::new()
    } else {
        let mut s = lines.join("\n");
        s.push('\n');
        s
    };
    let wh = match ctx.fs.open(&target_path, OpenOptions::write()) {
        Ok(h) => h,
        Err(e) => {
            emit_error(ctx.output, "patch", stripped, &e);
            return 2;
        }
    };
    if let Err(e) = ctx.fs.write_file(wh, content.as_bytes()) {
        ctx.fs.close(wh);
        emit_error(ctx.output, "patch", stripped, &e);
        return 2;
    }
    ctx.fs.close(wh);

    status
}

pub(crate) fn util_patch(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let flags = match parse_patch_flags(ctx, argv) {
        Ok(f) => f,
        Err(status) => return status,
    };

    // Read the patch text.
    let patch_text = if let Some(pf) = flags.patch_file {
        let full = resolve_path(ctx.cwd, pf);
        match read_text(ctx.fs, &full) {
            Ok(t) => t,
            Err(e) => {
                emit_error(ctx.output, "patch", pf, &e);
                return 2;
            }
        }
    } else if let Some(data) = ctx.stdin {
        String::from_utf8_lossy(data).to_string()
    } else {
        ctx.output.stderr(b"patch: no input\n");
        return 2;
    };

    let mut patch_files = parse_unified_diff(&patch_text);

    if flags.reverse {
        reverse_patch_hunks(&mut patch_files);
    }

    let mut status = 0;
    for pf in &patch_files {
        let rc = apply_patch_to_file(ctx, pf, flags.strip_count, flags.dry_run);
        if rc > status {
            status = rc;
        }
    }

    status
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UtilContext, VecOutput};
    use wasmsh_fs::{MemoryFs, OpenOptions, Vfs};

    fn make_fs() -> MemoryFs {
        MemoryFs::new()
    }

    fn write_file(fs: &mut MemoryFs, path: &str, content: &str) {
        let h = fs.open(path, OpenOptions::write()).unwrap();
        fs.write_file(h, content.as_bytes()).unwrap();
        fs.close(h);
    }

    fn run_diff(fs: &mut MemoryFs, argv: &[&str]) -> (i32, String, String) {
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs,
                output: &mut output,
                cwd: "/",
                stdin: None,
                state: None,
            };
            util_diff(&mut ctx, argv)
        };
        (
            status,
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )
    }

    fn run_patch(fs: &mut MemoryFs, argv: &[&str], stdin: Option<&[u8]>) -> (i32, String, String) {
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs,
                output: &mut output,
                cwd: "/",
                stdin,
                state: None,
            };
            util_patch(&mut ctx, argv)
        };
        (
            status,
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )
    }

    fn read_file(fs: &mut MemoryFs, path: &str) -> String {
        read_text(fs, path).unwrap()
    }

    // -----------------------------------------------------------------------
    // diff tests
    // -----------------------------------------------------------------------

    #[test]
    fn diff_identical_files() {
        let mut fs = make_fs();
        write_file(&mut fs, "/a.txt", "hello\nworld\n");
        write_file(&mut fs, "/b.txt", "hello\nworld\n");
        let (status, stdout, _) = run_diff(&mut fs, &["diff", "/a.txt", "/b.txt"]);
        assert_eq!(status, 0);
        assert!(stdout.is_empty());
    }

    #[test]
    fn diff_different_files_normal() {
        let mut fs = make_fs();
        write_file(&mut fs, "/a.txt", "alpha\nbeta\ngamma\n");
        write_file(&mut fs, "/b.txt", "alpha\ndelta\ngamma\n");
        let (status, stdout, _) = run_diff(&mut fs, &["diff", "/a.txt", "/b.txt"]);
        assert_eq!(status, 1);
        assert!(stdout.contains('c'), "expected change marker: {stdout}");
        assert!(stdout.contains("< beta"), "expected old line: {stdout}");
        assert!(stdout.contains("> delta"), "expected new line: {stdout}");
    }

    #[test]
    fn diff_unified_output() {
        let mut fs = make_fs();
        write_file(&mut fs, "/a.txt", "one\ntwo\nthree\n");
        write_file(&mut fs, "/b.txt", "one\nTWO\nthree\n");
        let (status, stdout, _) = run_diff(&mut fs, &["diff", "-u", "/a.txt", "/b.txt"]);
        assert_eq!(status, 1);
        assert!(
            stdout.contains("--- /a.txt"),
            "expected old header: {stdout}"
        );
        assert!(
            stdout.contains("+++ /b.txt"),
            "expected new header: {stdout}"
        );
        assert!(stdout.contains("@@"), "expected hunk header: {stdout}");
        assert!(stdout.contains("-two"), "expected removed line: {stdout}");
        assert!(stdout.contains("+TWO"), "expected added line: {stdout}");
    }

    #[test]
    fn diff_context_output() {
        let mut fs = make_fs();
        write_file(&mut fs, "/a.txt", "aaa\nbbb\nccc\n");
        write_file(&mut fs, "/b.txt", "aaa\nBBB\nccc\n");
        let (status, stdout, _) = run_diff(&mut fs, &["diff", "-c", "/a.txt", "/b.txt"]);
        assert_eq!(status, 1);
        assert!(
            stdout.contains("*** /a.txt"),
            "expected old header: {stdout}"
        );
        assert!(
            stdout.contains("--- /b.txt"),
            "expected new header: {stdout}"
        );
        assert!(stdout.contains("- bbb"), "expected old line: {stdout}");
        assert!(stdout.contains("+ BBB"), "expected new line: {stdout}");
    }

    #[test]
    fn diff_brief() {
        let mut fs = make_fs();
        write_file(&mut fs, "/a.txt", "hello\n");
        write_file(&mut fs, "/b.txt", "world\n");
        let (status, stdout, _) = run_diff(&mut fs, &["diff", "-q", "/a.txt", "/b.txt"]);
        assert_eq!(status, 1);
        assert!(
            stdout.contains("differ"),
            "expected differ message: {stdout}"
        );
    }

    #[test]
    fn diff_brief_identical() {
        let mut fs = make_fs();
        write_file(&mut fs, "/a.txt", "same\n");
        write_file(&mut fs, "/b.txt", "same\n");
        let (status, stdout, _) = run_diff(&mut fs, &["diff", "-q", "/a.txt", "/b.txt"]);
        assert_eq!(status, 0);
        assert!(stdout.is_empty());
    }

    #[test]
    fn diff_ignore_all_space() {
        let mut fs = make_fs();
        write_file(&mut fs, "/a.txt", "hello world\n");
        write_file(&mut fs, "/b.txt", "helloworld\n");
        let (status, _, _) = run_diff(&mut fs, &["diff", "-w", "/a.txt", "/b.txt"]);
        assert_eq!(status, 0, "whitespace-only difference should be ignored");
    }

    #[test]
    fn diff_ignore_blank_lines() {
        let mut fs = make_fs();
        write_file(&mut fs, "/a.txt", "one\n\ntwo\n");
        write_file(&mut fs, "/b.txt", "one\ntwo\n");
        let (status, _, _) = run_diff(&mut fs, &["diff", "-B", "/a.txt", "/b.txt"]);
        assert_eq!(status, 0, "blank-line-only difference should be ignored");
    }

    #[test]
    fn diff_new_file_flag() {
        let mut fs = make_fs();
        write_file(&mut fs, "/a.txt", "content\n");
        let (status, stdout, _) =
            run_diff(&mut fs, &["diff", "-N", "-u", "/a.txt", "/missing.txt"]);
        assert_eq!(status, 1);
        assert!(stdout.contains("-content"), "expected deletion: {stdout}");
    }

    #[test]
    fn diff_missing_file_error() {
        let mut fs = make_fs();
        write_file(&mut fs, "/a.txt", "content\n");
        let (status, _, stderr) = run_diff(&mut fs, &["diff", "/a.txt", "/nope.txt"]);
        assert_eq!(status, 2);
        assert!(!stderr.is_empty());
    }

    #[test]
    fn diff_insertion() {
        let mut fs = make_fs();
        write_file(&mut fs, "/a.txt", "one\nthree\n");
        write_file(&mut fs, "/b.txt", "one\ntwo\nthree\n");
        let (status, stdout, _) = run_diff(&mut fs, &["diff", "/a.txt", "/b.txt"]);
        assert_eq!(status, 1);
        assert!(stdout.contains("> two"), "expected insertion: {stdout}");
    }

    #[test]
    fn diff_deletion() {
        let mut fs = make_fs();
        write_file(&mut fs, "/a.txt", "one\ntwo\nthree\n");
        write_file(&mut fs, "/b.txt", "one\nthree\n");
        let (status, stdout, _) = run_diff(&mut fs, &["diff", "/a.txt", "/b.txt"]);
        assert_eq!(status, 1);
        assert!(stdout.contains("< two"), "expected deletion: {stdout}");
    }

    #[test]
    fn diff_empty_to_content() {
        let mut fs = make_fs();
        write_file(&mut fs, "/a.txt", "");
        write_file(&mut fs, "/b.txt", "hello\n");
        let (status, stdout, _) = run_diff(&mut fs, &["diff", "-u", "/a.txt", "/b.txt"]);
        assert_eq!(status, 1);
        assert!(stdout.contains("+hello"), "expected addition: {stdout}");
    }

    #[test]
    fn diff_content_to_empty() {
        let mut fs = make_fs();
        write_file(&mut fs, "/a.txt", "hello\n");
        write_file(&mut fs, "/b.txt", "");
        let (status, stdout, _) = run_diff(&mut fs, &["diff", "-u", "/a.txt", "/b.txt"]);
        assert_eq!(status, 1);
        assert!(stdout.contains("-hello"), "expected removal: {stdout}");
    }

    #[test]
    fn diff_recursive() {
        let mut fs = make_fs();
        fs.create_dir("/dir_a").unwrap();
        fs.create_dir("/dir_b").unwrap();
        write_file(&mut fs, "/dir_a/f.txt", "old\n");
        write_file(&mut fs, "/dir_b/f.txt", "new\n");
        let (status, stdout, _) = run_diff(&mut fs, &["diff", "-r", "-u", "/dir_a", "/dir_b"]);
        assert_eq!(status, 1);
        assert!(stdout.contains("-old"), "expected old line: {stdout}");
        assert!(stdout.contains("+new"), "expected new line: {stdout}");
    }

    #[test]
    fn diff_combined_flags() {
        let mut fs = make_fs();
        write_file(&mut fs, "/a.txt", "x\n");
        write_file(&mut fs, "/b.txt", "y\n");
        let (status, stdout, _) = run_diff(&mut fs, &["diff", "-uq", "/a.txt", "/b.txt"]);
        // -q takes precedence in brief mode
        assert_eq!(status, 1);
        assert!(stdout.contains("differ"));
    }

    #[test]
    fn diff_missing_operand() {
        let mut fs = make_fs();
        let (status, _, stderr) = run_diff(&mut fs, &["diff"]);
        assert_eq!(status, 2);
        assert!(stderr.contains("missing operand"));
    }

    // -----------------------------------------------------------------------
    // patch tests
    // -----------------------------------------------------------------------

    #[test]
    fn patch_simple_unified() {
        let mut fs = make_fs();
        write_file(&mut fs, "/target.txt", "one\ntwo\nthree\n");
        let patch = "\
--- /target.txt
+++ /target.txt
@@ -1,3 +1,3 @@
 one
-two
+TWO
 three
";
        write_file(&mut fs, "/patch.diff", patch);
        let (status, stdout, _) = run_patch(&mut fs, &["patch", "-i", "/patch.diff"], None);
        assert_eq!(status, 0, "patch should succeed");
        assert!(stdout.contains("succeeded"));
        let content = read_file(&mut fs, "/target.txt");
        assert!(content.contains("TWO"), "patched content: {content}");
        assert!(
            !content.contains("\ntwo\n"),
            "old line should be gone: {content}"
        );
    }

    #[test]
    fn patch_from_stdin() {
        let mut fs = make_fs();
        write_file(&mut fs, "/file.txt", "aaa\nbbb\nccc\n");
        let patch = "\
--- /file.txt
+++ /file.txt
@@ -1,3 +1,3 @@
 aaa
-bbb
+BBB
 ccc
";
        let (status, _, _) = run_patch(&mut fs, &["patch"], Some(patch.as_bytes()));
        assert_eq!(status, 0);
        let content = read_file(&mut fs, "/file.txt");
        assert!(content.contains("BBB"));
    }

    #[test]
    fn patch_strip_path() {
        let mut fs = make_fs();
        write_file(&mut fs, "/hello.txt", "old\n");
        let patch = "\
--- a/hello.txt
+++ b/hello.txt
@@ -1 +1 @@
-old
+new
";
        write_file(&mut fs, "/p.diff", patch);
        let (status, _, _) = run_patch(&mut fs, &["patch", "-p1", "-i", "/p.diff"], None);
        assert_eq!(status, 0);
        let content = read_file(&mut fs, "/hello.txt");
        assert_eq!(content.trim(), "new");
    }

    #[test]
    fn patch_reverse() {
        let mut fs = make_fs();
        // The patch says: remove "old", add "new".
        // With -R, it should remove "new" and add "old".
        write_file(&mut fs, "/r.txt", "new\n");
        let patch = "\
--- /r.txt
+++ /r.txt
@@ -1 +1 @@
-old
+new
";
        write_file(&mut fs, "/r.diff", patch);
        let (status, _, _) = run_patch(&mut fs, &["patch", "-R", "-i", "/r.diff"], None);
        assert_eq!(status, 0);
        let content = read_file(&mut fs, "/r.txt");
        assert_eq!(content.trim(), "old");
    }

    #[test]
    fn patch_dry_run() {
        let mut fs = make_fs();
        write_file(&mut fs, "/d.txt", "before\n");
        let patch = "\
--- /d.txt
+++ /d.txt
@@ -1 +1 @@
-before
+after
";
        write_file(&mut fs, "/d.diff", patch);
        let (status, _, _) = run_patch(&mut fs, &["patch", "--dry-run", "-i", "/d.diff"], None);
        assert_eq!(status, 0);
        // File should be unchanged.
        let content = read_file(&mut fs, "/d.txt");
        assert_eq!(content.trim(), "before");
    }

    #[test]
    fn patch_creates_new_file() {
        let mut fs = make_fs();
        let patch = "\
--- /dev/null
+++ /brand_new.txt
@@ -0,0 +1,2 @@
+hello
+world
";
        write_file(&mut fs, "/new.diff", patch);
        let (status, _, _) = run_patch(&mut fs, &["patch", "-i", "/new.diff"], None);
        assert_eq!(status, 0);
        let content = read_file(&mut fs, "/brand_new.txt");
        assert!(content.contains("hello"));
        assert!(content.contains("world"));
    }

    #[test]
    fn patch_multi_hunk() {
        let mut fs = make_fs();
        write_file(&mut fs, "/multi.txt", "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n");
        let patch = "\
--- /multi.txt
+++ /multi.txt
@@ -1,4 +1,4 @@
-1
+ONE
 2
 3
 4
@@ -7,4 +7,4 @@
 7
 8
-9
+NINE
 10
";
        write_file(&mut fs, "/multi.diff", patch);
        let (status, stdout, _) = run_patch(&mut fs, &["patch", "-i", "/multi.diff"], None);
        assert_eq!(status, 0, "multi-hunk patch should succeed");
        assert!(stdout.contains("hunk 1 succeeded"));
        assert!(stdout.contains("hunk 2 succeeded"));
        let content = read_file(&mut fs, "/multi.txt");
        assert!(content.contains("ONE"), "first hunk: {content}");
        assert!(content.contains("NINE"), "second hunk: {content}");
        assert!(!content.contains("\n1\n"), "old line 1 gone: {content}");
    }

    #[test]
    fn patch_no_input() {
        let mut fs = make_fs();
        let (status, _, stderr) = run_patch(&mut fs, &["patch"], None);
        assert_eq!(status, 2);
        assert!(stderr.contains("no input"));
    }

    // -----------------------------------------------------------------------
    // Round-trip: diff then patch
    // -----------------------------------------------------------------------

    #[test]
    fn roundtrip_diff_then_patch() {
        let mut fs = make_fs();
        let original = "line1\nline2\nline3\nline4\nline5\n";
        let modified = "line1\nLINE2\nline3\nline4\nline5\nextra\n";
        write_file(&mut fs, "/orig.txt", original);
        write_file(&mut fs, "/mod.txt", modified);

        // Generate unified diff.
        let (diff_status, diff_output, _) =
            run_diff(&mut fs, &["diff", "-u", "/orig.txt", "/mod.txt"]);
        assert_eq!(diff_status, 1, "files should differ");

        // Write the diff to a file and apply it.
        write_file(&mut fs, "/rt.diff", &diff_output);

        // Reset orig.txt and apply the patch.
        write_file(&mut fs, "/orig.txt", original);
        let (patch_status, _, _) = run_patch(&mut fs, &["patch", "-i", "/rt.diff"], None);
        assert_eq!(patch_status, 0, "patch should apply cleanly");

        let patched = read_file(&mut fs, "/orig.txt");
        assert_eq!(patched, modified, "patched file should match modified");
    }

    // -----------------------------------------------------------------------
    // LCS / edit-script unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn edit_script_identical() {
        let a = vec!["x", "y", "z"];
        let ops = compute_edit_script(&a, &a);
        assert!(ops.iter().all(|(op, _, _)| *op == EditOp::Equal));
    }

    #[test]
    fn edit_script_all_different() {
        let a = vec!["a", "b"];
        let b = vec!["c", "d"];
        let ops = compute_edit_script(&a, &b);
        let deletes = ops
            .iter()
            .filter(|(op, _, _)| *op == EditOp::Delete)
            .count();
        let inserts = ops
            .iter()
            .filter(|(op, _, _)| *op == EditOp::Insert)
            .count();
        assert_eq!(deletes, 2);
        assert_eq!(inserts, 2);
    }

    #[test]
    fn edit_script_insertion() {
        let a = vec!["a", "c"];
        let b = vec!["a", "b", "c"];
        let ops = compute_edit_script(&a, &b);
        let inserts = ops
            .iter()
            .filter(|(op, _, _)| *op == EditOp::Insert)
            .count();
        assert_eq!(inserts, 1);
    }

    #[test]
    fn edit_script_deletion() {
        let a = vec!["a", "b", "c"];
        let b = vec!["a", "c"];
        let ops = compute_edit_script(&a, &b);
        let deletes = ops
            .iter()
            .filter(|(op, _, _)| *op == EditOp::Delete)
            .count();
        assert_eq!(deletes, 1);
    }

    #[test]
    fn strip_path_components_test() {
        assert_eq!(strip_path_components("a/b/c.txt", 0), "a/b/c.txt");
        assert_eq!(strip_path_components("a/b/c.txt", 1), "b/c.txt");
        assert_eq!(strip_path_components("a/b/c.txt", 2), "c.txt");
        assert_eq!(strip_path_components("a/b/c.txt", 5), "c.txt");
    }
}
