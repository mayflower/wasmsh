//! Shell builtin commands for wasmsh.
//!
//! Builtins run in-process and can modify shell state directly.
//! Output goes through an `OutputSink` abstraction suitable for
//! browser streaming.

use indexmap::IndexMap;
use smol_str::SmolStr;
use wasmsh_fs::Vfs;
use wasmsh_state::{ShellState, ShellVar, VarValue};

/// Abstraction for stdout/stderr output, suitable for browser streaming.
pub trait OutputSink {
    fn stdout(&mut self, data: &[u8]);
    fn stderr(&mut self, data: &[u8]);
}

/// An `OutputSink` that collects output into byte vectors (for testing).
#[derive(Debug, Default, Clone)]
pub struct VecSink {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl OutputSink for VecSink {
    fn stdout(&mut self, data: &[u8]) {
        self.stdout.extend_from_slice(data);
    }
    fn stderr(&mut self, data: &[u8]) {
        self.stderr.extend_from_slice(data);
    }
}

impl VecSink {
    #[must_use]
    pub fn stdout_str(&self) -> &str {
        std::str::from_utf8(&self.stdout).unwrap_or("<invalid utf-8>")
    }
    #[must_use]
    pub fn stderr_str(&self) -> &str {
        std::str::from_utf8(&self.stderr).unwrap_or("<invalid utf-8>")
    }
}

/// Context passed to builtin implementations.
pub struct BuiltinContext<'a> {
    pub state: &'a mut ShellState,
    pub output: &'a mut dyn OutputSink,
    /// Optional VFS access (needed by `test -f`, etc.).
    pub fs: Option<&'a dyn Vfs>,
    /// Stdin data from pipe or here-doc.
    pub stdin: Option<&'a [u8]>,
}

impl std::fmt::Debug for BuiltinContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltinContext").finish_non_exhaustive()
    }
}

/// Signature for a builtin command function.
/// Receives the context and argv (argv\[0\] is the command name).
/// Returns the exit status.
pub type BuiltinFn = fn(&mut BuiltinContext<'_>, &[&str]) -> i32;

/// Registry of builtin commands.
pub struct BuiltinRegistry {
    builtins: IndexMap<&'static str, BuiltinFn>,
}

impl std::fmt::Debug for BuiltinRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltinRegistry")
            .field("count", &self.builtins.len())
            .finish()
    }
}

impl BuiltinRegistry {
    /// Create a registry with all standard builtins.
    #[must_use]
    pub fn new() -> Self {
        let mut builtins = IndexMap::<&'static str, BuiltinFn>::new();
        builtins.insert(":", builtin_colon);
        builtins.insert("true", builtin_true);
        builtins.insert("false", builtin_false);
        builtins.insert("echo", builtin_echo);
        builtins.insert("printf", builtin_printf);
        builtins.insert("pwd", builtin_pwd);
        builtins.insert("cd", builtin_cd);
        builtins.insert("export", builtin_export);
        builtins.insert("unset", builtin_unset);
        builtins.insert("readonly", builtin_readonly);
        builtins.insert("test", builtin_test);
        builtins.insert("[", builtin_test);
        builtins.insert("read", builtin_read);
        builtins.insert("shift", builtin_shift);
        builtins.insert("return", builtin_return);
        builtins.insert("exit", builtin_exit);
        builtins.insert("local", builtin_local);
        builtins.insert("type", builtin_type);
        builtins.insert("command", builtin_command);
        builtins.insert("eval", builtin_eval);
        builtins.insert("set", builtin_set);
        builtins.insert("getopts", builtin_getopts);
        builtins.insert("trap", builtin_trap);
        Self { builtins }
    }

    /// Look up a builtin by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<BuiltinFn> {
        self.builtins.get(name).copied()
    }

    /// Check if a name is a builtin.
    #[must_use]
    pub fn is_builtin(&self, name: &str) -> bool {
        self.builtins.contains_key(name)
    }
}

impl Default for BuiltinRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---- Builtin implementations ----

/// `:` — no-op, always returns 0.
fn builtin_colon(_ctx: &mut BuiltinContext<'_>, _argv: &[&str]) -> i32 {
    0
}

/// `true` — always returns 0.
fn builtin_true(_ctx: &mut BuiltinContext<'_>, _argv: &[&str]) -> i32 {
    0
}

/// `false` — always returns 1.
fn builtin_false(_ctx: &mut BuiltinContext<'_>, _argv: &[&str]) -> i32 {
    1
}

/// `echo` — print arguments separated by spaces.
/// Supports `-n` to suppress trailing newline and `-e` for escape interpretation.
fn builtin_echo(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    let args = &argv[1..];
    let mut suppress_newline = false;
    let mut interpret_escapes = false;
    let mut start = 0;

    for (i, arg) in args.iter().enumerate() {
        // Each flag-like argument must consist entirely of valid echo flags
        let bytes = arg.as_bytes();
        if bytes.first() != Some(&b'-') || bytes.len() < 2 {
            break;
        }
        let all_flags = bytes[1..].iter().all(|b| matches!(b, b'n' | b'e'));
        if !all_flags {
            break;
        }
        for &b in &bytes[1..] {
            match b {
                b'n' => suppress_newline = true,
                b'e' => interpret_escapes = true,
                _ => {}
            }
        }
        start = i + 1;
    }

    let text = args[start..].join(" ");
    if interpret_escapes {
        let processed = process_echo_escapes(&text);
        ctx.output.stdout(processed.as_bytes());
    } else {
        ctx.output.stdout(text.as_bytes());
    }
    if !suppress_newline {
        ctx.output.stdout(b"\n");
    }
    0
}

fn process_echo_escapes(s: &str) -> String {
    let mut result = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'n' => {
                    result.push('\n');
                    i += 2;
                }
                b't' => {
                    result.push('\t');
                    i += 2;
                }
                b'\\' => {
                    result.push('\\');
                    i += 2;
                }
                b'a' => {
                    result.push('\x07');
                    i += 2;
                }
                b'b' => {
                    result.push('\x08');
                    i += 2;
                }
                b'r' => {
                    result.push('\r');
                    i += 2;
                }
                b'0' => {
                    i += 2;
                    let mut val: u8 = 0;
                    let mut count = 0;
                    while i < bytes.len() && count < 3 && bytes[i] >= b'0' && bytes[i] <= b'7' {
                        val = val * 8 + (bytes[i] - b'0');
                        i += 1;
                        count += 1;
                    }
                    result.push(val as char);
                }
                _ => {
                    result.push('\\');
                    result.push(bytes[i + 1] as char);
                    i += 2;
                }
            }
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    result
}

/// `printf` — formatted output.
/// Supports: `%s`, `%d`, `%x`, `%o`, `%f`, `%c`, `%b`, `%q`, `%%`, width/precision,
/// left-align (`%-`), zero-pad (`%0`), and `\n`, `\t`, `\\` escape sequences.
/// Repeats the format string while there are remaining arguments (POSIX behavior).
fn builtin_printf(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    if argv.len() < 2 {
        ctx.output
            .stderr(b"printf: usage: printf format [arguments]\n");
        return 1;
    }

    let format = argv[1];
    let args = &argv[2..];
    let mut arg_idx = 0;
    let mut output = String::new();
    let bytes = format.as_bytes();

    loop {
        let start_arg_idx = arg_idx;
        printf_format_once(bytes, args, &mut arg_idx, &mut output);
        if arg_idx == start_arg_idx || arg_idx >= args.len() {
            break;
        }
    }

    ctx.output.stdout(output.as_bytes());
    0
}

/// Process one pass of a printf format string.
fn printf_format_once(bytes: &[u8], args: &[&str], arg_idx: &mut usize, output: &mut String) {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'%' {
                output.push('%');
                i += 2;
                continue;
            }
            i += 1;
            i = printf_format_spec(bytes, i, args, arg_idx, output);
        } else if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i = printf_escape(bytes, i, output);
        } else {
            output.push(bytes[i] as char);
            i += 1;
        }
    }
}

/// Parse and apply a single printf format specifier starting after the `%`.
/// Returns the new position in the format byte string.
fn printf_format_spec(
    bytes: &[u8],
    mut i: usize,
    args: &[&str],
    arg_idx: &mut usize,
    output: &mut String,
) -> usize {
    let mut left_align = false;
    let mut zero_pad = false;
    loop {
        if i < bytes.len() && bytes[i] == b'-' {
            left_align = true;
            i += 1;
        } else if i < bytes.len() && bytes[i] == b'0' && !left_align {
            zero_pad = true;
            i += 1;
        } else {
            break;
        }
    }
    let mut width: usize = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        width = width * 10 + (bytes[i] - b'0') as usize;
        i += 1;
    }
    let mut precision: Option<usize> = None;
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        let mut prec: usize = 0;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            prec = prec * 10 + (bytes[i] - b'0') as usize;
            i += 1;
        }
        precision = Some(prec);
    }
    if i >= bytes.len() {
        output.push('%');
        return i;
    }
    let conv = bytes[i];
    i += 1;
    let arg_str = args.get(*arg_idx).copied().unwrap_or("");
    let formatted = printf_convert(conv, arg_str, precision, arg_idx);
    printf_apply_width(output, &formatted, width, left_align, zero_pad);
    i
}

/// Convert a single printf format conversion character.
fn printf_convert(
    conv: u8,
    arg_str: &str,
    precision: Option<usize>,
    arg_idx: &mut usize,
) -> String {
    match conv {
        b's' => {
            *arg_idx += 1;
            let s = precision.map_or(arg_str, |prec| {
                if prec < arg_str.len() {
                    &arg_str[..prec]
                } else {
                    arg_str
                }
            });
            s.to_string()
        }
        b'd' => {
            *arg_idx += 1;
            arg_str.parse::<i64>().unwrap_or(0).to_string()
        }
        b'x' => {
            *arg_idx += 1;
            format!("{:x}", arg_str.parse::<i64>().unwrap_or(0))
        }
        b'o' => {
            *arg_idx += 1;
            format!("{:o}", arg_str.parse::<i64>().unwrap_or(0))
        }
        b'f' => {
            *arg_idx += 1;
            let val: f64 = arg_str.parse().unwrap_or(0.0);
            let prec = precision.unwrap_or(6);
            format!("{val:.prec$}")
        }
        b'c' => {
            *arg_idx += 1;
            arg_str
                .chars()
                .next()
                .map_or(String::new(), |c| c.to_string())
        }
        b'b' => {
            *arg_idx += 1;
            process_printf_backslash_escapes(arg_str)
        }
        b'q' => {
            *arg_idx += 1;
            shell_quote(arg_str)
        }
        _ => format!("%{}", conv as char),
    }
}

/// Apply width and alignment to a formatted string.
fn printf_apply_width(
    output: &mut String,
    formatted: &str,
    width: usize,
    left_align: bool,
    zero_pad: bool,
) {
    if width == 0 || formatted.len() >= width {
        output.push_str(formatted);
        return;
    }

    let pad_char = if zero_pad && !left_align { '0' } else { ' ' };
    let padding = width - formatted.len();
    if left_align {
        output.push_str(formatted);
        push_repeated_char(output, ' ', padding);
    } else {
        push_repeated_char(output, pad_char, padding);
        output.push_str(formatted);
    }
}

fn push_repeated_char(output: &mut String, ch: char, count: usize) {
    for _ in 0..count {
        output.push(ch);
    }
}

/// Process a backslash escape in a printf format string. Returns new position.
fn printf_escape(bytes: &[u8], i: usize, output: &mut String) -> usize {
    match bytes[i + 1] {
        b'n' => {
            output.push('\n');
            i + 2
        }
        b't' => {
            output.push('\t');
            i + 2
        }
        b'\\' => {
            output.push('\\');
            i + 2
        }
        b'r' => {
            output.push('\r');
            i + 2
        }
        b'a' => {
            output.push('\x07');
            i + 2
        }
        b'b' => {
            output.push('\x08');
            i + 2
        }
        _ => {
            output.push('\\');
            i + 1
        }
    }
}

/// Process backslash escape sequences for `%b` in printf.
fn process_printf_backslash_escapes(s: &str) -> String {
    let mut result = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'n' => {
                    result.push('\n');
                    i += 2;
                }
                b't' => {
                    result.push('\t');
                    i += 2;
                }
                b'\\' => {
                    result.push('\\');
                    i += 2;
                }
                b'a' => {
                    result.push('\x07');
                    i += 2;
                }
                b'b' => {
                    result.push('\x08');
                    i += 2;
                }
                b'r' => {
                    result.push('\r');
                    i += 2;
                }
                b'0' => {
                    i += 2;
                    let mut val: u8 = 0;
                    let mut count = 0;
                    while i < bytes.len() && count < 3 && bytes[i] >= b'0' && bytes[i] <= b'7' {
                        val = val * 8 + (bytes[i] - b'0');
                        i += 1;
                        count += 1;
                    }
                    result.push(val as char);
                }
                _ => {
                    result.push('\\');
                    result.push(bytes[i + 1] as char);
                    i += 2;
                }
            }
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    result
}

/// Shell-quote a string for `%q` in printf: wrap in $'...' with escapes.
fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    // Check if the string needs quoting
    let needs_quoting = s
        .bytes()
        .any(|b| !b.is_ascii_alphanumeric() && !matches!(b, b'_' | b'-' | b'.' | b'/' | b':'));
    if !needs_quoting {
        return s.to_string();
    }
    let mut result = String::from("$'");
    for ch in s.chars() {
        match ch {
            '\'' => result.push_str("\\'"),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\t' => result.push_str("\\t"),
            '\r' => result.push_str("\\r"),
            '\x07' => result.push_str("\\a"),
            '\x08' => result.push_str("\\b"),
            _ => result.push(ch),
        }
    }
    result.push('\'');
    result
}

/// `pwd` — print working directory.
fn builtin_pwd(ctx: &mut BuiltinContext<'_>, _argv: &[&str]) -> i32 {
    ctx.output.stdout(ctx.state.cwd.as_bytes());
    ctx.output.stdout(b"\n");
    0
}

/// `cd` — change working directory.
/// - `cd` (no args): go to HOME
/// - `cd -`: go to OLDPWD
/// - `cd DIR`: set cwd to DIR
fn builtin_cd(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    let target = if argv.len() < 2 {
        // cd with no args → HOME
        if let Some(home) = ctx.state.get_var("HOME") {
            home.to_string()
        } else {
            ctx.output.stderr(b"cd: HOME not set\n");
            return 1;
        }
    } else if argv[1] == "-" {
        if let Some(old) = ctx.state.get_var("OLDPWD") {
            let s = old.to_string();
            ctx.output.stdout(s.as_bytes());
            ctx.output.stdout(b"\n");
            s
        } else {
            ctx.output.stderr(b"cd: OLDPWD not set\n");
            return 1;
        }
    } else {
        argv[1].to_string()
    };

    let old_pwd = ctx.state.cwd.clone();
    ctx.state.cwd = target;
    ctx.state.set_var("OLDPWD".into(), SmolStr::from(old_pwd));
    ctx.state
        .set_var("PWD".into(), SmolStr::from(ctx.state.cwd.as_str()));
    0
}

/// `export` — mark variables as exported.
/// - `export NAME=VALUE`: set and export
/// - `export NAME`: export existing variable
fn builtin_export(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    for arg in &argv[1..] {
        if let Some(eq_pos) = arg.find('=') {
            let name = &arg[..eq_pos];
            let value = &arg[eq_pos + 1..];
            if let Some(existing) = ctx.state.env.get(name) {
                if existing.readonly {
                    let msg = format!("export: {name}: readonly variable\n");
                    ctx.output.stderr(msg.as_bytes());
                    continue;
                }
            }
            ctx.state.env.set(
                SmolStr::from(name),
                ShellVar {
                    value: VarValue::Scalar(SmolStr::from(value)),
                    exported: true,
                    readonly: false,
                    integer: false,
                    nameref: false,
                },
            );
        } else {
            // Export existing variable
            if let Some(var) = ctx.state.env.get(arg) {
                let mut var = var.clone();
                var.exported = true;
                ctx.state.env.set(SmolStr::from(*arg), var);
            } else {
                // Create empty exported variable
                ctx.state.env.set(
                    SmolStr::from(*arg),
                    ShellVar {
                        value: VarValue::Scalar(SmolStr::default()),
                        exported: true,
                        readonly: false,
                        integer: false,
                        nameref: false,
                    },
                );
            }
        }
    }
    0
}

/// `unset` — remove variables from the environment.
/// Supports `unset 'arr[N]'` to remove a single array element.
fn builtin_unset(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    let mut status = 0;
    for name in &argv[1..] {
        // Check for array element syntax: name[index]
        if let Some(bracket_pos) = name.find('[') {
            if name.ends_with(']') {
                let base = &name[..bracket_pos];
                let index = &name[bracket_pos + 1..name.len() - 1];
                ctx.state.unset_array_element(base, index);
                continue;
            }
        }
        if let Err(e) = ctx.state.unset_var(name) {
            let msg = format!("unset: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            status = 1;
        }
    }
    status
}

/// `readonly` — mark variables as readonly.
/// - `readonly NAME=VALUE`: set and mark readonly
/// - `readonly NAME`: mark existing variable readonly
fn builtin_readonly(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    for arg in &argv[1..] {
        if let Some(eq_pos) = arg.find('=') {
            let name = &arg[..eq_pos];
            let value = &arg[eq_pos + 1..];
            ctx.state
                .set_readonly(SmolStr::from(name), SmolStr::from(value));
        } else {
            // Mark existing variable readonly
            let value = ctx.state.get_var(arg).unwrap_or_default();
            ctx.state.set_readonly(SmolStr::from(*arg), value);
        }
    }
    0
}

/// `test` / `[` — conditional expression evaluation.
fn builtin_test(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    let args: Vec<&str> = if argv.first() == Some(&"[") {
        if argv.last() != Some(&"]") {
            ctx.output.stderr(b"[: missing ']'\n");
            return 2;
        }
        argv[1..argv.len() - 1].to_vec()
    } else {
        argv[1..].to_vec()
    };

    if args.is_empty() {
        return 1;
    }

    i32::from(!test_check(&args, ctx))
}

fn test_check(args: &[&str], ctx: &BuiltinContext<'_>) -> bool {
    // Handle `!` prefix at any arg count (e.g. `! -f /path`, `! "a" = "b"`)
    if !args.is_empty() && args[0] == "!" {
        return !test_check(&args[1..], ctx);
    }
    match args.len() {
        1 => !args[0].is_empty(),
        2 => test_unary(args[0], args[1], ctx),
        3 => test_binary(args[0], args[1], args[2]),
        _ => false,
    }
}

fn test_unary(op: &str, val: &str, ctx: &BuiltinContext<'_>) -> bool {
    match op {
        "-n" => !val.is_empty(),
        "-z" | "!" => val.is_empty(),
        "-f" => ctx
            .fs
            .is_some_and(|fs| fs.stat(val).is_ok_and(|m| !m.is_dir)),
        "-d" => ctx
            .fs
            .is_some_and(|fs| fs.stat(val).is_ok_and(|m| m.is_dir)),
        "-e" => ctx.fs.is_some_and(|fs| fs.stat(val).is_ok()),
        "-s" => ctx
            .fs
            .is_some_and(|fs| fs.stat(val).is_ok_and(|m| m.size > 0)),
        "-r" | "-w" | "-x" => ctx.fs.is_some_and(|fs| fs.stat(val).is_ok()),
        _ => false,
    }
}

fn test_binary(left: &str, op: &str, right: &str) -> bool {
    match op {
        "=" | "==" => left == right,
        "!=" => left != right,
        "-eq" => int(left) == int(right),
        "-ne" => int(left) != int(right),
        "-lt" => int(left) < int(right),
        "-gt" => int(left) > int(right),
        "-le" => int(left) <= int(right),
        "-ge" => int(left) >= int(right),
        _ => false,
    }
}

fn int(s: &str) -> i64 {
    s.parse().unwrap_or(0)
}

/// `shift` — shift positional parameters left by N (default 1).
fn builtin_shift(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    let n: usize = argv.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    if n > ctx.state.positional.len() {
        ctx.output.stderr(b"shift: shift count out of range\n");
        return 1;
    }
    ctx.state.positional = ctx.state.positional[n..].to_vec();
    0
}

/// `return` — return from a function with optional status.
/// In our model this just sets the exit status; the function body
/// execution loop in `WorkerRuntime` checks it.
fn builtin_return(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    argv.get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(ctx.state.last_status)
}

/// `exit` — exit the shell with optional status.
fn builtin_exit(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    argv.get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(ctx.state.last_status)
}

/// `local` — declare local variables (in function scope).
/// Runtime uses save/restore stack for function-local variables.
/// `local VAR=val` sets the variable in the current scope.
fn builtin_local(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    for arg in &argv[1..] {
        if let Some(eq_pos) = arg.find('=') {
            let name = &arg[..eq_pos];
            let value = &arg[eq_pos + 1..];
            ctx.state.set_var(SmolStr::from(name), SmolStr::from(value));
        } else {
            // Declare without value
            ctx.state.set_var(SmolStr::from(*arg), SmolStr::default());
        }
    }
    0
}

/// `type` — display information about command type.
fn builtin_type(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    let registry = BuiltinRegistry::new();
    let mut status = 0;
    for name in &argv[1..] {
        if registry.is_builtin(name) {
            let msg = format!("{name} is a shell builtin\n");
            ctx.output.stdout(msg.as_bytes());
        } else {
            let msg = format!("{name}: not found\n");
            ctx.output.stderr(msg.as_bytes());
            status = 1;
        }
    }
    status
}

/// `command` — execute command, bypassing functions. `-v` shows type.
fn builtin_command(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    let args = &argv[1..];
    if args.first() == Some(&"-v") {
        let registry = BuiltinRegistry::new();
        for name in &args[1..] {
            if registry.is_builtin(name) {
                let msg = format!("{name}\n");
                ctx.output.stdout(msg.as_bytes());
            } else {
                return 1;
            }
        }
        return 0;
    }
    // Without -v, command just runs the command (function bypass handled at runtime level)
    0
}

/// `eval` — evaluate arguments as shell code.
/// Intercepted at runtime level, not a placeholder. The runtime re-parses
/// and executes the concatenated arguments directly.
fn builtin_eval(_ctx: &mut BuiltinContext<'_>, _argv: &[&str]) -> i32 {
    // Actual eval is handled in WorkerRuntime by re-parsing the concatenated args.
    // The runtime intercepts "eval" before reaching this builtin.
    0
}

/// `set` — set shell options or positional parameters.
/// Map a long option name (used with `-o`/`+o`) to its short-flag equivalent.
/// Returns the `SHOPT_*` variable name for the option.
fn set_long_option_var(name: &str) -> Option<&'static str> {
    match name {
        "errexit" => Some("SHOPT_e"),
        "nounset" => Some("SHOPT_u"),
        "xtrace" => Some("SHOPT_x"),
        "noglob" => Some("SHOPT_f"),
        "allexport" => Some("SHOPT_a"),
        "noclobber" => Some("SHOPT_C"),
        "pipefail" => Some("SHOPT_o_pipefail"),
        _ => None,
    }
}

fn builtin_set(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    let args = &argv[1..];
    if args.is_empty() {
        return 0;
    }
    if args[0] == "--" {
        ctx.state.positional = args[1..].iter().map(|s| SmolStr::from(*s)).collect();
        return 0;
    }
    let mut i = 0;
    while i < args.len() {
        let arg = args[i];
        if (arg.starts_with('-') || arg.starts_with('+')) && arg.len() > 1 {
            let enable = arg.starts_with('-');
            set_parse_option(ctx, args, &mut i, enable);
        }
        i += 1;
    }
    0
}

/// Parse a single `set` option flag and apply it.
fn set_parse_option(ctx: &mut BuiltinContext<'_>, args: &[&str], i: &mut usize, enable: bool) {
    let flags = &args[*i][1..];
    let val = if enable { "1" } else { "0" };
    if flags == "o" {
        *i += 1;
        if *i < args.len() {
            if let Some(var) = set_long_option_var(args[*i]) {
                ctx.state.set_var(SmolStr::from(var), SmolStr::from(val));
            } else {
                let msg = format!("set: unrecognized option: {}\n", args[*i]);
                ctx.output.stderr(msg.as_bytes());
            }
        }
    } else {
        for c in flags.chars() {
            let opt_name = format!("SHOPT_{c}");
            ctx.state
                .set_var(SmolStr::from(opt_name.as_str()), SmolStr::from(val));
        }
    }
}

/// `getopts` — parse positional parameters for options.
fn builtin_getopts(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    if argv.len() < 3 {
        ctx.output
            .stderr(b"getopts: usage: getopts optstring name\n");
        return 2;
    }
    let optstring = argv[1];
    let var_name = argv[2];
    let optind: usize = ctx
        .state
        .get_var("OPTIND")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);

    if optind > ctx.state.positional.len() {
        return 1; // no more options
    }

    let arg = &ctx.state.positional[optind - 1];
    if !arg.starts_with('-') || arg == "-" {
        return 1; // not an option
    }

    let opt_char = arg.chars().nth(1).unwrap_or('?');
    if optstring.contains(opt_char) {
        ctx.state
            .set_var(SmolStr::from(var_name), SmolStr::from(&arg[1..2]));
    } else {
        ctx.state
            .set_var(SmolStr::from(var_name), SmolStr::from("?"));
    }
    ctx.state.set_var(
        SmolStr::from("OPTIND"),
        SmolStr::from((optind + 1).to_string().as_str()),
    );
    0
}

/// Parsed options for the `read` builtin.
struct ReadOpts<'a> {
    prompt: Option<&'a str>,
    delimiter: char,
    nchars: Option<usize>,
    exact_nchars: Option<usize>,
    array_name: Option<&'a str>,
    remaining_args: &'a [&'a str],
}

/// Parse `read` builtin flags, returning parsed options.
fn parse_read_opts<'a>(argv: &'a [&'a str]) -> ReadOpts<'a> {
    let mut args = &argv[1..];
    let mut opts = ReadOpts {
        prompt: None,
        delimiter: '\n',
        nchars: None,
        exact_nchars: None,
        array_name: None,
        remaining_args: &[],
    };
    while let Some(arg) = args.first() {
        match *arg {
            "-r" | "-s" => args = &args[1..],
            "-p" => {
                opts.prompt = take_read_opt_value(&mut args);
            }
            "-d" => {
                if let Some(value) = take_read_opt_value(&mut args) {
                    opts.delimiter = value.chars().next().unwrap_or('\n');
                }
            }
            "-n" => {
                opts.nchars = take_read_opt_value(&mut args).and_then(|value| value.parse().ok());
            }
            "-N" => {
                opts.exact_nchars =
                    take_read_opt_value(&mut args).and_then(|value| value.parse().ok());
            }
            "-a" => {
                opts.array_name = take_read_opt_value(&mut args);
            }
            "-t" => drop(take_read_opt_value(&mut args)),
            _ => break,
        }
    }
    opts.remaining_args = args;
    opts
}

fn take_read_opt_value<'a>(args: &mut &'a [&'a str]) -> Option<&'a str> {
    if args.len() > 1 {
        let value = args[1];
        *args = &args[2..];
        Some(value)
    } else {
        *args = &args[1..];
        None
    }
}

/// `read` — read a line from stdin into variable(s).
/// Supports: `-r` (no backslash interpretation), `-p prompt`, `-d delim`,
/// `-n nchars`, `-N nchars`, `-a array`, `-t timeout`, `-s` (silent).
fn builtin_read(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    let opts = parse_read_opts(argv);
    emit_read_prompt(ctx, opts.prompt);
    let var_names = read_var_names(&opts);
    let Some(input_text) = read_input_text(ctx) else {
        return 1;
    };

    let (line, remaining) = read_split_input(&input_text, &opts);
    store_read_remaining(ctx, &remaining);
    if let Some(arr_name) = opts.array_name {
        read_into_array(ctx.state, &line, arr_name);
        return 0;
    }
    read_assign_vars(ctx.state, &line, &var_names);
    0
}

fn emit_read_prompt(ctx: &mut BuiltinContext<'_>, prompt: Option<&str>) {
    if let Some(prompt) = prompt {
        ctx.output.stderr(prompt.as_bytes());
    }
}

fn read_var_names<'a>(opts: &'a ReadOpts<'a>) -> Vec<&'a str> {
    if opts.array_name.is_some() || opts.remaining_args.is_empty() {
        vec!["REPLY"]
    } else {
        opts.remaining_args.to_vec()
    }
}

fn store_read_remaining(ctx: &mut BuiltinContext<'_>, remaining: &str) {
    ctx.state
        .set_var(SmolStr::from("_STDIN_REMAINING"), SmolStr::from(remaining));
}

/// Obtain input text for `read` from stdin or the `_STDIN_REMAINING` variable.
fn read_input_text(ctx: &mut BuiltinContext<'_>) -> Option<String> {
    if let Some(data) = ctx.stdin {
        return Some(String::from_utf8_lossy(data).to_string());
    }
    let rem = ctx.state.get_var("_STDIN_REMAINING")?;
    if rem.is_empty() {
        return None;
    }
    Some(rem.to_string())
}

/// Split input into (`current_line`, remaining) according to `read` options (-N, -n, delimiter).
fn read_split_input(input_text: &str, opts: &ReadOpts<'_>) -> (String, String) {
    if let Some(n) = opts.exact_nchars {
        return read_split_exact(input_text, n);
    }
    if let Some(n) = opts.nchars {
        return read_split_nchars(input_text, n, opts.delimiter);
    }
    // Normal line-based read using delimiter
    let mut parts = input_text.splitn(2, opts.delimiter);
    let first = parts.next().unwrap_or("").to_string();
    let rest = parts.next().unwrap_or("").to_string();
    (first, rest)
}

/// Split for `-N` (exact N characters, no delimiter stop).
fn read_split_exact(input_text: &str, n: usize) -> (String, String) {
    let chars: String = input_text.chars().take(n).collect();
    let rest_start = chars.len();
    let rest = if rest_start < input_text.len() {
        &input_text[rest_start..]
    } else {
        ""
    };
    (chars, rest.to_string())
}

/// Split for `-n` (at most N characters, stop at delimiter too).
fn read_split_nchars(input_text: &str, n: usize, delimiter: char) -> (String, String) {
    let mut chars = String::new();
    let mut rest_start = 0;
    for ch in input_text.chars() {
        if chars.len() >= n || ch == delimiter {
            break;
        }
        chars.push(ch);
        rest_start += ch.len_utf8();
    }
    // Skip the delimiter if present
    if rest_start < input_text.len()
        && input_text.as_bytes().get(rest_start) == Some(&(delimiter as u8))
    {
        rest_start += 1;
    }
    let rest = if rest_start < input_text.len() {
        &input_text[rest_start..]
    } else {
        ""
    };
    (chars, rest.to_string())
}

/// Split a line by IFS and store fields into an indexed array.
fn read_into_array(state: &mut ShellState, line: &str, arr_name: &str) {
    let fields = ifs_split_fields(state, line);
    state.init_indexed_array(SmolStr::from(arr_name));
    for (i, field) in fields.iter().enumerate() {
        state.set_array_element(
            SmolStr::from(arr_name),
            &i.to_string(),
            SmolStr::from(*field),
        );
    }
}

/// Split a line by IFS and assign fields to the given variable names.
fn read_assign_vars(state: &mut ShellState, line: &str, var_names: &[&str]) {
    let fields = ifs_split_fields(state, line);
    for (i, var_name) in var_names.iter().enumerate() {
        let val = if i == var_names.len() - 1 {
            // Last variable gets the rest of the line
            if i < fields.len() {
                fields[i..].join(" ")
            } else {
                String::new()
            }
        } else if let Some(field) = fields.get(i) {
            (*field).to_string()
        } else {
            String::new()
        };
        state.set_var(SmolStr::from(*var_name), SmolStr::from(val.as_str()));
    }
}

/// Split a line by IFS characters, filtering empty fields.
fn ifs_split_fields<'a>(state: &ShellState, line: &'a str) -> Vec<&'a str> {
    let ifs = state
        .get_var("IFS")
        .unwrap_or_else(|| SmolStr::from(" \t\n"));
    if ifs.is_empty() {
        vec![line]
    } else {
        line.split(|c: char| ifs.contains(c))
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// `trap` — set handlers for signals/events.
/// In wasmsh, only EXIT and ERR traps are supported via shell variables.
fn builtin_trap(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    let args = &argv[1..];
    if args.len() < 2 {
        return 0;
    }
    let handler = args[0];
    for signal in &args[1..] {
        match *signal {
            "EXIT" | "0" => {
                ctx.state
                    .set_var(SmolStr::from("_TRAP_EXIT"), SmolStr::from(handler));
            }
            "ERR" => {
                ctx.state
                    .set_var(SmolStr::from("_TRAP_ERR"), SmolStr::from(handler));
            }
            _ => {
                let msg = format!("trap: {signal}: signal not supported\n");
                ctx.output.stderr(msg.as_bytes());
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_builtin(name: &str, argv: &[&str]) -> (i32, VecSink) {
        let registry = BuiltinRegistry::new();
        let mut state = ShellState::new();
        let mut sink = VecSink::default();
        let builtin = registry.get(name).unwrap();
        let status = {
            let mut ctx = BuiltinContext {
                state: &mut state,
                output: &mut sink,
                fs: None,
                stdin: None,
            };
            builtin(&mut ctx, argv)
        };
        (status, sink)
    }

    fn run_builtin_with_state(name: &str, argv: &[&str], state: &mut ShellState) -> (i32, VecSink) {
        let registry = BuiltinRegistry::new();
        let mut sink = VecSink::default();
        let builtin = registry.get(name).unwrap();
        let status = {
            let mut ctx = BuiltinContext {
                state,
                output: &mut sink,
                fs: None,
                stdin: None,
            };
            builtin(&mut ctx, argv)
        };
        (status, sink)
    }

    #[test]
    fn colon_returns_zero() {
        let (status, _) = run_builtin(":", &[":"]);
        assert_eq!(status, 0);
    }

    #[test]
    fn true_returns_zero() {
        let (status, _) = run_builtin("true", &["true"]);
        assert_eq!(status, 0);
    }

    #[test]
    fn false_returns_one() {
        let (status, _) = run_builtin("false", &["false"]);
        assert_eq!(status, 1);
    }

    #[test]
    fn echo_basic() {
        let (status, sink) = run_builtin("echo", &["echo", "hello", "world"]);
        assert_eq!(status, 0);
        assert_eq!(sink.stdout_str(), "hello world\n");
    }

    #[test]
    fn echo_no_args() {
        let (_, sink) = run_builtin("echo", &["echo"]);
        assert_eq!(sink.stdout_str(), "\n");
    }

    #[test]
    fn echo_suppress_newline() {
        let (_, sink) = run_builtin("echo", &["echo", "-n", "hello"]);
        assert_eq!(sink.stdout_str(), "hello");
    }

    #[test]
    fn printf_basic() {
        let (status, sink) = run_builtin("printf", &["printf", "hello %s\\n", "world"]);
        assert_eq!(status, 0);
        assert_eq!(sink.stdout_str(), "hello world\n");
    }

    #[test]
    fn printf_int() {
        let (_, sink) = run_builtin("printf", &["printf", "%d", "42"]);
        assert_eq!(sink.stdout_str(), "42");
    }

    #[test]
    fn printf_no_args() {
        let (status, sink) = run_builtin("printf", &["printf"]);
        assert_eq!(status, 1);
        assert!(!sink.stderr_str().is_empty());
    }

    #[test]
    fn pwd_shows_cwd() {
        let mut state = ShellState::new();
        state.cwd = "/home/user".into();
        let (status, sink) = run_builtin_with_state("pwd", &["pwd"], &mut state);
        assert_eq!(status, 0);
        assert_eq!(sink.stdout_str(), "/home/user\n");
    }

    #[test]
    fn cd_changes_cwd() {
        let mut state = ShellState::new();
        let (status, _) = run_builtin_with_state("cd", &["cd", "/tmp"], &mut state);
        assert_eq!(status, 0);
        assert_eq!(state.cwd, "/tmp");
        assert_eq!(state.get_var("PWD").unwrap(), "/tmp");
        assert_eq!(state.get_var("OLDPWD").unwrap(), "/");
    }

    #[test]
    fn cd_dash_returns_to_oldpwd() {
        let mut state = ShellState::new();
        run_builtin_with_state("cd", &["cd", "/tmp"], &mut state);
        let (status, sink) = run_builtin_with_state("cd", &["cd", "-"], &mut state);
        assert_eq!(status, 0);
        assert_eq!(state.cwd, "/");
        assert_eq!(sink.stdout_str(), "/\n");
    }

    #[test]
    fn cd_no_args_goes_home() {
        let mut state = ShellState::new();
        state.set_var("HOME".into(), "/home/user".into());
        let (status, _) = run_builtin_with_state("cd", &["cd"], &mut state);
        assert_eq!(status, 0);
        assert_eq!(state.cwd, "/home/user");
    }

    #[test]
    fn cd_no_home_error() {
        let mut state = ShellState::new();
        let (status, sink) = run_builtin_with_state("cd", &["cd"], &mut state);
        assert_eq!(status, 1);
        assert!(sink.stderr_str().contains("HOME not set"));
    }

    #[test]
    fn export_name_value() {
        let mut state = ShellState::new();
        run_builtin_with_state("export", &["export", "FOO=bar"], &mut state);
        let var = state.env.get("FOO").unwrap();
        assert_eq!(var.value.as_scalar(), "bar");
        assert!(var.exported);
    }

    #[test]
    fn export_existing() {
        let mut state = ShellState::new();
        state.set_var("X".into(), "val".into());
        assert!(!state.env.get("X").unwrap().exported);
        run_builtin_with_state("export", &["export", "X"], &mut state);
        assert!(state.env.get("X").unwrap().exported);
    }

    #[test]
    fn unset_variable() {
        let mut state = ShellState::new();
        state.set_var("FOO".into(), "bar".into());
        run_builtin_with_state("unset", &["unset", "FOO"], &mut state);
        // After unset, variable is truly gone
        assert!(state.get_var("FOO").is_none());
    }

    #[test]
    fn unset_readonly_fails() {
        let mut state = ShellState::new();
        state.set_readonly("X".into(), "locked".into());
        let (status, sink) = run_builtin_with_state("unset", &["unset", "X"], &mut state);
        assert_eq!(status, 1);
        assert!(sink.stderr_str().contains("readonly"));
        assert!(state.get_var("X").is_some()); // still set
    }

    #[test]
    fn readonly_set_value() {
        let mut state = ShellState::new();
        run_builtin_with_state("readonly", &["readonly", "X=locked"], &mut state);
        assert_eq!(state.get_var("X").unwrap(), "locked");
        let var = state.env.get("X").unwrap();
        assert!(var.readonly);
    }

    #[test]
    fn readonly_mark_existing() {
        let mut state = ShellState::new();
        state.set_var("X".into(), "val".into());
        run_builtin_with_state("readonly", &["readonly", "X"], &mut state);
        assert!(state.env.get("X").unwrap().readonly);
    }

    #[test]
    fn registry_lookup() {
        let registry = BuiltinRegistry::new();
        assert!(registry.is_builtin("echo"));
        assert!(registry.is_builtin(":"));
        assert!(registry.is_builtin("readonly"));
        assert!(!registry.is_builtin("ls"));
    }

    // ---- set builtin: -o option tests ----

    #[test]
    fn set_short_flag() {
        let mut state = ShellState::new();
        run_builtin_with_state("set", &["set", "-e"], &mut state);
        assert_eq!(state.get_var("SHOPT_e").unwrap(), "1");
    }

    #[test]
    fn set_plus_disables_flag() {
        let mut state = ShellState::new();
        run_builtin_with_state("set", &["set", "-e"], &mut state);
        run_builtin_with_state("set", &["set", "+e"], &mut state);
        assert_eq!(state.get_var("SHOPT_e").unwrap(), "0");
    }

    #[test]
    fn set_o_pipefail() {
        let mut state = ShellState::new();
        run_builtin_with_state("set", &["set", "-o", "pipefail"], &mut state);
        assert_eq!(state.get_var("SHOPT_o_pipefail").unwrap(), "1");
    }

    #[test]
    fn set_plus_o_pipefail() {
        let mut state = ShellState::new();
        run_builtin_with_state("set", &["set", "-o", "pipefail"], &mut state);
        run_builtin_with_state("set", &["set", "+o", "pipefail"], &mut state);
        assert_eq!(state.get_var("SHOPT_o_pipefail").unwrap(), "0");
    }

    #[test]
    fn set_o_errexit_aliases_e() {
        let mut state = ShellState::new();
        run_builtin_with_state("set", &["set", "-o", "errexit"], &mut state);
        assert_eq!(state.get_var("SHOPT_e").unwrap(), "1");
    }

    #[test]
    fn set_o_nounset_aliases_u() {
        let mut state = ShellState::new();
        run_builtin_with_state("set", &["set", "-o", "nounset"], &mut state);
        assert_eq!(state.get_var("SHOPT_u").unwrap(), "1");
    }

    #[test]
    fn set_o_xtrace_aliases_x() {
        let mut state = ShellState::new();
        run_builtin_with_state("set", &["set", "-o", "xtrace"], &mut state);
        assert_eq!(state.get_var("SHOPT_x").unwrap(), "1");
    }

    #[test]
    fn set_o_noglob_aliases_f() {
        let mut state = ShellState::new();
        run_builtin_with_state("set", &["set", "-o", "noglob"], &mut state);
        assert_eq!(state.get_var("SHOPT_f").unwrap(), "1");
    }

    #[test]
    fn set_o_allexport_aliases_a() {
        let mut state = ShellState::new();
        run_builtin_with_state("set", &["set", "-o", "allexport"], &mut state);
        assert_eq!(state.get_var("SHOPT_a").unwrap(), "1");
    }

    #[test]
    fn set_o_noclobber_aliases_capital_c() {
        let mut state = ShellState::new();
        run_builtin_with_state("set", &["set", "-o", "noclobber"], &mut state);
        assert_eq!(state.get_var("SHOPT_C").unwrap(), "1");
    }

    #[test]
    fn set_o_unrecognized_option_reports_error() {
        let mut state = ShellState::new();
        let (status, sink) =
            run_builtin_with_state("set", &["set", "-o", "nonexistent"], &mut state);
        assert_eq!(status, 0); // set doesn't fail, just warns on stderr
        assert!(sink.stderr_str().contains("unrecognized option"));
    }
}
