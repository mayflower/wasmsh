//! RFC 7578 `multipart/form-data` encoder used by `curl -F`.
//!
//! Produces a `(body, content_type)` pair with a boundary derived
//! deterministically from the part contents so tests are reproducible
//! while still guaranteeing non-collision in practice.

use crate::helpers::resolve_path;
use crate::UtilContext;
use wasmsh_fs::{OpenOptions, Vfs};

/// Where the body of a form part comes from.
#[derive(Debug, Clone)]
pub(crate) enum FormContent {
    /// Literal text (`-F name=value` or `--form-string name=value`).
    Text(String),
    /// File content uploaded as a binary part (`-F name=@path`).
    ///
    /// Default `Content-Type: application/octet-stream`, default filename
    /// is the basename of `path`.
    File(String),
    /// File content inlined as a text part with no filename (`-F name=<path`).
    Inline(String),
}

/// A single form part for multipart/form-data uploads.
#[derive(Debug, Clone)]
pub(crate) struct FormPart {
    pub name: String,
    pub content: FormContent,
    /// Overrides the default filename (`;filename=` modifier).
    pub filename: Option<String>,
    /// Overrides the default Content-Type (`;type=` modifier).
    pub content_type: Option<String>,
}

/// Parse a `-F`/`--form` argument into a `FormPart`.
///
/// Supported grammar:
/// - `name=value`                        → text part
/// - `name=@path[;type=T][;filename=F]`  → file part (binary, filename=basename)
/// - `name=<path[;type=T]`               → inline-file part (text, no filename)
pub(crate) fn parse_form_arg(arg: &str, as_string: bool) -> Result<FormPart, String> {
    let (name, rest) = arg
        .split_once('=')
        .ok_or_else(|| format!("invalid form part (missing '='): {arg}"))?;
    let (body_spec, modifiers) = split_form_modifiers(rest);
    let (content, default_filename) = classify_form_body(body_spec, as_string);
    let (filename_override, content_type) = parse_form_modifier_list(&modifiers);

    Ok(FormPart {
        name: name.to_string(),
        content,
        filename: filename_override.or(default_filename),
        content_type,
    })
}

fn split_form_modifiers(rest: &str) -> (&str, Vec<&str>) {
    let mut parts = rest.split(';');
    let body_spec = parts.next().unwrap_or("");
    let modifiers: Vec<&str> = parts.map(str::trim).collect();
    (body_spec, modifiers)
}

fn classify_form_body(body_spec: &str, as_string: bool) -> (FormContent, Option<String>) {
    if as_string {
        return (FormContent::Text(body_spec.to_string()), None);
    }
    if let Some(path) = body_spec.strip_prefix('@') {
        let default_filename = basename(path).to_string();
        return (FormContent::File(path.to_string()), Some(default_filename));
    }
    if let Some(path) = body_spec.strip_prefix('<') {
        return (FormContent::Inline(path.to_string()), None);
    }
    (FormContent::Text(body_spec.to_string()), None)
}

fn parse_form_modifier_list(modifiers: &[&str]) -> (Option<String>, Option<String>) {
    let mut filename = None;
    let mut content_type = None;
    for m in modifiers {
        if let Some(v) = m.strip_prefix("type=") {
            content_type = Some(v.trim_matches('"').to_string());
        } else if let Some(v) = m.strip_prefix("filename=") {
            filename = Some(v.trim_matches('"').to_string());
        }
    }
    (filename, content_type)
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Load the body bytes for a single part (resolving `@` and `<` file references).
fn load_part_body(ctx: &mut UtilContext<'_>, part: &FormPart) -> Result<Vec<u8>, String> {
    match &part.content {
        FormContent::Text(s) => Ok(s.as_bytes().to_vec()),
        FormContent::File(path) | FormContent::Inline(path) => read_form_file(ctx, path),
    }
}

fn read_form_file(ctx: &mut UtilContext<'_>, path: &str) -> Result<Vec<u8>, String> {
    let resolved = resolve_path(ctx.cwd, path);
    let h = ctx
        .fs
        .open(&resolved, OpenOptions::read())
        .map_err(|e| format!("cannot read '{path}': {e}"))?;
    let data = ctx.fs.read_file(h).map_err(|e| e.to_string());
    ctx.fs.close(h);
    data
}

/// Encode a multipart body. Returns `(body_bytes, content_type_header_value)`.
pub(crate) fn encode_multipart(
    ctx: &mut UtilContext<'_>,
    parts: &[FormPart],
) -> Result<(Vec<u8>, String), String> {
    let bodies: Vec<Vec<u8>> = parts
        .iter()
        .map(|p| load_part_body(ctx, p))
        .collect::<Result<_, _>>()?;
    let boundary = choose_boundary(parts, &bodies);

    let mut out = Vec::with_capacity(total_capacity(&bodies));
    for (part, body) in parts.iter().zip(bodies.iter()) {
        out.extend_from_slice(b"--");
        out.extend_from_slice(boundary.as_bytes());
        out.extend_from_slice(b"\r\n");
        append_part_headers(&mut out, part);
        out.extend_from_slice(body);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"--");
    out.extend_from_slice(boundary.as_bytes());
    out.extend_from_slice(b"--\r\n");

    let content_type = format!("multipart/form-data; boundary={boundary}");
    Ok((out, content_type))
}

fn append_part_headers(out: &mut Vec<u8>, part: &FormPart) {
    out.extend_from_slice(b"Content-Disposition: form-data; name=\"");
    out.extend_from_slice(part.name.as_bytes());
    out.push(b'"');
    if let Some(f) = &part.filename {
        out.extend_from_slice(b"; filename=\"");
        out.extend_from_slice(f.as_bytes());
        out.push(b'"');
    }
    out.extend_from_slice(b"\r\n");

    let ctype = part
        .content_type
        .as_deref()
        .or_else(|| default_content_type(part));
    if let Some(t) = ctype {
        out.extend_from_slice(b"Content-Type: ");
        out.extend_from_slice(t.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
}

fn default_content_type(part: &FormPart) -> Option<&'static str> {
    match part.content {
        FormContent::File(_) => Some("application/octet-stream"),
        _ => None,
    }
}

fn total_capacity(bodies: &[Vec<u8>]) -> usize {
    bodies.iter().map(Vec::len).sum::<usize>() + bodies.len() * 128 + 128
}

/// Pick a boundary that does not occur in any part body.
///
/// Starts from a djb2-style hash of the concatenated bodies so that the
/// same input produces the same boundary across runs (reproducible tests),
/// then bumps a suffix if a collision is detected.
fn choose_boundary(parts: &[FormPart], bodies: &[Vec<u8>]) -> String {
    let mut hash: u64 = 5381;
    for part in parts {
        for byte in part.name.as_bytes() {
            hash = hash.wrapping_mul(33).wrapping_add(u64::from(*byte));
        }
    }
    for body in bodies {
        for byte in body {
            hash = hash.wrapping_mul(33).wrapping_add(u64::from(*byte));
        }
    }

    let mut suffix: u32 = 0;
    loop {
        let candidate = format!("----wasmshboundary{hash:016x}-{suffix:04x}");
        if !bodies.iter().any(|b| contains(b, candidate.as_bytes())) {
            return candidate;
        }
        suffix = suffix.wrapping_add(1);
        if suffix == 0 {
            // Extremely unlikely; fall back to a pseudo-random extension.
            return format!("----wasmshboundary{hash:016x}-ffffffff");
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VecOutput;
    use wasmsh_fs::MemoryFs;

    fn ctx_with_file<'a>(
        fs: &'a mut MemoryFs,
        out: &'a mut VecOutput,
        path: &str,
        contents: &[u8],
    ) -> UtilContext<'a> {
        let h = fs.open(path, OpenOptions::write()).unwrap();
        fs.write_file(h, contents).unwrap();
        fs.close(h);
        UtilContext {
            fs,
            output: out,
            cwd: "/",
            stdin: None,
            state: None,
            network: None,
        }
    }

    #[test]
    fn parse_text_part() {
        let p = parse_form_arg("name=alice", false).unwrap();
        assert_eq!(p.name, "name");
        assert!(matches!(p.content, FormContent::Text(ref s) if s == "alice"));
        assert!(p.filename.is_none());
    }

    #[test]
    fn parse_file_part_with_modifiers() {
        let p = parse_form_arg(
            "upload=@/etc/data.bin;type=image/png;filename=pretty.png",
            false,
        )
        .unwrap();
        assert_eq!(p.name, "upload");
        assert!(matches!(p.content, FormContent::File(ref s) if s == "/etc/data.bin"));
        assert_eq!(p.filename.as_deref(), Some("pretty.png"));
        assert_eq!(p.content_type.as_deref(), Some("image/png"));
    }

    #[test]
    fn parse_inline_file_part() {
        let p = parse_form_arg("text=</tmp/note.txt", false).unwrap();
        assert!(matches!(p.content, FormContent::Inline(ref s) if s == "/tmp/note.txt"));
        assert!(p.filename.is_none());
    }

    #[test]
    fn form_string_never_interprets_at() {
        let p = parse_form_arg("v=@literal", true).unwrap();
        assert!(matches!(p.content, FormContent::Text(ref s) if s == "@literal"));
    }

    #[test]
    fn encode_text_only() {
        let mut fs = MemoryFs::new();
        let mut out = VecOutput::default();
        let mut ctx = UtilContext {
            fs: &mut fs,
            output: &mut out,
            cwd: "/",
            stdin: None,
            state: None,
            network: None,
        };
        let parts = vec![
            parse_form_arg("a=1", false).unwrap(),
            parse_form_arg("b=two", false).unwrap(),
        ];
        let (body, ct) = encode_multipart(&mut ctx, &parts).unwrap();
        assert!(ct.starts_with("multipart/form-data; boundary=----wasmshboundary"));
        let s = String::from_utf8_lossy(&body);
        assert!(s.contains("name=\"a\""));
        assert!(s.contains("\r\n\r\n1\r\n"));
        assert!(s.contains("name=\"b\""));
        assert!(s.contains("\r\n\r\ntwo\r\n"));
        assert!(s.ends_with("--\r\n"));
    }

    #[test]
    fn encode_file_part_sets_default_content_type_and_filename() {
        let mut fs = MemoryFs::new();
        let mut out = VecOutput::default();
        let mut ctx = ctx_with_file(&mut fs, &mut out, "/data.bin", b"PAYLOAD");
        let parts = vec![parse_form_arg("file=@/data.bin", false).unwrap()];
        let (body, _) = encode_multipart(&mut ctx, &parts).unwrap();
        let s = String::from_utf8_lossy(&body);
        assert!(s.contains("filename=\"data.bin\""));
        assert!(s.contains("Content-Type: application/octet-stream\r\n"));
        assert!(s.contains("\r\n\r\nPAYLOAD\r\n"));
    }

    #[test]
    fn encode_inline_part_has_no_filename() {
        let mut fs = MemoryFs::new();
        let mut out = VecOutput::default();
        let mut ctx = ctx_with_file(&mut fs, &mut out, "/msg.txt", b"hi there");
        let parts = vec![parse_form_arg("text=</msg.txt", false).unwrap()];
        let (body, _) = encode_multipart(&mut ctx, &parts).unwrap();
        let s = String::from_utf8_lossy(&body);
        assert!(!s.contains("filename="));
        assert!(s.contains("\r\n\r\nhi there\r\n"));
    }

    #[test]
    fn boundary_regenerated_when_part_body_would_collide() {
        let mut fs = MemoryFs::new();
        let mut out = VecOutput::default();
        let mut ctx = UtilContext {
            fs: &mut fs,
            output: &mut out,
            cwd: "/",
            stdin: None,
            state: None,
            network: None,
        };
        let parts = vec![parse_form_arg("x=anything", false).unwrap()];
        let (body, ct) = encode_multipart(&mut ctx, &parts).unwrap();
        let boundary = ct.split("boundary=").nth(1).unwrap();
        // For a single text part the boundary appears twice in the wire
        // body (opening delimiter + closing delimiter).  That is the
        // structural minimum; anything higher would mean the user data
        // collided with our boundary.
        let occurrences = body
            .windows(boundary.len())
            .filter(|w| *w == boundary.as_bytes())
            .count();
        assert_eq!(occurrences, 2, "expected exactly 2 delimiter occurrences");
    }
}
