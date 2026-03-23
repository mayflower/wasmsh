//! Shared helper functions used across utility modules.

use wasmsh_fs::{FsError, MemoryFs, OpenOptions, Vfs};

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

pub(crate) fn copy_file_contents(fs: &mut MemoryFs, src: &str, dst: &str) -> Result<(), String> {
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

pub(crate) fn read_text(fs: &mut MemoryFs, path: &str) -> Result<String, FsError> {
    let h = fs.open(path, OpenOptions::read())?;
    let data = fs.read_file(h)?;
    fs.close(h);
    String::from_utf8(data).map_err(|_| FsError::Io("invalid utf-8".into()))
}

/// Get input text from file args or stdin.
pub(crate) fn get_input_text(ctx: &mut UtilContext<'_>, file_args: &[&str]) -> String {
    if file_args.is_empty() {
        if let Some(data) = ctx.stdin {
            return String::from_utf8_lossy(data).to_string();
        }
        String::new()
    } else {
        let full = resolve_path(ctx.cwd, file_args[0]);
        read_text(ctx.fs, &full).unwrap_or_default()
    }
}

/// Parse `-n N` or `-N` line count from argv. Returns (count, remaining files).
/// Returns (count, `from_start`, files). `from_start=true` means `+N` syntax.
pub(crate) fn parse_line_count<'a>(
    argv: &'a [&'a str],
    default: usize,
) -> (usize, bool, Vec<&'a str>) {
    let args = &argv[1..];
    if args.is_empty() {
        return (default, false, vec![]);
    }
    if args[0] == "-n" && args.len() >= 2 {
        if let Some(rest) = args[1].strip_prefix('+') {
            let n = rest.parse().unwrap_or(1);
            return (n, true, args[2..].to_vec());
        }
        let n = args[1].parse().unwrap_or(default);
        return (n, false, args[2..].to_vec());
    }
    if let Some(rest) = args[0].strip_prefix('-') {
        if let Ok(n) = rest.parse::<usize>() {
            return (n, false, args[1..].to_vec());
        }
    }
    if let Some(rest) = args[0].strip_prefix('+') {
        if let Ok(n) = rest.parse::<usize>() {
            return (n, true, args[1..].to_vec());
        }
    }
    (default, false, args.to_vec())
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
