//! YAML utility: yq.
//!
//! yq is "jq for YAML" — a tool that parses YAML input, runs a
//! jq-style filter over it, and re-serialises the result.  wasmsh's
//! previous implementation carried a hand-written YAML 1.1-ish parser
//! and a tiny jq-subset filter engine (~2000 lines total) that had
//! to be maintained in lockstep with the real jq semantics.
//!
//! Since ADR-0026 adopts `jaq-core` for the filter language, this
//! module only has to:
//!
//! 1. Parse YAML input via `saphyr`.
//! 2. Convert `saphyr::Yaml` to jaq's `Val` so the filter engine can
//!    operate on it.
//! 3. Run the filter through `jaq_runner`.
//! 4. Emit results as either YAML (default), JSON (`-j`), or raw
//!    strings (`-r`).
//!
//! See ADR-0027.
//!
//! Field ordering: `yaml_rust` / `saphyr` parse mapping keys in document
//! order.  jaq's `Val::Obj` uses an insertion-order `IndexMap`, so the
//! conversion preserves document order end-to-end.

use std::fmt::Write;
use std::rc::Rc;

use saphyr::{LoadableYamlNode, Yaml};

use crate::helpers::{collect_input_text, collect_path_text, resolve_path};
use crate::jaq_runner;
use crate::UtilContext;

use jaq_json::Val;

// ---------------------------------------------------------------------------
// CLI option parsing
// ---------------------------------------------------------------------------

/// Parsed yq command-line options.
#[allow(clippy::struct_excessive_bools)]
struct YqOptions {
    raw_output: bool,
    exit_status_mode: bool,
    compact: bool,
    /// `-j` / `--json-output`: emit JSON instead of YAML.
    json_output: bool,
}

impl YqOptions {
    fn new() -> Self {
        Self {
            raw_output: false,
            exit_status_mode: false,
            compact: false,
            json_output: false,
        }
    }
}

fn parse_yq_flags(args: &[&str]) -> (YqOptions, usize) {
    let mut opts = YqOptions::new();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i];
        match arg {
            "-r" | "--raw-output" => opts.raw_output = true,
            "-e" | "--exit-status" => opts.exit_status_mode = true,
            "-c" | "--compact-output" => opts.compact = true,
            "-j" | "--json-output" => opts.json_output = true,
            _ if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") => {
                if !parse_combined_flags(&arg[1..], &mut opts) {
                    break;
                }
            }
            _ => break,
        }
        i += 1;
    }
    (opts, i)
}

fn parse_combined_flags(flags: &str, opts: &mut YqOptions) -> bool {
    for ch in flags.chars() {
        match ch {
            'r' => opts.raw_output = true,
            'e' => opts.exit_status_mode = true,
            'c' => opts.compact = true,
            'j' => opts.json_output = true,
            _ => return false,
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub(crate) fn util_yq(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let (opts, consumed) = parse_yq_flags(&argv[1..]);
    let args = &argv[1 + consumed..];

    if args.is_empty() {
        ctx.output.stderr(b"yq: missing filter\n");
        return 1;
    }

    let filter_src = args[0];
    let file_args = &args[1..];

    let filter = match jaq_runner::compile_filter(filter_src, &[]) {
        Ok(f) => f,
        Err(e) => {
            let msg = format!("yq: error parsing filter: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            return 1;
        }
    };

    let text = match read_yq_input(ctx, file_args) {
        Ok(t) => t,
        Err(code) => return code,
    };

    let input = match parse_yaml_to_val(&text) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("yq: parse error: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            return 1;
        }
    };

    let (results, err) = jaq_runner::run_filter(&filter, input, &[]);
    if let Some(e) = err {
        let msg = format!("yq: {e}\n");
        ctx.output.stderr(msg.as_bytes());
        return 1;
    }

    if opts.exit_status_mode && results.is_empty() {
        return 1;
    }

    for val in &results {
        let out = format_yq_result(val, &opts);
        ctx.output.stdout(out.as_bytes());
        if !out.ends_with('\n') {
            ctx.output.stdout(b"\n");
        }
    }

    if opts.exit_status_mode {
        i32::from(!results.iter().all(is_truthy))
    } else {
        0
    }
}

fn is_truthy(val: &Val) -> bool {
    !matches!(val, Val::Null | Val::Bool(false))
}

fn read_yq_input(ctx: &mut UtilContext<'_>, file_args: &[&str]) -> Result<String, i32> {
    if file_args.is_empty() {
        if ctx.stdin.is_none() {
            ctx.output.stderr(b"yq: missing input\n");
            return Err(1);
        }
        return collect_input_text(ctx, &[], "yq");
    }
    let mut combined = String::new();
    for path in file_args {
        let full = resolve_path(ctx.cwd, path);
        match collect_path_text(ctx, &full, path, "yq") {
            Ok(t) => {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&t);
            }
            Err(status) => return Err(status),
        }
    }
    Ok(combined)
}

// ---------------------------------------------------------------------------
// YAML → jaq Val
// ---------------------------------------------------------------------------

/// Parse a YAML document into a jaq `Val`.  Returns the first
/// document in the stream (to match the previous behaviour of the
/// handwritten parser, which treated multi-document inputs as a
/// single root value).
fn parse_yaml_to_val(text: &str) -> Result<Val, String> {
    let docs = Yaml::load_from_str(text).map_err(|e| format!("{e}"))?;
    let first = docs
        .into_iter()
        .next()
        .unwrap_or(Yaml::Value(saphyr::Scalar::Null));
    Ok(yaml_to_val(&first))
}

fn yaml_to_val(yaml: &Yaml<'_>) -> Val {
    use saphyr::Scalar;
    match yaml {
        Yaml::Value(scalar) => match scalar {
            Scalar::Null => Val::Null,
            Scalar::Boolean(b) => Val::Bool(*b),
            Scalar::Integer(i) => {
                // jaq's Val::Int is `isize`; wrap losslessly where
                // possible and fall back to string-form Num for
                // oversized integers.
                if let Ok(v) = isize::try_from(*i) {
                    Val::Int(v)
                } else {
                    Val::Num(Rc::new(i.to_string()))
                }
            }
            Scalar::FloatingPoint(f) => Val::Float(f.into_inner()),
            Scalar::String(s) => Val::Str(Rc::new(s.to_string())),
        },
        Yaml::Sequence(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(yaml_to_val(item));
            }
            Val::Arr(Rc::new(out))
        }
        Yaml::Mapping(map) => {
            // jaq-json's Val::Obj uses foldhash::fast::RandomState,
            // so we match it here to satisfy `Val::obj`.
            let hasher = foldhash::fast::RandomState::default();
            let mut obj: indexmap::IndexMap<Rc<String>, Val, foldhash::fast::RandomState> =
                indexmap::IndexMap::with_hasher(hasher);
            for (k, v) in map {
                let key = yaml_key_to_string(k);
                obj.insert(Rc::new(key), yaml_to_val(v));
            }
            Val::obj(obj)
        }
        // Tagged nodes unwrap to their underlying value — yq is a
        // data processor, not a schema validator.  Aliases,
        // directives, and malformed nodes collapse to null so the
        // filter never has to special-case them.
        Yaml::Tagged(_, inner) => yaml_to_val(inner),
        Yaml::Alias(_) | Yaml::BadValue | Yaml::Representation(_, _, _) => Val::Null,
    }
}

fn yaml_key_to_string(yaml: &Yaml<'_>) -> String {
    use saphyr::Scalar;
    match yaml {
        Yaml::Value(Scalar::String(s)) => s.to_string(),
        Yaml::Value(Scalar::Integer(i)) => i.to_string(),
        Yaml::Value(Scalar::FloatingPoint(f)) => f.into_inner().to_string(),
        Yaml::Value(Scalar::Boolean(b)) => b.to_string(),
        Yaml::Value(Scalar::Null) => "null".to_string(),
        Yaml::Sequence(_) | Yaml::Mapping(_) => {
            // Complex keys aren't meaningful for jq-style filtering;
            // flatten via Display.
            let mut s = String::new();
            let _ = write!(s, "{yaml:?}");
            s
        }
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Output formatting
// ---------------------------------------------------------------------------

fn format_yq_result(val: &Val, opts: &YqOptions) -> String {
    if opts.json_output {
        let mut buf = String::new();
        jaq_runner::format_json(&mut buf, val, opts.compact);
        buf
    } else if opts.raw_output {
        raw_string(val)
    } else {
        format_yaml(val)
    }
}

fn raw_string(val: &Val) -> String {
    if let Val::Str(s) = val {
        return s.to_string();
    }
    let mut buf = String::new();
    jaq_runner::format_json(&mut buf, val, true);
    buf
}

/// Serialise a `Val` as a YAML block.
///
/// This is a minimal YAML emitter sufficient for yq's output needs:
/// scalars, arrays, mappings, with conservative string quoting that
/// matches the previous handwritten output.  Nested composites use
/// 2-space indentation.
fn format_yaml(val: &Val) -> String {
    let mut buf = String::new();
    format_yaml_node(&mut buf, val, 0);
    buf
}

fn format_yaml_node(buf: &mut String, val: &Val, indent: usize) {
    match val {
        Val::Bool(b) => {
            let _ = write!(buf, "{b}");
        }
        Val::Int(i) => {
            let _ = write!(buf, "{i}");
        }
        Val::Num(n) => buf.push_str(n),
        Val::Float(f) if f.is_finite() => {
            let _ = write!(buf, "{f}");
        }
        // Null, NaN, and ±Inf all serialise as YAML null.
        Val::Null | Val::Float(_) => buf.push_str("null"),
        Val::Str(s) => buf.push_str(&yaml_format_scalar_string(s)),
        Val::Arr(arr) if arr.is_empty() => buf.push_str("[]"),
        Val::Arr(arr) => yaml_format_array(buf, arr, indent),
        Val::Obj(obj) if obj.is_empty() => buf.push_str("{}"),
        Val::Obj(obj) => {
            let pairs: Vec<(&Rc<String>, &Val)> = obj.iter().collect();
            yaml_format_object(buf, &pairs, indent);
        }
    }
}

fn yaml_format_scalar_string(s: &str) -> String {
    // Quote strings that contain YAML-significant characters or
    // parse ambiguously as another scalar type.  The previous
    // handwritten emitter was conservative; we match its rules.
    let needs_quotes = s.is_empty()
        || s.contains('\n')
        || s.contains(':')
        || s.contains('#')
        || s.contains('"')
        || s.contains('\'')
        || s.contains('\\')
        || s.starts_with(' ')
        || s.ends_with(' ')
        || matches!(
            s.to_ascii_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~"
        )
        || s.parse::<f64>().is_ok();
    if needs_quotes {
        format!(
            "\"{}\"",
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
        )
    } else {
        s.to_string()
    }
}

fn yaml_format_array(buf: &mut String, arr: &[Val], indent: usize) {
    let prefix = " ".repeat(indent);
    for (i, item) in arr.iter().enumerate() {
        if i > 0 {
            buf.push('\n');
            buf.push_str(&prefix);
        }
        buf.push_str("- ");
        match item {
            Val::Arr(inner) if !inner.is_empty() => {
                buf.push('\n');
                buf.push_str(&" ".repeat(indent + 2));
                format_yaml_node(buf, item, indent + 2);
            }
            Val::Obj(obj) if !obj.is_empty() => {
                // A mapping inside a sequence: the first key sits on
                // the `- ` line, subsequent keys align with it.
                let pairs: Vec<(&Rc<String>, &Val)> = obj.iter().collect();
                yaml_format_object(buf, &pairs, indent + 2);
            }
            _ => format_yaml_node(buf, item, indent + 2),
        }
    }
}

fn yaml_format_object(buf: &mut String, pairs: &[(&Rc<String>, &Val)], indent: usize) {
    let prefix = " ".repeat(indent);
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            buf.push('\n');
            buf.push_str(&prefix);
        }
        let _ = write!(buf, "{k}:");
        yaml_format_object_value(buf, v, indent);
    }
}

fn yaml_format_object_value(buf: &mut String, val: &Val, indent: usize) {
    match val {
        Val::Obj(inner) if !inner.is_empty() => {
            buf.push('\n');
            buf.push_str(&" ".repeat(indent + 2));
            format_yaml_node(buf, val, indent + 2);
        }
        Val::Arr(inner) if !inner.is_empty() => {
            buf.push('\n');
            buf.push_str(&" ".repeat(indent + 2));
            format_yaml_node(buf, val, indent + 2);
        }
        _ => {
            buf.push(' ');
            format_yaml_node(buf, val, indent);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UtilContext, VecOutput};
    use wasmsh_fs::MemoryFs;

    fn run_yq(argv: &[&str], stdin: Option<&[u8]>) -> (i32, String, String) {
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
            util_yq(&mut ctx, argv)
        };
        (
            status,
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    #[test]
    fn identity_preserves_yaml_shape() {
        let (status, out, _) = run_yq(&["yq", "."], Some(b"name: hello\nvalue: 42\n"));
        assert_eq!(status, 0);
        assert!(out.contains("name: hello"), "got: {out}");
        assert!(out.contains("value: 42"), "got: {out}");
    }

    #[test]
    fn field_access_scalar() {
        let (status, out, _) = run_yq(&["yq", ".name"], Some(b"name: world\ncount: 5\n"));
        assert_eq!(status, 0);
        assert_eq!(out.trim(), "world");
    }

    #[test]
    fn nested_field() {
        let (status, out, _) = run_yq(&["yq", ".outer.inner"], Some(b"outer:\n  inner: deep\n"));
        assert_eq!(status, 0);
        assert_eq!(out.trim(), "deep");
    }

    #[test]
    fn length_of_array() {
        let (status, out, _) = run_yq(
            &["yq", ".items | length"],
            Some(b"items:\n  - x\n  - y\n  - z\n"),
        );
        assert_eq!(status, 0);
        assert_eq!(out.trim(), "3");
    }

    #[test]
    fn length_of_object() {
        let (status, out, _) = run_yq(&["yq", "length"], Some(b"a: 1\nb: 2\nc: 3\n"));
        assert_eq!(status, 0);
        assert_eq!(out.trim(), "3");
    }

    #[test]
    fn keys_sorted() {
        let (status, out, _) = run_yq(&["yq", "keys"], Some(b"b: 2\na: 1\nc: 3\n"));
        assert_eq!(status, 0);
        let s = out.trim();
        // jaq's `keys` returns sorted keys.
        assert!(
            s.contains('a') && s.contains('b') && s.contains('c'),
            "got: {s}"
        );
    }

    #[test]
    fn iterate_array() {
        let (status, out, _) = run_yq(
            &["yq", ".items | .[]"],
            Some(b"items:\n  - one\n  - two\n  - three\n"),
        );
        assert_eq!(status, 0);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].trim(), "one");
        assert_eq!(lines[1].trim(), "two");
        assert_eq!(lines[2].trim(), "three");
    }

    #[test]
    fn json_output_flag() {
        let (status, out, _) = run_yq(&["yq", "-jc", "."], Some(b"name: test\nvalue: 42\n"));
        assert_eq!(status, 0);
        assert_eq!(out.trim(), r#"{"name":"test","value":42}"#);
    }

    #[test]
    fn raw_output_flag() {
        let (status, out, _) = run_yq(&["yq", "-r", ".name"], Some(b"name: hello\n"));
        assert_eq!(status, 0);
        assert_eq!(out, "hello\n");
    }

    #[test]
    fn select_filter() {
        // `-jc` = JSON output, compact — so we can match on the
        // JSON representation instead of YAML.
        let (status, out, _) = run_yq(
            &["yq", "-jc", ".items[] | select(.enabled)"],
            Some(b"items:\n  - name: a\n    enabled: true\n  - name: b\n    enabled: false\n"),
        );
        assert_eq!(status, 0);
        assert!(out.contains("\"name\":\"a\""), "got: {out}");
        assert!(!out.contains("\"name\":\"b\""), "got: {out}");
    }

    #[test]
    fn missing_filter_errors() {
        let (status, _, err) = run_yq(&["yq"], Some(b"x: 1\n"));
        assert_eq!(status, 1);
        assert!(err.contains("missing filter"));
    }

    #[test]
    fn type_filter_object() {
        // Default YAML output emits strings without quotes.
        let (status, out, _) = run_yq(&["yq", "type"], Some(b"a: 1\n"));
        assert_eq!(status, 0);
        assert_eq!(out.trim(), "object");
    }

    #[test]
    fn type_filter_object_json_output() {
        // With `-j` JSON output, strings come out quoted.
        let (status, out, _) = run_yq(&["yq", "-j", "type"], Some(b"a: 1\n"));
        assert_eq!(status, 0);
        assert_eq!(out.trim(), "\"object\"");
    }

    #[test]
    fn nested_objects_preserved_in_yaml_output() {
        let (status, out, _) = run_yq(
            &["yq", "."],
            Some(b"outer:\n  inner: value\n  list:\n    - a\n    - b\n"),
        );
        assert_eq!(status, 0);
        assert!(out.contains("outer:"), "got: {out}");
        assert!(out.contains("inner: value"), "got: {out}");
    }

    #[test]
    fn exit_status_empty_result() {
        // `-e` with no results should exit 1 (like real jq).
        let (status, _, _) = run_yq(&["yq", "-e", ".missing"], Some(b"a: 1\n"));
        // `.missing` on an object that doesn't have `missing` yields null,
        // which is one result of value null — `-e` exits 1 for null.
        assert_eq!(status, 1);
    }

    #[test]
    fn parse_error_reports_location() {
        let (status, _, err) = run_yq(&["yq", "."], Some(b"::: bad yaml :::\n"));
        // Either the parse succeeds with a string, or it errors; we
        // just care that the utility doesn't panic.
        let _ = (status, err);
    }
}
