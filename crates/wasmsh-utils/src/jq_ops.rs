//! jq utility: JSON processor.
//!
//! Backed by the `jaq-core` / `jaq-std` / `jaq-json` crates — a pure
//! Rust reimplementation of jq with an NLnet security audit.  See
//! ADR-0026, which supersedes the previous hand-rolled implementation
//! (~8000 lines that only supported a subset of the jq filter
//! language).  The CLI surface that wasmsh exposes to shell scripts
//! is unchanged: same flags, same stdin/file handling, same exit
//! codes.

use jaq_core::{Filter, Native};
use jaq_json::Val;

use crate::helpers::{collect_input_text, collect_path_text, resolve_path};
use crate::jaq_runner;
use crate::UtilContext;

// ---------------------------------------------------------------------------
// Option parsing
// ---------------------------------------------------------------------------

#[allow(clippy::struct_excessive_bools)]
struct JqOpts {
    /// `-r` / `-j` / `--raw-output` / `--join-output`: emit strings
    /// without JSON quoting.
    raw_output: bool,
    /// `-j` / `--join-output`: like `-r` but without a trailing newline
    /// between values.
    join_output: bool,
    /// `-e` / `--exit-status`: exit 1 if the last value was `null` or
    /// `false` (or if there were no outputs).
    exit_status: bool,
    /// `-c` / `--compact-output`: emit single-line JSON.
    compact: bool,
    /// `-n` / `--null-input`: do not read from stdin/file; use `null`
    /// as the sole input.
    null_input: bool,
    /// `-s` / `--slurp`: consume all input JSON values into a single
    /// array and run the filter once with that array as input.
    slurp: bool,
    /// Additional variable bindings from `--arg NAME VALUE` and
    /// `--argjson NAME JSON_VALUE`.  They become jq `$name` variables.
    vars: Vec<(String, Val)>,
}

impl JqOpts {
    fn new() -> Self {
        Self {
            raw_output: false,
            join_output: false,
            exit_status: false,
            compact: false,
            null_input: false,
            slurp: false,
            vars: Vec::new(),
        }
    }
}

pub(crate) fn util_jq(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut opts = JqOpts::new();

    while let Some(&arg) = args.first() {
        match parse_jq_option(ctx, &mut args, &mut opts, arg) {
            Ok(true) => {}
            Ok(false) => break,
            Err(code) => return code,
        }
    }

    let Some((filter_src, file_args)) = extract_filter_arg(&mut args) else {
        ctx.output.stderr(b"jq: no filter provided\n");
        return 1;
    };

    // jaq expects global var names with the `$` prefix attached.
    let var_names: Vec<String> = opts
        .vars
        .iter()
        .map(|(name, _)| format!("${name}"))
        .collect();
    let var_names: Vec<&str> = var_names.iter().map(String::as_str).collect();
    let filter = match compile_filter(filter_src, &var_names) {
        Ok(f) => f,
        Err(e) => {
            let msg = format!("jq: error parsing filter: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            return 2;
        }
    };

    run_jq_pipeline(ctx, &opts, &filter, file_args)
}

fn extract_filter_arg<'a>(args: &mut &'a [&'a str]) -> Option<(&'a str, &'a [&'a str])> {
    let &f = args.first()?;
    *args = &args[1..];
    Some((f, args))
}

fn parse_jq_option(
    ctx: &mut UtilContext<'_>,
    args: &mut &[&str],
    opts: &mut JqOpts,
    arg: &str,
) -> Result<bool, i32> {
    match arg {
        "-r" | "--raw-output" => {
            opts.raw_output = true;
            *args = &args[1..];
            Ok(true)
        }
        "-j" | "--join-output" => {
            opts.raw_output = true;
            opts.join_output = true;
            *args = &args[1..];
            Ok(true)
        }
        "-e" | "--exit-status" => {
            opts.exit_status = true;
            *args = &args[1..];
            Ok(true)
        }
        "-c" | "--compact-output" => {
            opts.compact = true;
            *args = &args[1..];
            Ok(true)
        }
        "-n" | "--null-input" => {
            opts.null_input = true;
            *args = &args[1..];
            Ok(true)
        }
        "-s" | "--slurp" => {
            opts.slurp = true;
            *args = &args[1..];
            Ok(true)
        }
        "--arg" => parse_jq_named_arg(ctx, args, &mut opts.vars),
        "--argjson" => parse_jq_named_json_arg(ctx, args, &mut opts.vars),
        "--" => {
            *args = &args[1..];
            Ok(false)
        }
        _ if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") => {
            Ok(parse_jq_short_flags(args, opts, arg))
        }
        _ => Ok(false),
    }
}

fn parse_jq_short_flags(args: &mut &[&str], opts: &mut JqOpts, arg: &str) -> bool {
    for c in arg[1..].chars() {
        match c {
            'e' => opts.exit_status = true,
            'c' => opts.compact = true,
            'n' => opts.null_input = true,
            's' => opts.slurp = true,
            'r' => opts.raw_output = true,
            'j' => {
                opts.raw_output = true;
                opts.join_output = true;
            }
            _ => return false,
        }
    }
    *args = &args[1..];
    true
}

fn parse_jq_named_arg(
    ctx: &mut UtilContext<'_>,
    args: &mut &[&str],
    vars: &mut Vec<(String, Val)>,
) -> Result<bool, i32> {
    if args.len() < 3 {
        ctx.output.stderr(b"jq: --arg requires NAME VALUE\n");
        return Err(1);
    }
    vars.push((args[1].to_string(), Val::from(args[2].to_string())));
    *args = &args[3..];
    Ok(true)
}

fn parse_jq_named_json_arg(
    ctx: &mut UtilContext<'_>,
    args: &mut &[&str],
    vars: &mut Vec<(String, Val)>,
) -> Result<bool, i32> {
    if args.len() < 3 {
        ctx.output.stderr(b"jq: --argjson requires NAME VALUE\n");
        return Err(1);
    }
    let val = match parse_json_single(args[2]) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("jq: invalid JSON for --argjson: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            return Err(1);
        }
    };
    vars.push((args[1].to_string(), val));
    *args = &args[3..];
    Ok(true)
}

// ---------------------------------------------------------------------------
// jaq compilation / execution
// ---------------------------------------------------------------------------

/// Compile a jq filter source string with the combined `jaq-std` +
/// `jaq-json` standard library available.
fn compile_filter(source: &str, var_names: &[&str]) -> Result<Filter<Native<Val>>, String> {
    jaq_runner::compile_filter(source, var_names)
}

// ---------------------------------------------------------------------------
// Pipeline: read input, run filter, format output
// ---------------------------------------------------------------------------

fn run_jq_pipeline(
    ctx: &mut UtilContext<'_>,
    opts: &JqOpts,
    filter: &Filter<Native<Val>>,
    file_args: &[&str],
) -> i32 {
    // 1. Collect raw JSON input text from stdin or the listed files.
    let input_texts = match collect_jq_input_texts(ctx, file_args, opts.null_input) {
        Ok(texts) => texts,
        Err(code) => return code,
    };

    // 2. Parse into jaq Val values.
    let parsed = match parse_jq_input_values(ctx, &input_texts, opts.null_input) {
        Ok(values) => values,
        Err(code) => return code,
    };

    // 3. Apply the --slurp transformation if requested.
    let inputs: Vec<Val> = if opts.slurp {
        vec![Val::Arr(parsed.into())]
    } else {
        parsed
    };

    // 4. Execute the filter against each input value, formatting output.
    let (status, had_output, last_value) = execute_filter(ctx, &inputs, filter, opts);
    finalize_jq_status(opts.exit_status, status, had_output, last_value.as_ref())
}

fn collect_jq_input_texts(
    ctx: &mut UtilContext<'_>,
    file_args: &[&str],
    null_input: bool,
) -> Result<Vec<String>, i32> {
    if null_input {
        return Ok(vec![]);
    }
    if file_args.is_empty() {
        let data = collect_input_text(ctx, &[], "jq")?;
        if data.is_empty() {
            ctx.output.stderr(b"jq: no input\n");
            return Err(1);
        }
        return Ok(vec![data]);
    }
    let mut texts = Vec::new();
    for path in file_args {
        let full = resolve_path(ctx.cwd, path);
        match collect_path_text(ctx, &full, path, "jq") {
            Ok(text) => texts.push(text),
            Err(status) => return Err(status),
        }
    }
    Ok(texts)
}

fn parse_jq_input_values(
    ctx: &mut UtilContext<'_>,
    input_texts: &[String],
    null_input: bool,
) -> Result<Vec<Val>, i32> {
    if null_input {
        return Ok(vec![Val::Null]);
    }
    let mut out = Vec::new();
    for text in input_texts {
        match parse_json_all(text) {
            Ok(vals) => out.extend(vals),
            Err(e) => {
                let msg = format!("jq: error parsing JSON: {e}\n");
                ctx.output.stderr(msg.as_bytes());
                return Err(2);
            }
        }
    }
    Ok(out)
}

// Parsing helpers are shared with yaml_ops via `jaq_runner`.
use jaq_runner::{parse_json_all, parse_json_single};

fn execute_filter(
    ctx: &mut UtilContext<'_>,
    inputs: &[Val],
    filter: &Filter<Native<Val>>,
    opts: &JqOpts,
) -> (i32, bool, Option<Val>) {
    let mut status = 0;
    let mut had_output = false;
    let mut last_value: Option<Val> = None;

    let vars: Vec<Val> = opts.vars.iter().map(|(_, v)| v.clone()).collect();

    for input in inputs {
        let (values, err) = jaq_runner::run_filter(filter, input.clone(), &vars);
        for val in values {
            emit_value(ctx, &val, opts);
            had_output = true;
            last_value = Some(val);
        }
        if let Some(e) = err {
            let msg = format!("jq: error: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            status = 5;
        }
    }

    (status, had_output, last_value)
}

fn emit_value(ctx: &mut UtilContext<'_>, val: &Val, opts: &JqOpts) {
    let mut buf = String::new();
    if opts.raw_output {
        if let Val::Str(s) = val {
            buf.push_str(s);
        } else {
            jaq_runner::format_json(&mut buf, val, opts.compact);
        }
    } else {
        jaq_runner::format_json(&mut buf, val, opts.compact);
    }
    if !opts.join_output {
        buf.push('\n');
    }
    ctx.output.stdout(buf.as_bytes());
}

fn finalize_jq_status(
    exit_status_mode: bool,
    run_status: i32,
    had_output: bool,
    last_value: Option<&Val>,
) -> i32 {
    if run_status != 0 {
        return run_status;
    }
    if !exit_status_mode {
        return 0;
    }
    // `-e`: exit 1 if the last value was null or false, or if no
    // output was produced.
    if !had_output {
        return 1;
    }
    match last_value {
        Some(Val::Null) | Some(Val::Bool(false)) => 1,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// These exercise the wasmsh wrapper around jaq-core, not jaq itself
// (jaq has its own audited test suite).  We verify: option parsing,
// stdin/file reading, pretty vs compact output, `-r` raw strings,
// `-e` exit semantics, `--slurp` / `-n`, and `--arg` / `--argjson`.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UtilContext, VecOutput};
    use wasmsh_fs::{MemoryFs, OpenOptions, Vfs};

    fn run(argv: &[&str], stdin: Option<&[u8]>) -> (i32, String, String) {
        let mut fs = MemoryFs::new();
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut output,
                cwd: "/",
                stdin: stdin.map(crate::UtilStdin::from_bytes),
                state: None,
                network: None,
            };
            util_jq(&mut ctx, argv)
        };
        (
            status,
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn run_with_file(argv: &[&str], path: &str, content: &[u8]) -> (i32, String, String) {
        let mut fs = MemoryFs::new();
        let h = fs.open(path, OpenOptions::write()).unwrap();
        fs.write_file(h, content).unwrap();
        fs.close(h);
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut output,
                cwd: "/",
                stdin: None,
                state: None,
                network: None,
            };
            util_jq(&mut ctx, argv)
        };
        (
            status,
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    #[test]
    fn identity_passes_through() {
        let (status, out, _) = run(&["jq", "."], Some(b"42"));
        assert_eq!(status, 0);
        assert_eq!(out, "42\n");
    }

    #[test]
    fn field_access() {
        let (status, out, _) = run(&["jq", ".foo"], Some(br#"{"foo": 123}"#));
        assert_eq!(status, 0);
        assert_eq!(out, "123\n");
    }

    #[test]
    fn nested_field() {
        let (status, out, _) = run(&["jq", ".a.b"], Some(br#"{"a": {"b": "x"}}"#));
        assert_eq!(status, 0);
        assert_eq!(out, "\"x\"\n");
    }

    #[test]
    fn array_index() {
        let (status, out, _) = run(&["jq", ".[1]"], Some(b"[10, 20, 30]"));
        assert_eq!(status, 0);
        assert_eq!(out, "20\n");
    }

    #[test]
    fn map_filter() {
        let (status, out, _) = run(&["jq", "-c", "map(.+1)"], Some(b"[1,2,3]"));
        assert_eq!(status, 0);
        assert_eq!(out, "[2,3,4]\n");
    }

    #[test]
    fn pretty_output_is_indented() {
        // Default (non-compact) output for an object must be
        // multi-line and 2-space indented, matching real jq.
        let (status, out, _) = run(&["jq", "."], Some(br#"{"a": 1, "b": [2, 3]}"#));
        assert_eq!(status, 0);
        assert!(
            out.starts_with("{\n  \"a\": 1,\n"),
            "expected pretty output, got: {out}"
        );
        assert!(out.contains("  \"b\": [\n    2,\n    3\n  ]"));
    }

    #[test]
    fn compact_single_line() {
        let (status, out, _) = run(&["jq", "-c", "."], Some(br#"{"a": 1}"#));
        assert_eq!(status, 0);
        assert_eq!(out, "{\"a\":1}\n");
    }

    #[test]
    fn raw_output_strips_quotes() {
        let (status, out, _) = run(&["jq", "-r", ".name"], Some(br#"{"name": "alice"}"#));
        assert_eq!(status, 0);
        assert_eq!(out, "alice\n");
    }

    #[test]
    fn raw_output_leaves_non_strings_alone() {
        let (status, out, _) = run(&["jq", "-r", ".count"], Some(br#"{"count": 42}"#));
        assert_eq!(status, 0);
        assert_eq!(out, "42\n");
    }

    #[test]
    fn slurp_reads_array_stream() {
        let (status, out, _) = run(&["jq", "-sc", "length"], Some(b"1 2 3 4"));
        assert_eq!(status, 0);
        assert_eq!(out, "4\n");
    }

    #[test]
    fn null_input_ignores_stdin() {
        let (status, out, _) = run(
            &["jq", "-nc", "{x:1,y:2}"],
            Some(b"should not be parsed"),
        );
        assert_eq!(status, 0);
        assert_eq!(out, "{\"x\":1,\"y\":2}\n");
    }

    #[test]
    fn arg_variable_string() {
        let (status, out, err) = run(
            &["jq", "-r", "--arg", "name", "bob", "$name"],
            Some(b"null"),
        );
        assert_eq!(status, 0, "stderr: {err}");
        assert_eq!(out, "bob\n");
    }

    #[test]
    fn argjson_variable_number() {
        let (status, out, _) = run(
            &["jq", "-c", "--argjson", "n", "42", "{val:$n}"],
            Some(b"null"),
        );
        assert_eq!(status, 0);
        assert_eq!(out, "{\"val\":42}\n");
    }

    #[test]
    fn exit_status_false_returns_one() {
        let (status, _, _) = run(&["jq", "-e", "."], Some(b"false"));
        assert_eq!(status, 1);
    }

    #[test]
    fn exit_status_null_returns_one() {
        let (status, _, _) = run(&["jq", "-e", "."], Some(b"null"));
        assert_eq!(status, 1);
    }

    #[test]
    fn exit_status_truthy_returns_zero() {
        let (status, _, _) = run(&["jq", "-e", "."], Some(b"42"));
        assert_eq!(status, 0);
    }

    #[test]
    fn read_from_file() {
        let (status, out, _) =
            run_with_file(&["jq", "-c", ".", "/data.json"], "/data.json", b"[1,2]");
        assert_eq!(status, 0);
        assert_eq!(out, "[1,2]\n");
    }

    #[test]
    fn missing_file_errors() {
        let mut fs = MemoryFs::new();
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut output,
                cwd: "/",
                stdin: None,
                state: None,
                network: None,
            };
            util_jq(&mut ctx, &["jq", ".", "/missing.json"])
        };
        assert_ne!(status, 0);
    }

    #[test]
    fn keys_returns_sorted() {
        let (status, out, _) = run(&["jq", "-c", "keys"], Some(br#"{"b":2,"a":1}"#));
        assert_eq!(status, 0);
        assert_eq!(out, "[\"a\",\"b\"]\n");
    }

    #[test]
    fn length_of_array() {
        let (status, out, _) = run(&["jq", "length"], Some(b"[1,2,3,4,5]"));
        assert_eq!(status, 0);
        assert_eq!(out, "5\n");
    }

    #[test]
    fn select_filter() {
        let (status, out, _) = run(
            &["jq", "-c", ".[] | select(.score > 80)"],
            Some(br#"[{"name":"a","score":70},{"name":"b","score":90}]"#),
        );
        assert_eq!(status, 0);
        assert_eq!(out, "{\"name\":\"b\",\"score\":90}\n");
    }

    #[test]
    fn pipe_composition() {
        let (status, out, _) = run(
            &["jq", "-r", ".users | map(.name) | join(\", \")"],
            Some(br#"{"users":[{"name":"a"},{"name":"b"},{"name":"c"}]}"#),
        );
        assert_eq!(status, 0);
        assert_eq!(out, "a, b, c\n");
    }

    #[test]
    fn bad_filter_returns_error() {
        let (status, _, stderr) = run(&["jq", ".foo.["], Some(b"{}"));
        assert_ne!(status, 0);
        assert!(!stderr.is_empty());
    }

    #[test]
    fn invalid_json_input() {
        let (status, _, stderr) = run(&["jq", "."], Some(b"not json"));
        assert_ne!(status, 0);
        assert!(stderr.contains("jq:"));
    }
}
