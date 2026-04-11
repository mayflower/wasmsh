//! Thin shared wrapper around `jaq-core` for compiling and running
//! jq-style filters.
//!
//! This module exists so that both `jq_ops` and `yaml_ops` can reuse
//! exactly the same filter compiler and execution loop — real jq and
//! real yq differ only in their input/output serialisation, not in
//! the filter language.  See ADR-0026 and ADR-0027.

use std::rc::Rc;

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{Compiler, Ctx, Filter, Native, RcIter};
use jaq_json::Val;

/// Compile a jq filter source string with the standard library
/// (`jaq-std` + `jaq-json`) and the supplied named global variables.
///
/// Variable names must include the leading `$` (e.g. `"$foo"`), which
/// is how jaq's parser records them.
///
/// Returns a `String` error containing a `Debug`-formatted copy of the
/// load or compile failure — the caller is responsible for prefixing
/// it with the utility name (e.g. `"jq: error parsing filter: …"`).
///
/// # Errors
///
/// Returns a human-readable error string if the pattern cannot be
/// parsed or compiled.
pub(crate) fn compile_filter(
    source: &str,
    var_names: &[&str],
) -> Result<Filter<Native<Val>>, String> {
    let arena = Arena::default();
    let loader = Loader::new(jaq_std::defs().chain(jaq_json::defs()));
    let program = File {
        code: source,
        path: (),
    };
    let modules = loader
        .load(&arena, program)
        .map_err(|errs| format!("{errs:?}"))?;
    Compiler::default()
        .with_funs(jaq_std::funs().chain(jaq_json::funs()))
        .with_global_vars(var_names.iter().copied())
        .compile(modules)
        .map_err(|errs| format!("{errs:?}"))
}

/// Execute a compiled filter against a single input value, returning
/// every produced output value plus any runtime error encountered.
///
/// `vars` must be supplied in the same order as the `var_names`
/// passed to [`compile_filter`].  This matches jaq's convention that
/// the Nth global var binds to the Nth entry in the context's
/// variable list.
pub(crate) fn run_filter(
    filter: &Filter<Native<Val>>,
    input: Val,
    vars: &[Val],
) -> (Vec<Val>, Option<String>) {
    let empty_inputs: RcIter<core::iter::Empty<Result<Val, String>>> =
        RcIter::new(core::iter::empty());
    let ctx = Ctx::new(vars.iter().cloned(), &empty_inputs);
    let mut out = Vec::new();
    let mut err = None;
    for result in filter.run((ctx, input)) {
        match result {
            Ok(val) => out.push(val),
            Err(e) => {
                err = Some(format!("{e:?}"));
                break;
            }
        }
    }
    (out, err)
}

/// Parse a single JSON value from a string via hifijson + jaq-json.
///
/// # Errors
///
/// Returns a human-readable error string if the input is not a
/// valid JSON document.
pub(crate) fn parse_json_single(s: &str) -> Result<Val, String> {
    use hifijson::token::Lex;
    let mut lexer = hifijson::SliceLexer::new(s.as_bytes());
    lexer
        .exactly_one(Val::parse)
        .map_err(|e| format!("{e}"))
}

/// Parse a stream of whitespace-separated JSON values (jq's convention:
/// a file may contain multiple concatenated JSON documents).
///
/// # Errors
///
/// Returns a human-readable error string if any document in the
/// stream fails to parse.
pub(crate) fn parse_json_all(s: &str) -> Result<Vec<Val>, String> {
    use hifijson::token::Lex;
    let mut out = Vec::new();
    let mut lexer = hifijson::SliceLexer::new(s.as_bytes());
    while let Some(token) = lexer.ws_token() {
        let val = Val::parse(token, &mut lexer).map_err(|e| format!("{e}"))?;
        out.push(val);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Pretty-printer (shared between jq and yq's JSON-output mode)
// ---------------------------------------------------------------------------

/// Format a Val in either compact or jq-style pretty form.
///
/// jaq-json's own `Display` impl is compact-only — it ignores the
/// `{:#}` alternate format flag — so we walk the value tree directly
/// and emit 2-space indentation matching real jq output byte-for-byte.
pub(crate) fn format_json(buf: &mut String, val: &Val, compact: bool) {
    use core::fmt::Write;
    if compact {
        let _ = write!(buf, "{val}");
    } else {
        format_json_pretty(buf, val, 0);
    }
}

fn format_json_pretty(buf: &mut String, val: &Val, indent: usize) {
    use core::fmt::Write;
    match val {
        Val::Arr(arr) if !arr.is_empty() => {
            buf.push_str("[\n");
            let inner = indent + 1;
            let pad = "  ".repeat(inner);
            for (i, item) in arr.iter().enumerate() {
                buf.push_str(&pad);
                format_json_pretty(buf, item, inner);
                if i + 1 < arr.len() {
                    buf.push(',');
                }
                buf.push('\n');
            }
            buf.push_str(&"  ".repeat(indent));
            buf.push(']');
        }
        Val::Obj(obj) if !obj.is_empty() => {
            buf.push_str("{\n");
            let inner = indent + 1;
            let pad = "  ".repeat(inner);
            let entries: Vec<_> = obj.iter().collect();
            for (i, (k, v)) in entries.iter().enumerate() {
                buf.push_str(&pad);
                let key = Val::Str(Rc::clone(k));
                let _ = write!(buf, "{key}: ");
                format_json_pretty(buf, v, inner);
                if i + 1 < entries.len() {
                    buf.push(',');
                }
                buf.push('\n');
            }
            buf.push_str(&"  ".repeat(indent));
            buf.push('}');
        }
        // Scalars and empty composites use the compact Display impl,
        // which is already correct for the pretty output.
        _ => {
            let _ = write!(buf, "{val}");
        }
    }
}
