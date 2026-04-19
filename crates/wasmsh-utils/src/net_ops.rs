//! Network utilities: `curl` and `wget`.
//!
//! The curl implementation covers the flag surface that matters for a
//! shell sandbox: request shaping (`-d`/`--data-*`/`--json`, `-F` multipart,
//! `-u`, `-A`, `-e`), response shaping (`-i`, `-D`, `-O`/`-J`,
//! `--output-dir`, `--create-dirs`, extended `-w` tokens), and sandbox
//! limits (`--max-time`, `--connect-timeout`, `--max-filesize`,
//! `--max-redirs`, `--retry*`).  Unsupported curl flags that would depend
//! on protocols or capabilities the sandbox does not expose (FTP, proxies,
//! client certificates, Unix sockets, …) are rejected with a clear
//! "not supported in sandbox" diagnostic rather than silently accepted or
//! reported as "unknown option".

use crate::helpers::resolve_path;
use crate::net_multipart::{encode_multipart, parse_form_arg, FormPart};
use crate::net_types::{HttpRequest, HttpResponse, NetworkBackend, NetworkError};
use crate::UtilContext;
use base64::engine::general_purpose::STANDARD as B64_STANDARD;
use base64::Engine as _;
use wasmsh_fs::{OpenOptions, Vfs};

// ── Data pieces (`-d`, `--data-*`, `--json`) ────────────────────

#[derive(Debug, Clone)]
enum DataPiece {
    /// `-d LITERAL` — CR/LF stripped.
    Ascii(Vec<u8>),
    /// `-d @file` — CR/LF stripped from file contents.
    AsciiFile(String),
    /// `--data-binary LITERAL` — no processing.
    Binary(Vec<u8>),
    /// `--data-binary @file` — verbatim file contents.
    BinaryFile(String),
    /// `--data-raw LITERAL` — `@` not interpreted.
    Raw(Vec<u8>),
    /// `--data-urlencode` spec (`value`, `=value`, `name=value`, `@file`,
    /// `name@file`, `name=value@unused` — kept as a raw spec for lazy
    /// resolution at build time).
    UrlEncode(String),
    /// `--json LITERAL` — literal JSON.
    Json(Vec<u8>),
    /// `--json @file` — JSON read from file.
    JsonFile(String),
}

fn is_json_piece(p: &DataPiece) -> bool {
    matches!(p, DataPiece::Json(_) | DataPiece::JsonFile(_))
}

fn strip_crlf(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .copied()
        .filter(|b| *b != b'\r' && *b != b'\n')
        .collect()
}

// ── CurlOpts ────────────────────────────────────────────────────

#[allow(clippy::struct_excessive_bools)]
#[derive(Default)]
struct CurlOpts {
    urls: Vec<String>,
    remote_name_all: bool,
    aws_sigv4: Option<String>,
    method: Option<String>,
    headers: Vec<(String, String)>,
    data_pieces: Vec<DataPiece>,
    form_parts: Vec<FormPart>,
    output: Option<String>,
    output_dir: Option<String>,
    create_dirs: bool,
    remote_name: bool,
    remote_header_name: bool,
    include_headers: bool,
    dump_header: Option<String>,
    user: Option<String>,
    user_agent: Option<String>,
    referer: Option<String>,
    silent: bool,
    show_error: bool,
    follow_redirects: bool,
    head_only: bool,
    fail_on_error: bool,
    fail_with_body: bool,
    verbose: bool,
    write_out: Option<String>,
    max_time_ms: Option<u64>,
    connect_timeout_ms: Option<u64>,
    max_filesize: Option<u64>,
    max_redirs: Option<u32>,
    retry: u32,
    retry_delay_ms: u64,
    retry_max_time_ms: Option<u64>,
    retry_connrefused: bool,
    retry_all_errors: bool,
    get_via_url: bool,
    compressed: bool,
    range: Option<String>,
    upload_file: Option<String>,
    cookie: Option<String>,
    oauth2_bearer: Option<String>,
    time_cond: Option<String>,
    netrc_file: Option<String>,
    netrc_enabled: bool,
}

// ── Argument parsing ────────────────────────────────────────────

type ArgResult<T> = Result<T, String>;

struct ArgCursor<'a> {
    argv: &'a [&'a str],
    i: usize,
}

impl ArgCursor<'_> {
    fn take_value(&mut self, flag: &str) -> ArgResult<String> {
        self.i += 1;
        self.argv
            .get(self.i)
            .map(|s| (*s).to_string())
            .ok_or_else(|| format!("{flag} requires an argument"))
    }
}

/// Parse an argv into one or more `CurlOpts` segments.
///
/// Segments are delimited by `--next` / `-:` — each segment owns its own
/// flags and URL list, and runs independently (like real curl).  Before
/// parsing, `-K`/`--config FILE` entries are expanded in place so that
/// option files compose cleanly with the command-line flags.
fn parse_curl_args(ctx: &mut UtilContext<'_>, argv: &[&str]) -> ArgResult<Vec<CurlOpts>> {
    let expanded = expand_config_files(ctx, argv)?;
    let tokens: Vec<&str> = expanded.iter().map(String::as_str).collect();
    let segments = split_on_next(&tokens);
    segments
        .iter()
        .map(|seg| parse_single_segment(seg))
        .collect()
}

fn parse_single_segment(argv: &[&str]) -> ArgResult<CurlOpts> {
    let mut opts = CurlOpts::default();
    let mut cur = ArgCursor { argv, i: 0 };

    while cur.i < argv.len() {
        let arg = argv[cur.i];
        if arg.starts_with("--") {
            parse_long_option(&mut opts, arg, &mut cur)?;
        } else if arg.starts_with('-') && arg.len() > 1 {
            parse_short_option(&mut opts, arg, &mut cur)?;
        } else {
            opts.urls.push(arg.to_string());
        }
        cur.i += 1;
    }
    Ok(opts)
}

fn split_on_next<'a>(argv: &'a [&'a str]) -> Vec<Vec<&'a str>> {
    let mut out: Vec<Vec<&'a str>> = Vec::new();
    let mut cur: Vec<&'a str> = Vec::new();
    // Skip argv[0] which is the program name ("curl").
    let mut first = true;
    for a in argv {
        if first {
            first = false;
            continue;
        }
        if *a == "--next" || *a == "-:" {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(*a);
        }
    }
    out.push(cur);
    out
}

/// Expand every `-K FILE`/`--config FILE` reference by splicing the file's
/// tokens into the argv stream at the same position.  Config files do not
/// recursively include other config files.
fn expand_config_files(ctx: &mut UtilContext<'_>, argv: &[&str]) -> ArgResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i];
        if matches!(arg, "-K" | "--config") {
            i += 1;
            let path = *argv
                .get(i)
                .ok_or_else(|| format!("{arg} requires a filename argument"))?;
            let tokens = load_config_file(ctx, path)?;
            out.extend(tokens);
            i += 1;
            continue;
        }
        out.push(arg.to_string());
        i += 1;
    }
    Ok(out)
}

fn load_config_file(ctx: &mut UtilContext<'_>, path: &str) -> ArgResult<Vec<String>> {
    let bytes = read_data_file(ctx, path).map_err(|e| format!("--config {path}: {e}"))?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let mut out = Vec::new();
    for line in text.lines() {
        out.extend(parse_config_line(line));
    }
    Ok(out)
}

/// Parse a single config-file line.  Blank lines and `#` comments are
/// skipped.  A line is split into `name [sep] value?`, where `sep` is
/// `=`, `:`, or whitespace; quoted values preserve spaces.
fn parse_config_line(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return Vec::new();
    }
    let (name_raw, rest) = split_config_name(trimmed);
    let name = normalize_config_name(name_raw);
    let value = extract_config_value(rest);
    let mut out = vec![name];
    if let Some(v) = value {
        out.push(v);
    }
    out
}

fn split_config_name(line: &str) -> (&str, &str) {
    let end = line
        .find(|c: char| c == '=' || c == ':' || c.is_whitespace())
        .unwrap_or(line.len());
    (&line[..end], &line[end..])
}

fn normalize_config_name(name: &str) -> String {
    if name.starts_with('-') {
        name.to_string()
    } else {
        format!("--{name}")
    }
}

fn extract_config_value(rest: &str) -> Option<String> {
    let trimmed = rest.trim_start_matches(|c: char| c == '=' || c == ':' || c.is_whitespace());
    if trimmed.is_empty() {
        return None;
    }
    Some(unquote_config_value(trimmed))
}

fn unquote_config_value(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[0] == bytes[bytes.len() - 1]
    {
        return s[1..s.len() - 1].to_string();
    }
    s.trim_end().to_string()
}

fn parse_long_option(opts: &mut CurlOpts, arg: &str, cur: &mut ArgCursor<'_>) -> ArgResult<()> {
    match arg {
        "--request" => opts.method = Some(cur.take_value("--request")?),
        "--header" => push_header(&mut opts.headers, &cur.take_value("--header")?)?,
        "--user-agent" => opts.user_agent = Some(cur.take_value("--user-agent")?),
        "--referer" => opts.referer = Some(cur.take_value("--referer")?),
        "--user" => opts.user = Some(cur.take_value("--user")?),
        "--data" | "--data-ascii" => {
            opts.data_pieces
                .push(parse_data_arg(&cur.take_value(arg)?, DataKind::Ascii));
        }
        "--data-binary" => {
            opts.data_pieces.push(parse_data_arg(
                &cur.take_value("--data-binary")?,
                DataKind::Binary,
            ));
        }
        "--data-raw" => {
            let v = cur.take_value("--data-raw")?;
            opts.data_pieces.push(DataPiece::Raw(v.into_bytes()));
        }
        "--data-urlencode" => {
            let v = cur.take_value("--data-urlencode")?;
            opts.data_pieces.push(DataPiece::UrlEncode(v));
        }
        "--json" => {
            opts.data_pieces
                .push(parse_data_arg(&cur.take_value("--json")?, DataKind::Json));
        }
        "--form" => opts
            .form_parts
            .push(parse_form_arg(&cur.take_value("--form")?, false)?),
        "--form-string" => opts
            .form_parts
            .push(parse_form_arg(&cur.take_value("--form-string")?, true)?),
        "--output" => opts.output = Some(cur.take_value("--output")?),
        "--output-dir" => opts.output_dir = Some(cur.take_value("--output-dir")?),
        "--create-dirs" => opts.create_dirs = true,
        "--remote-name" => opts.remote_name = true,
        "--remote-header-name" => opts.remote_header_name = true,
        "--include" => opts.include_headers = true,
        "--dump-header" => opts.dump_header = Some(cur.take_value("--dump-header")?),
        "--silent" => opts.silent = true,
        "--show-error" => opts.show_error = true,
        "--location" => opts.follow_redirects = true,
        "--head" => opts.head_only = true,
        "--fail" => opts.fail_on_error = true,
        "--verbose" => opts.verbose = true,
        "--write-out" => opts.write_out = Some(cur.take_value("--write-out")?),
        "--max-time" => {
            opts.max_time_ms = Some(parse_seconds_to_ms(&cur.take_value("--max-time")?)?);
        }
        "--connect-timeout" => {
            opts.connect_timeout_ms =
                Some(parse_seconds_to_ms(&cur.take_value("--connect-timeout")?)?);
        }
        "--max-filesize" => {
            opts.max_filesize = Some(parse_u64(&cur.take_value("--max-filesize")?)?);
        }
        "--max-redirs" => opts.max_redirs = Some(parse_u32(&cur.take_value("--max-redirs")?)?),
        "--retry" => opts.retry = parse_u32(&cur.take_value("--retry")?)?,
        "--retry-delay" => {
            opts.retry_delay_ms = parse_seconds_to_ms(&cur.take_value("--retry-delay")?)?;
        }
        "--retry-max-time" => {
            opts.retry_max_time_ms =
                Some(parse_seconds_to_ms(&cur.take_value("--retry-max-time")?)?);
        }
        "--retry-connrefused" => opts.retry_connrefused = true,
        "--retry-all-errors" => opts.retry_all_errors = true,
        "--url" => opts.urls.push(cur.take_value("--url")?),
        "--remote-name-all" => opts.remote_name_all = true,
        "--aws-sigv4" => opts.aws_sigv4 = Some(cur.take_value("--aws-sigv4")?),
        "--get" => opts.get_via_url = true,
        "--compressed" => opts.compressed = true,
        "--range" => opts.range = Some(cur.take_value("--range")?),
        "--fail-with-body" => opts.fail_with_body = true,
        "--upload-file" => opts.upload_file = Some(cur.take_value("--upload-file")?),
        "--cookie" => opts.cookie = Some(cur.take_value("--cookie")?),
        "--oauth2-bearer" => opts.oauth2_bearer = Some(cur.take_value("--oauth2-bearer")?),
        "--time-cond" => opts.time_cond = Some(cur.take_value("--time-cond")?),
        "--netrc" | "--netrc-optional" => opts.netrc_enabled = true,
        "--netrc-file" => opts.netrc_file = Some(cur.take_value("--netrc-file")?),
        other => {
            if is_silent_no_op(other, cur)? {
                return Ok(());
            }
            reject_or_unknown(other, cur)?;
        }
    }
    Ok(())
}

fn parse_short_option(opts: &mut CurlOpts, arg: &str, cur: &mut ArgCursor<'_>) -> ArgResult<()> {
    let flags = &arg[1..];
    let mut it = flags.chars().enumerate().peekable();
    while let Some((idx, ch)) = it.next() {
        match ch {
            's' => opts.silent = true,
            'S' => opts.show_error = true,
            'L' => opts.follow_redirects = true,
            'I' => opts.head_only = true,
            'f' => opts.fail_on_error = true,
            'v' => opts.verbose = true,
            'i' => opts.include_headers = true,
            'O' => opts.remote_name = true,
            'J' => opts.remote_header_name = true,
            'G' => opts.get_via_url = true,
            'n' => opts.netrc_enabled = true,
            // `-k` (insecure), `-Z` (parallel → we run sequentially), and
            // `-4/-6/-#/-0/-1/-2/-3` (IP/protocol version pins and
            // progress-bar UI) are all silent no-ops in the sandbox.
            'k' | 'Z' | '4' | '6' | '#' | '0' | '1' | '2' | '3' => {}
            'X' | 'H' | 'd' | 'o' | 'w' | 'u' | 'A' | 'e' | 'D' | 'F' | 'T' | 'b' | 'r' | 'z' => {
                let rest: String = flags[idx + 1..].to_string();
                consume_short_with_arg(opts, ch, &rest, cur)?;
                return Ok(());
            }
            'c' => {
                return Err(
                    "-c not supported in sandbox (option requires feature not exposed)".into(),
                );
            }
            other => return Err(format!("unknown option: -{other}")),
        }
        let _ = it.peek();
    }
    Ok(())
}

fn consume_short_with_arg(
    opts: &mut CurlOpts,
    ch: char,
    rest: &str,
    cur: &mut ArgCursor<'_>,
) -> ArgResult<()> {
    let value = if rest.is_empty() {
        cur.take_value(&format!("-{ch}"))?
    } else {
        rest.to_string()
    };
    match ch {
        'X' => opts.method = Some(value),
        'H' => push_header(&mut opts.headers, &value)?,
        'd' => opts
            .data_pieces
            .push(parse_data_arg(&value, DataKind::Ascii)),
        'o' => opts.output = Some(value),
        'w' => opts.write_out = Some(value),
        'u' => opts.user = Some(value),
        'A' => opts.user_agent = Some(value),
        'e' => opts.referer = Some(value),
        'D' => opts.dump_header = Some(value),
        'F' => opts.form_parts.push(parse_form_arg(&value, false)?),
        'T' => opts.upload_file = Some(value),
        'b' => opts.cookie = Some(value),
        'r' => opts.range = Some(value),
        'z' => opts.time_cond = Some(value),
        _ => unreachable!(),
    }
    Ok(())
}

fn push_header(headers: &mut Vec<(String, String)>, raw: &str) -> ArgResult<()> {
    if let Some((k, v)) = raw.split_once(':') {
        headers.push((k.trim().to_string(), v.trim().to_string()));
        Ok(())
    } else {
        Err(format!("invalid header format: {raw}"))
    }
}

// ── Data argument interpretation ────────────────────────────────

#[derive(Clone, Copy)]
enum DataKind {
    Ascii,
    Binary,
    Json,
}

fn parse_data_arg(v: &str, kind: DataKind) -> DataPiece {
    if let Some(path) = v.strip_prefix('@') {
        return match kind {
            DataKind::Ascii => DataPiece::AsciiFile(path.to_string()),
            DataKind::Binary => DataPiece::BinaryFile(path.to_string()),
            DataKind::Json => DataPiece::JsonFile(path.to_string()),
        };
    }
    match kind {
        DataKind::Ascii => DataPiece::Ascii(v.as_bytes().to_vec()),
        DataKind::Binary => DataPiece::Binary(v.as_bytes().to_vec()),
        DataKind::Json => DataPiece::Json(v.as_bytes().to_vec()),
    }
}

fn parse_seconds_to_ms(s: &str) -> ArgResult<u64> {
    let secs: f64 = s.parse().map_err(|_| format!("invalid duration: {s}"))?;
    if !secs.is_finite() || secs < 0.0 {
        return Err(format!("invalid duration: {s}"));
    }
    Ok((secs * 1000.0) as u64)
}

fn parse_u64(s: &str) -> ArgResult<u64> {
    s.parse().map_err(|_| format!("invalid number: {s}"))
}

fn parse_u32(s: &str) -> ArgResult<u32> {
    s.parse().map_err(|_| format!("invalid number: {s}"))
}

// ── Silent no-ops ───────────────────────────────────────────────

/// Flags that are meaningful to real curl but irrelevant inside the
/// wasmsh sandbox (progress/tracing UI, HTTP-version selection, TCP
/// tuning, IPv4/v6 pinning, DNS overrides etc.).  Accepting them as
/// no-ops lets scripts written for a real curl run unchanged rather
/// than erroring out on flags that the host transport already handles
/// or that are simply cosmetic.
const SILENT_NO_OP_FLAGS_WITH_ARG: &[&str] = &[
    "--parallel-max",
    "--resolve",
    "--happy-eyeballs-timeout-ms",
    "--expect100-timeout",
    "--interface",
    "--local-port",
    "--dns-interface",
    "--dns-ipv4-addr",
    "--dns-ipv6-addr",
    "--dns-servers",
    "--keepalive-time",
    "--speed-limit",
    "--speed-time",
    "--tftp-blksize",
    "--limit-rate",
];

const SILENT_NO_OP_FLAGS: &[&str] = &[
    "--parallel",
    "--parallel-immediate",
    "--progress-bar",
    "--no-progress-meter",
    "--no-buffer",
    "--styled-output",
    "--no-alpn",
    "--no-npn",
    "--no-keepalive",
    "--no-sessionid",
    "--tcp-nodelay",
    "--tcp-fastopen",
    "--ipv4",
    "--ipv6",
    "--path-as-is",
    "--http0.9",
    "--http1.0",
    "--http1.1",
    "--http2",
    "--http2-prior-knowledge",
    "--http3",
    "--http3-only",
    "--ssl",
    "--ssl-reqd",
    "--tlsv1",
    "--tlsv1.0",
    "--tlsv1.1",
    "--tlsv1.2",
    "--tlsv1.3",
    "--false-start",
    "--alpn",
    "--tr-encoding",
    "--ca-native",
    "--trace-time",
    "--trace-ids",
];

fn is_silent_no_op(opt: &str, cur: &mut ArgCursor<'_>) -> ArgResult<bool> {
    if SILENT_NO_OP_FLAGS.contains(&opt) {
        return Ok(true);
    }
    if SILENT_NO_OP_FLAGS_WITH_ARG.contains(&opt) {
        // Consume and discard the argument.
        let _ = cur.take_value(opt)?;
        return Ok(true);
    }
    Ok(false)
}

// ── Tier-4 rejection ────────────────────────────────────────────

/// If `opt` is a known-but-unsupported long flag, fail with a sandbox
/// diagnostic; otherwise fall through to an "unknown option" error.
fn reject_or_unknown(opt: &str, cur: &mut ArgCursor<'_>) -> ArgResult<()> {
    if let Some(consumes_arg) = classify_rejected(opt) {
        if consumes_arg {
            cur.i += 1; // consume and discard the argument so error messages are predictable
        }
        return Err(format!("{opt} not supported in sandbox"));
    }
    Err(format!("unknown option: {opt}"))
}

/// Returns `Some(true)` if the unsupported flag takes an argument,
/// `Some(false)` if it is a bare switch, `None` if we don't recognize it.
fn classify_rejected(opt: &str) -> Option<bool> {
    for prefix in REJECTED_PREFIXES_WITH_ARG {
        if opt.starts_with(prefix) {
            return Some(true);
        }
    }
    for prefix in REJECTED_PREFIXES_NO_ARG {
        if opt.starts_with(prefix) {
            return Some(false);
        }
    }
    for (exact, arg) in REJECTED_EXACT {
        if *exact == opt {
            return Some(*arg);
        }
    }
    None
}

const REJECTED_PREFIXES_WITH_ARG: &[&str] = &[
    "--proxy",
    "--socks4",
    "--socks5",
    "--preproxy",
    "--haproxy",
    "--ftp-",
    "--tftp-",
    "--mail-",
    "--pop3-",
    "--imap-",
    "--smtp-",
    "--ldap-",
    "--cert",
    "--capath",
    "--cacert",
    "--key",
    "--ciphers",
    "--tlsauth",
    "--tls-",
    "--tlsv",
    "--krb",
    "--negotiate",
    "--ntlm",
    "--dns-",
    "--doh-",
    "--resolve",
    "--interface",
    "--local-port",
    "--hostpub",
    "--pubkey",
    "--pinnedpubkey",
    "--crlfile",
    "--engine",
    "--egd-file",
    "--random-file",
    "--curves",
    "--aws-sigv4",
    "--oauth2-bearer",
    "--sasl-authzid",
    "--service-name",
    "--alt-svc",
    "--hsts",
    "--ipfs-gateway",
    "--unix-socket",
    "--abstract-unix-socket",
    "--expect100-timeout",
    "--happy-eyeballs-timeout-ms",
    "--connect-to",
    "--speed-limit",
    "--speed-time",
    "--keepalive-time",
    "--login-options",
    "--delegation",
];

const REJECTED_PREFIXES_NO_ARG: &[&str] = &[
    "--ftp-",
    "--ntlm",
    "--negotiate",
    "--digest",
    "--basic",
    "--anyauth",
    "--tcp-",
    "--ssl",
    "--sslv",
    "--http0.9",
    "--http1.0",
    "--http1.1",
    "--http2",
    "--http3",
    "--false-start",
    "--path-as-is",
    "--compressed-ssh",
    "--no-alpn",
    "--no-npn",
    "--no-sessionid",
    "--no-keepalive",
    "--metalink",
    "--crlf",
    "--use-ascii",
    "--ca-native",
    "--cert-status",
    "--tr-encoding",
    "--raw",
    "--xattr",
];

const REJECTED_EXACT: &[(&str, bool)] = &[
    ("--netrc", false),
    ("--netrc-file", true),
    ("--netrc-optional", false),
    ("--config", true),
    ("--libcurl", true),
    ("--trace", true),
    ("--trace-ascii", true),
    ("--trace-config", true),
    ("--trace-ids", false),
    ("--trace-time", false),
    ("--disable", false),
    ("--disable-eprt", false),
    ("--disable-epsv", false),
    ("--disallow-username-in-url", false),
    ("--stderr", true),
    ("--styled-output", false),
    ("--suppress-connect-headers", false),
    ("--variable", true),
    ("--etag-compare", true),
    ("--etag-save", true),
    ("--time-cond", true),
    ("--continue-at", true),
    ("--url-query", true),
    ("--fail-early", false),
    ("--location-trusted", false),
    ("--remove-on-error", false),
    ("--no-clobber", false),
    ("--no-buffer", false),
    ("--no-progress-meter", false),
    ("--progress-bar", false),
    ("--request-target", true),
    ("--globoff", false),
    ("--ipv4", false),
    ("--ipv6", false),
    ("--ignore-content-length", false),
    ("--junk-session-cookies", false),
    ("--cookie-jar", true),
    ("--list-only", false),
    ("--manual", false),
    ("--quote", true),
    ("--telnet-option", true),
    ("--append", false),
    ("--limit-rate", true),
    ("--rate", true),
    ("--mail-auth", true),
    ("--max-filesize-xxx", true), // sentinel; harmless
];

// ── Body construction ───────────────────────────────────────────

fn build_body(
    ctx: &mut UtilContext<'_>,
    opts: &CurlOpts,
) -> Result<(Option<Vec<u8>>, Vec<(String, String)>), String> {
    if !opts.form_parts.is_empty() {
        if !opts.data_pieces.is_empty() {
            return Err("cannot combine --data/--json with --form".into());
        }
        let (body, ct) = encode_multipart(ctx, &opts.form_parts)?;
        return Ok((Some(body), vec![("Content-Type".into(), ct)]));
    }
    if opts.data_pieces.is_empty() {
        return Ok((None, Vec::new()));
    }

    let any_json = opts.data_pieces.iter().any(is_json_piece);
    let mut body = Vec::new();
    for piece in &opts.data_pieces {
        let bytes = materialize_piece(ctx, piece)?;
        if !body.is_empty() && !any_json {
            body.push(b'&');
        }
        body.extend_from_slice(&bytes);
    }

    let headers = if any_json {
        vec![
            ("Content-Type".into(), "application/json".into()),
            ("Accept".into(), "application/json".into()),
        ]
    } else {
        vec![(
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        )]
    };
    Ok((Some(body), headers))
}

fn materialize_piece(ctx: &mut UtilContext<'_>, piece: &DataPiece) -> Result<Vec<u8>, String> {
    match piece {
        DataPiece::Ascii(b) => Ok(strip_crlf(b)),
        DataPiece::AsciiFile(p) => read_data_file(ctx, p).map(|b| strip_crlf(&b)),
        DataPiece::Binary(b) | DataPiece::Raw(b) | DataPiece::Json(b) => Ok(b.clone()),
        DataPiece::BinaryFile(p) | DataPiece::JsonFile(p) => read_data_file(ctx, p),
        DataPiece::UrlEncode(spec) => urlencode_spec(ctx, spec),
    }
}

fn read_data_file(ctx: &mut UtilContext<'_>, path: &str) -> Result<Vec<u8>, String> {
    let resolved = resolve_path(ctx.cwd, path);
    let h = ctx
        .fs
        .open(&resolved, OpenOptions::read())
        .map_err(|e| format!("cannot read '{path}': {e}"))?;
    let data = ctx.fs.read_file(h).map_err(|e| e.to_string());
    ctx.fs.close(h);
    data
}

/// Implements the five `--data-urlencode` input shapes documented by curl.
fn urlencode_spec(ctx: &mut UtilContext<'_>, spec: &str) -> Result<Vec<u8>, String> {
    let (name, value_bytes) = parse_urlencode_spec(ctx, spec)?;
    let encoded = percent_encode(&value_bytes);
    Ok(match name {
        Some(n) => format!("{n}={encoded}").into_bytes(),
        None => encoded.into_bytes(),
    })
}

fn parse_urlencode_spec(
    ctx: &mut UtilContext<'_>,
    spec: &str,
) -> Result<(Option<String>, Vec<u8>), String> {
    // Order matches curl(1) `--data-urlencode`:
    //   content           → no name, encode literal
    //   =content          → no name, encode literal after '='
    //   name=content      → name + encode literal
    //   @file             → no name, encode file
    //   name@file         → name + encode file
    if let Some(path) = spec.strip_prefix('@') {
        let bytes = read_data_file(ctx, path)?;
        return Ok((None, bytes));
    }
    if let Some(rest) = spec.strip_prefix('=') {
        return Ok((None, rest.as_bytes().to_vec()));
    }
    if let Some((name, rest)) = spec.split_once('=') {
        return Ok((Some(name.to_string()), rest.as_bytes().to_vec()));
    }
    if let Some((name, path)) = spec.split_once('@') {
        let bytes = read_data_file(ctx, path)?;
        return Ok((Some(name.to_string()), bytes));
    }
    Ok((None, spec.as_bytes().to_vec()))
}

fn percent_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => {
                // Writing to a String is infallible.
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

// ── HttpRequest construction ────────────────────────────────────

fn build_curl_request(
    ctx: &mut UtilContext<'_>,
    opts: &CurlOpts,
    url: &str,
) -> Result<HttpRequest, String> {
    let (mut body, body_headers) = build_body(ctx, opts)?;
    let mut body_is_upload = false;

    if let Some(path) = &opts.upload_file {
        if body.is_some() || !opts.form_parts.is_empty() {
            return Err("cannot combine --upload-file with --data/--form".into());
        }
        body = Some(read_data_file(ctx, path)?);
        body_is_upload = true;
    }

    let (final_url, body_after_get) = apply_get_via_url(opts, url, body)?;
    let body = body_after_get;
    let body_headers = if body.is_none() {
        Vec::new()
    } else {
        body_headers
    };

    let method = infer_method(opts, body.is_some(), body_is_upload);
    let mut headers = assemble_headers(ctx, opts, body_headers, &final_url)?;

    if opts.aws_sigv4.is_some() {
        apply_aws_sigv4(
            ctx,
            opts,
            &method,
            &final_url,
            body.as_deref(),
            &mut headers,
        )?;
    }

    Ok(HttpRequest {
        url: final_url,
        method,
        headers,
        body,
        follow_redirects: opts.follow_redirects,
        timeout_ms: opts.max_time_ms,
        connect_timeout_ms: opts.connect_timeout_ms,
        max_redirs: opts.max_redirs,
        max_response_bytes: opts.max_filesize,
    })
}

/// `-G`/`--get`: move the body (already encoded via `build_body`) into the URL
/// query string and drop the request body.  Works for `-d`, `--data-urlencode`,
/// etc.; the encoding has already happened in `build_body`.
fn apply_get_via_url(
    opts: &CurlOpts,
    url: &str,
    body: Option<Vec<u8>>,
) -> Result<(String, Option<Vec<u8>>), String> {
    if !opts.get_via_url {
        return Ok((url.to_string(), body));
    }
    let Some(bytes) = body else {
        return Ok((url.to_string(), None));
    };
    let query = String::from_utf8(bytes).map_err(|_| "non-UTF8 body with -G".to_string())?;
    let joined = append_query(url, &query);
    Ok((joined, None))
}

fn append_query(url: &str, query: &str) -> String {
    if query.is_empty() {
        return url.to_string();
    }
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}{query}")
}

fn infer_method(opts: &CurlOpts, has_body: bool, is_upload: bool) -> String {
    if let Some(m) = &opts.method {
        return m.clone();
    }
    if opts.head_only {
        return "HEAD".into();
    }
    if is_upload {
        return "PUT".into();
    }
    if opts.get_via_url {
        return "GET".into();
    }
    if has_body {
        return "POST".into();
    }
    "GET".into()
}

fn assemble_headers(
    ctx: &mut UtilContext<'_>,
    opts: &CurlOpts,
    body_headers: Vec<(String, String)>,
    url: &str,
) -> Result<Vec<(String, String)>, String> {
    let mut headers: Vec<(String, String)> = opts.headers.clone();
    for (k, v) in body_headers {
        if !headers.iter().any(|(hk, _)| hk.eq_ignore_ascii_case(&k)) {
            headers.push((k, v));
        }
    }
    let auth = resolve_authorization(ctx, opts, url);
    add_derived_header(&mut headers, "Authorization", auth);
    add_derived_header(&mut headers, "User-Agent", opts.user_agent.clone());
    add_derived_header(&mut headers, "Referer", opts.referer.clone());
    add_derived_header(
        &mut headers,
        "Range",
        opts.range.as_ref().map(|r| format!("bytes={r}")),
    );
    add_derived_header(
        &mut headers,
        "If-Modified-Since",
        opts.time_cond.as_deref().and_then(parse_time_cond_header),
    );
    if opts.compressed {
        add_derived_header(
            &mut headers,
            "Accept-Encoding",
            Some("gzip, deflate".to_string()),
        );
    }
    if let Some(cookie) = &opts.cookie {
        let value = resolve_cookie_value(ctx, cookie)?;
        add_derived_header(&mut headers, "Cookie", Some(value));
    }
    Ok(headers)
}

/// Precedence: explicit `-u` beats `--oauth2-bearer` beats netrc lookup.
fn resolve_authorization(ctx: &mut UtilContext<'_>, opts: &CurlOpts, url: &str) -> Option<String> {
    if let Some(user_pass) = &opts.user {
        return Some(basic_auth(user_pass));
    }
    if let Some(token) = &opts.oauth2_bearer {
        return Some(format!("Bearer {token}"));
    }
    if opts.netrc_enabled || opts.netrc_file.is_some() {
        if let Some(user_pass) = lookup_netrc(ctx, opts, url) {
            return Some(basic_auth(&user_pass));
        }
    }
    None
}

/// Map `--time-cond VALUE` to an `If-Modified-Since` value.
///
/// Accepts a verbatim HTTP-date (`"Wed, 21 Oct 2015 07:28:00 GMT"`) or the
/// same with a leading `-` (which in real curl flips the header to
/// `If-Unmodified-Since`; we pass the stripped value through and let the
/// server apply the less-strict form).  We deliberately do not accept
/// `@file` — file mtimes are not exposed to the utilities layer.
fn parse_time_cond_header(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let stripped = trimmed.strip_prefix('-').unwrap_or(trimmed).trim();
    if stripped.is_empty() {
        return None;
    }
    Some(stripped.to_string())
}

fn lookup_netrc(ctx: &mut UtilContext<'_>, opts: &CurlOpts, url: &str) -> Option<String> {
    let Ok(parsed) = url::Url::parse(url) else {
        return None;
    };
    let host = parsed.host_str()?.to_ascii_lowercase();
    let path = netrc_path(ctx, opts);
    let bytes = read_data_file(ctx, &path).ok()?;
    find_netrc_credentials(&String::from_utf8_lossy(&bytes), &host)
}

fn netrc_path(ctx: &UtilContext<'_>, opts: &CurlOpts) -> String {
    if let Some(path) = &opts.netrc_file {
        return path.clone();
    }
    let home = ctx
        .state
        .and_then(|s| s.get_var("HOME"))
        .map(|v| v.to_string())
        .unwrap_or_default();
    if home.is_empty() {
        return "/.netrc".into();
    }
    format!("{home}/.netrc")
}

fn find_netrc_credentials(text: &str, host: &str) -> Option<String> {
    let tokens: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .flat_map(str::split_whitespace)
        .collect();
    match_netrc_entry(&tokens, host, true).or_else(|| match_netrc_entry(&tokens, host, false))
}

fn match_netrc_entry(tokens: &[&str], host: &str, exact: bool) -> Option<String> {
    let mut i = 0;
    while i < tokens.len() {
        let is_match = if exact {
            tokens[i] == "machine"
                && i + 1 < tokens.len()
                && tokens[i + 1].eq_ignore_ascii_case(host)
        } else {
            tokens[i] == "default"
        };
        if is_match {
            let skip = if exact { 2 } else { 1 };
            return extract_login_password(&tokens[i + skip..]);
        }
        i += 1;
    }
    None
}

fn extract_login_password(tokens: &[&str]) -> Option<String> {
    let mut login: Option<&str> = None;
    let mut password: Option<&str> = None;
    let mut i = 0;
    while i + 1 < tokens.len() {
        match tokens[i] {
            "machine" | "default" => break,
            "login" => login = Some(tokens[i + 1]),
            "password" => password = Some(tokens[i + 1]),
            _ => {}
        }
        i += 2;
    }
    let user = login?;
    let pass = password.unwrap_or("");
    Some(format!("{user}:{pass}"))
}

/// `-b` takes either a literal `key=val; key2=val2` string or an `@file`
/// / filename path.  curl(1) treats any argument without `=` as a file; we
/// also accept explicit `@file` for symmetry with `-d`.
fn resolve_cookie_value(ctx: &mut UtilContext<'_>, arg: &str) -> Result<String, String> {
    if let Some(path) = arg.strip_prefix('@') {
        return cookie_from_file(ctx, path);
    }
    if arg.contains('=') {
        return Ok(arg.to_string());
    }
    cookie_from_file(ctx, arg)
}

fn cookie_from_file(ctx: &mut UtilContext<'_>, path: &str) -> Result<String, String> {
    let bytes = read_data_file(ctx, path)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut pairs: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        pairs.push(line.to_string());
    }
    Ok(pairs.join("; "))
}

fn add_derived_header(headers: &mut Vec<(String, String)>, name: &str, value: Option<String>) {
    let Some(v) = value else { return };
    if headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name)) {
        return;
    }
    headers.push((name.to_string(), v));
}

fn basic_auth(user_pass: &str) -> String {
    // Real curl prompts for the password when missing; we treat "user" as
    // "user:" (empty password) rather than prompting.
    let creds = if user_pass.contains(':') {
        user_pass.to_string()
    } else {
        format!("{user_pass}:")
    };
    format!("Basic {}", B64_STANDARD.encode(creds.as_bytes()))
}

// ── AWS SigV4 ───────────────────────────────────────────────────

/// Sign the request using AWS Signature V4 and swap any Basic
/// `Authorization` header for the signed one.
///
/// Credentials come from `-u ACCESS_KEY:SECRET_KEY`.  Region and service
/// come from the `--aws-sigv4` spec (`"<prv1>[:<prv2>[:<region>[:<svc>]]]"`);
/// both default to `us-east-1`/`s3` in line with curl's heuristics.  The
/// request time is read from the `WASMSH_DATE` shell variable (the same
/// source `util_date` uses) so tests are deterministic.
fn apply_aws_sigv4(
    ctx: &UtilContext<'_>,
    opts: &CurlOpts,
    method: &str,
    url: &str,
    body: Option<&[u8]>,
    headers: &mut Vec<(String, String)>,
) -> Result<(), String> {
    let spec = opts
        .aws_sigv4
        .as_ref()
        .ok_or_else(|| "--aws-sigv4 was not provided".to_string())?;
    let (region, service) = parse_sigv4_spec(spec);
    let (access_key, secret_key) = parse_sigv4_credentials(opts)?;
    let (date_stamp, amz_date) = sigv4_timestamps(ctx)?;

    let parsed = url::Url::parse(url).map_err(|e| format!("--aws-sigv4: invalid URL: {e}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "--aws-sigv4: URL has no host".to_string())?
        .to_string();

    // Drop any previously-set Authorization (e.g. Basic from -u) — the
    // SigV4 header replaces it.  Also drop x-amz-date so we set a fresh one.
    headers.retain(|(k, _)| {
        !k.eq_ignore_ascii_case("authorization") && !k.eq_ignore_ascii_case("x-amz-date")
    });
    headers.push(("x-amz-date".into(), amz_date.clone()));
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        headers.push(("host".into(), host));
    }

    let payload_hash = sha256_hex(body.unwrap_or(&[]));
    let (canonical_headers, signed_headers) = canonicalize_headers(headers);
    let canonical_path = canonical_uri_path(&parsed);
    let canonical_query = canonical_query_string(&parsed);
    let canonical_request = format!(
        "{method}\n{canonical_path}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );

    let credential_scope = format!("{date_stamp}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let signing_key = sigv4_signing_key(&secret_key, &date_stamp, &region, &service);
    let signature = hex_encode(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));

    let auth = format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}"
    );
    headers.push(("Authorization".into(), auth));
    Ok(())
}

fn parse_sigv4_spec(spec: &str) -> (String, String) {
    let parts: Vec<&str> = spec.split(':').collect();
    let region = parts.get(2).copied().unwrap_or("us-east-1").to_string();
    let service = parts.get(3).copied().unwrap_or("s3").to_string();
    (region, service)
}

fn parse_sigv4_credentials(opts: &CurlOpts) -> Result<(String, String), String> {
    let user_pass = opts
        .user
        .as_ref()
        .ok_or_else(|| "--aws-sigv4 requires -u ACCESS_KEY:SECRET_KEY".to_string())?;
    let (access, secret) = user_pass
        .split_once(':')
        .ok_or_else(|| "--aws-sigv4: -u must be ACCESS_KEY:SECRET_KEY".to_string())?;
    Ok((access.to_string(), secret.to_string()))
}

fn sigv4_timestamps(ctx: &UtilContext<'_>) -> Result<(String, String), String> {
    let raw = ctx
        .state
        .and_then(|s| s.get_var("WASMSH_DATE"))
        .map_or_else(|| "2026-01-01 00:00:00 UTC".to_string(), |v| v.to_string());
    // Accept "YYYY-MM-DD HH:MM:SS [TZ]" — the format util_date also uses.
    let (date, time_rest) = raw
        .split_once(' ')
        .ok_or_else(|| format!("WASMSH_DATE: unparseable date '{raw}'"))?;
    let time = time_rest.split_whitespace().next().unwrap_or("00:00:00");
    let date_stamp = date.replace('-', "");
    let time_stamp = time.replace(':', "");
    if date_stamp.len() != 8 || time_stamp.len() != 6 {
        return Err(format!("WASMSH_DATE: invalid date format '{raw}'"));
    }
    Ok((date_stamp.clone(), format!("{date_stamp}T{time_stamp}Z")))
}

/// Canonicalize the path portion of the URL for `SigV4`.  AWS requires a
/// non-empty `URI` (defaulting to `/`), and any already-encoded sequences
/// are preserved verbatim.
fn canonical_uri_path(url: &url::Url) -> String {
    let path = url.path();
    if path.is_empty() {
        "/".into()
    } else {
        path.to_string()
    }
}

/// Canonical query string: sort by (name, value), percent-encode both.
fn canonical_query_string(url: &url::Url) -> String {
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                percent_encode(k.as_bytes()),
                percent_encode(v.as_bytes())
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Produce the `(canonical_headers, signed_headers)` pair required by
/// `SigV4`.  All provided headers are signed; their names are lowercased
/// and values trimmed.
fn canonicalize_headers(headers: &[(String, String)]) -> (String, String) {
    let mut lowered: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    lowered.sort_by(|a, b| a.0.cmp(&b.0));
    let mut canonical = String::new();
    for (k, v) in &lowered {
        use std::fmt::Write as _;
        let _ = writeln!(canonical, "{k}:{v}");
    }
    // `writeln!` emits `\n`; SigV4 canonical-headers spec wants `\n` too.
    let signed = lowered
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    (canonical, signed)
}

fn sigv4_signing_key(secret: &str, date_stamp: &str, region: &str, service: &str) -> [u8; 32] {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex_encode(&Sha256::digest(data))
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut normalized = [0u8; 64];
    if key.len() > 64 {
        normalized[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= normalized[i];
        opad[i] ^= normalized[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    let out = outer.finalize();
    let mut result = [0u8; 32];
    result.copy_from_slice(&out);
    result
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

// ── Verbose / status helpers ────────────────────────────────────

fn reason_phrase(code: u16) -> &'static str {
    match code {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        206 => "Partial Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        418 => "I'm a teapot",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

fn format_status_line(status: u16) -> String {
    let phrase = reason_phrase(status);
    if phrase.is_empty() {
        format!("HTTP/1.1 {status}")
    } else {
        format!("HTTP/1.1 {status} {phrase}")
    }
}

fn curl_log_request(ctx: &mut UtilContext<'_>, request: &HttpRequest) {
    ctx.output
        .stderr(format!("* Trying {} in sandbox...\n", request.url).as_bytes());
    ctx.output
        .stderr(format!("> {} {}\n", request.method, request.url).as_bytes());
    for (k, v) in &request.headers {
        ctx.output.stderr(format!("> {k}: {v}\n").as_bytes());
    }
    ctx.output.stderr(b">\n");
}

fn curl_log_response(ctx: &mut UtilContext<'_>, response: &HttpResponse) {
    ctx.output
        .stderr(format!("< {}\n", format_status_line(response.status)).as_bytes());
    for (k, v) in &response.headers {
        ctx.output.stderr(format!("< {k}: {v}\n").as_bytes());
    }
    ctx.output.stderr(b"<\n");
}

fn curl_handle_fetch_error(ctx: &mut UtilContext<'_>, e: &NetworkError, opts: &CurlOpts) -> i32 {
    if !opts.silent || opts.show_error {
        ctx.output.stderr(format!("curl: {e}\n").as_bytes());
    }
    match e {
        NetworkError::HostDenied(_) => 6,
        NetworkError::ConnectionFailed(_) => 7,
        NetworkError::Timeout(_) => 28,
        NetworkError::ResponseTooLarge(_) => 63,
        NetworkError::TooManyRedirects(_) => 47,
        _ => 1,
    }
}

// ── Retry loop ──────────────────────────────────────────────────

fn is_retryable_error(e: &NetworkError, opts: &CurlOpts) -> bool {
    if matches!(e, NetworkError::HostDenied(_) | NetworkError::InvalidUrl(_)) {
        return false;
    }
    if opts.retry_all_errors {
        return true;
    }
    match e {
        NetworkError::Timeout(_) => true,
        NetworkError::ConnectionFailed(msg) => {
            !msg.to_ascii_lowercase().contains("refused") || opts.retry_connrefused
        }
        _ => false,
    }
}

fn is_retryable_status(status: u16, opts: &CurlOpts) -> bool {
    if opts.retry_all_errors && status >= 400 {
        return true;
    }
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

fn fetch_with_retries(
    backend: &dyn NetworkBackend,
    request: &HttpRequest,
    opts: &CurlOpts,
) -> Result<HttpResponse, NetworkError> {
    let mut attempt: u32 = 0;
    loop {
        let result = backend.fetch(request);
        if opts.retry == 0 || attempt >= opts.retry {
            return result;
        }
        match result {
            Ok(resp) if is_retryable_status(resp.status, opts) => attempt += 1,
            Err(ref e) if is_retryable_error(e, opts) => attempt += 1,
            other => return other,
        }
    }
}

// ── Response writing ────────────────────────────────────────────

fn enforce_response_size(opts: &CurlOpts, response: &HttpResponse) -> Option<i32> {
    let cap = opts.max_filesize?;
    let body_len = response.body.len() as u64;
    if body_len <= cap {
        return None;
    }
    Some(63)
}

fn write_curl_response(
    ctx: &mut UtilContext<'_>,
    opts: &CurlOpts,
    response: &HttpResponse,
    request_url: &str,
) -> i32 {
    if let Some(code) = write_dump_header(ctx, opts, response) {
        return code;
    }
    if opts.head_only {
        return write_head_output(ctx, response);
    }
    if opts.include_headers {
        emit_headers_to_stdout(ctx, response);
    }
    write_body(ctx, opts, response, request_url)
}

fn write_head_output(ctx: &mut UtilContext<'_>, response: &HttpResponse) -> i32 {
    emit_headers_to_stdout(ctx, response);
    0
}

fn emit_headers_to_stdout(ctx: &mut UtilContext<'_>, response: &HttpResponse) {
    ctx.output
        .stdout(format!("{}\r\n", format_status_line(response.status)).as_bytes());
    for (k, v) in &response.headers {
        ctx.output.stdout(format!("{k}: {v}\r\n").as_bytes());
    }
    ctx.output.stdout(b"\r\n");
}

fn write_dump_header(
    ctx: &mut UtilContext<'_>,
    opts: &CurlOpts,
    response: &HttpResponse,
) -> Option<i32> {
    let target = opts.dump_header.as_deref()?;
    let mut buf = Vec::new();
    buf.extend_from_slice(format_status_line(response.status).as_bytes());
    buf.extend_from_slice(b"\r\n");
    for (k, v) in &response.headers {
        buf.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    buf.extend_from_slice(b"\r\n");

    if target == "-" {
        ctx.output.stdout(&buf);
        return None;
    }
    let path = resolve_output_path(ctx, opts, target);
    if let Some(code) = ensure_output_parents(ctx, opts, &path, "curl") {
        return Some(code);
    }
    if let Err(e) = write_bytes(ctx, &path, &buf) {
        ctx.output
            .stderr(format!("curl: cannot write header dump '{target}': {e}\n").as_bytes());
        return Some(23);
    }
    None
}

fn write_body(
    ctx: &mut UtilContext<'_>,
    opts: &CurlOpts,
    response: &HttpResponse,
    request_url: &str,
) -> i32 {
    let output_file = choose_output_filename(opts, response, request_url);
    let Some(output_file) = output_file else {
        ctx.output.stdout(&response.body);
        return 0;
    };
    let path = resolve_output_path(ctx, opts, &output_file);
    if let Some(code) = ensure_output_parents(ctx, opts, &path, "curl") {
        return code;
    }
    match write_bytes(ctx, &path, &response.body) {
        Ok(()) => 0,
        Err(e) => {
            ctx.output
                .stderr(format!("curl: cannot write to '{output_file}': {e}\n").as_bytes());
            23
        }
    }
}

fn choose_output_filename(
    opts: &CurlOpts,
    response: &HttpResponse,
    request_url: &str,
) -> Option<String> {
    if let Some(name) = &opts.output {
        return Some(name.clone());
    }
    if opts.remote_name {
        if opts.remote_header_name {
            if let Some(name) = content_disposition_filename(response) {
                return Some(name);
            }
        }
        return Some(remote_filename_from_url(request_url));
    }
    None
}

fn remote_filename_from_url(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.path()
                .rsplit('/')
                .find(|seg| !seg.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "curl_output".into())
}

fn content_disposition_filename(response: &HttpResponse) -> Option<String> {
    let (_, raw) = response
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-disposition"))?;
    for part in raw.split(';') {
        let p = part.trim();
        if let Some(v) = p.strip_prefix("filename=") {
            return Some(sanitize_filename(v.trim().trim_matches('"')));
        }
    }
    None
}

fn sanitize_filename(name: &str) -> String {
    name.rsplit(['/', '\\']).next().unwrap_or(name).to_string()
}

fn resolve_output_path(ctx: &UtilContext<'_>, opts: &CurlOpts, filename: &str) -> String {
    if filename.starts_with('/') {
        return wasmsh_fs::normalize_path(filename);
    }
    if let Some(dir) = &opts.output_dir {
        let base = resolve_path(ctx.cwd, dir);
        return wasmsh_fs::normalize_path(&format!("{base}/{filename}"));
    }
    resolve_path(ctx.cwd, filename)
}

fn ensure_output_parents(
    ctx: &mut UtilContext<'_>,
    opts: &CurlOpts,
    path: &str,
    cmd: &str,
) -> Option<i32> {
    if !opts.create_dirs {
        return None;
    }
    let parent = path.rsplit_once('/').map_or("", |(p, _)| p);
    if parent.is_empty() {
        return None;
    }
    match mkdirs(ctx, parent) {
        Ok(()) => None,
        Err(e) => {
            ctx.output
                .stderr(format!("{cmd}: cannot create directories for '{path}': {e}\n").as_bytes());
            Some(23)
        }
    }
}

fn mkdirs(ctx: &mut UtilContext<'_>, path: &str) -> Result<(), String> {
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut cur = String::new();
    for p in parts {
        cur.push('/');
        cur.push_str(p);
        if ctx.fs.stat(&cur).is_ok() {
            continue;
        }
        ctx.fs.create_dir(&cur).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn write_bytes(ctx: &mut UtilContext<'_>, path: &str, data: &[u8]) -> Result<(), String> {
    let h = ctx
        .fs
        .open(path, OpenOptions::write())
        .map_err(|e| e.to_string())?;
    let res = ctx.fs.write_file(h, data).map_err(|e| e.to_string());
    ctx.fs.close(h);
    res
}

// ── `-w`/--write-out ────────────────────────────────────────────

struct WriteOutCtx<'a> {
    url: &'a str,
    method: &'a str,
    url_index: u32,
    response: &'a HttpResponse,
    error: Option<&'a NetworkError>,
}

fn format_write_out(fmt: &str, wo: &WriteOutCtx<'_>) -> String {
    let mut out = String::with_capacity(fmt.len());
    let mut it = fmt.chars().peekable();
    while let Some(ch) = it.next() {
        match ch {
            '\\' => handle_escape(&mut it, &mut out),
            '%' if it.peek() == Some(&'{') => handle_token(&mut it, &mut out, wo),
            other => out.push(other),
        }
    }
    out
}

fn handle_escape(it: &mut std::iter::Peekable<std::str::Chars<'_>>, out: &mut String) {
    match it.next() {
        Some('n') => out.push('\n'),
        Some('t') => out.push('\t'),
        Some('r') => out.push('\r'),
        Some('\\') | None => out.push('\\'),
        Some(other) => {
            out.push('\\');
            out.push(other);
        }
    }
}

fn handle_token(
    it: &mut std::iter::Peekable<std::str::Chars<'_>>,
    out: &mut String,
    wo: &WriteOutCtx<'_>,
) {
    it.next(); // consume '{'
    let mut name = String::new();
    // Track brace depth so nested forms like `%{header{x-name}}` parse
    // as a single token (name = "header{x-name}").
    let mut depth: i32 = 1;
    for ch in it.by_ref() {
        match ch {
            '{' => {
                depth += 1;
                name.push(ch);
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                name.push(ch);
            }
            _ => name.push(ch),
        }
    }
    out.push_str(&resolve_write_out_token(&name, wo));
}

fn resolve_write_out_token(name: &str, wo: &WriteOutCtx<'_>) -> String {
    if let Some(header_name) = name
        .strip_prefix("header{")
        .and_then(|s| s.strip_suffix('}'))
    {
        return find_header(wo.response, header_name).unwrap_or_default();
    }
    if name == "json" {
        return write_out_json(wo);
    }
    if name == "header_json" {
        return write_out_header_json(wo);
    }
    match name {
        "http_code" | "response_code" => wo.response.status.to_string(),
        "url" | "url_effective" => wo.url.to_string(),
        "method" => wo.method.to_string(),
        "scheme" => url_scheme(wo.url).to_string(),
        "urlnum" => wo.url_index.to_string(),
        "num_headers" => wo.response.headers.len().to_string(),
        "size_download" => wo.response.body.len().to_string(),
        "size_header" => estimate_header_size(wo.response).to_string(),
        "content_type" => find_header(wo.response, "content-type").unwrap_or_default(),
        "http_version" => "1.1".to_string(),
        "errormsg" => wo.error.map(ToString::to_string).unwrap_or_default(),
        "num_redirects" | "time_total" | "time_connect" | "time_starttransfer"
        | "time_namelookup" | "time_pretransfer" | "time_appconnect" | "time_redirect"
        | "speed_download" | "speed_upload" => "0".to_string(),
        _ => format!("%{{{name}}}"),
    }
}

fn url_scheme(url: &str) -> &str {
    url.split_once("://").map_or("", |(s, _)| s)
}

/// Emit a minimal JSON blob covering the well-defined `-w` tokens.
fn write_out_json(wo: &WriteOutCtx<'_>) -> String {
    let ct = find_header(wo.response, "content-type").unwrap_or_default();
    format!(
        "{{\"http_code\":{},\"method\":\"{}\",\"scheme\":\"{}\",\"url_effective\":\"{}\",\"urlnum\":{},\"size_download\":{},\"content_type\":\"{}\",\"num_headers\":{}}}",
        wo.response.status,
        json_escape(wo.method),
        json_escape(url_scheme(wo.url)),
        json_escape(wo.url),
        wo.url_index,
        wo.response.body.len(),
        json_escape(&ct),
        wo.response.headers.len(),
    )
}

fn write_out_header_json(wo: &WriteOutCtx<'_>) -> String {
    let mut out = String::from("{");
    for (i, (k, v)) in wo.response.headers.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&json_escape(k));
        out.push_str("\":[\"");
        out.push_str(&json_escape(v));
        out.push_str("\"]");
    }
    out.push('}');
    out
}

fn json_escape(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

fn estimate_header_size(response: &HttpResponse) -> usize {
    let status_line = format_status_line(response.status).len() + 2;
    let headers: usize = response
        .headers
        .iter()
        .map(|(k, v)| k.len() + v.len() + 4)
        .sum();
    status_line + headers + 2
}

fn find_header(response: &HttpResponse, name: &str) -> Option<String> {
    response
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

// ── Entry points ────────────────────────────────────────────────

pub(crate) fn util_curl(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let segments = match parse_curl_args(ctx, argv) {
        Ok(s) => s,
        Err(e) => {
            ctx.output.stderr(format!("curl: {e}\n").as_bytes());
            return 2;
        }
    };

    if segments.iter().all(|s| s.urls.is_empty()) {
        ctx.output.stderr(b"curl: no URL specified\n");
        return 2;
    }

    if ctx.network.is_none() {
        ctx.output.stderr(b"curl: network access not available\n");
        return 1;
    }

    let mut last_nonzero: i32 = 0;
    let mut url_index: u32 = 0;
    for opts in &segments {
        for url in &opts.urls {
            let status = run_single_url(ctx, opts, url, url_index);
            url_index += 1;
            if status != 0 {
                last_nonzero = status;
            }
        }
    }
    last_nonzero
}

/// Execute one URL within a `--next`-delimited segment.  Every URL in the
/// same segment shares the same flags, except for `-O`/`--remote-name-all`
/// which derive the output filename per-URL.
fn run_single_url(
    ctx: &mut UtilContext<'_>,
    base_opts: &CurlOpts,
    url: &str,
    url_index: u32,
) -> i32 {
    let opts = effective_opts_for_url(base_opts);

    let request = match build_curl_request(ctx, &opts, url) {
        Ok(r) => r,
        Err(e) => {
            ctx.output.stderr(format!("curl: {e}\n").as_bytes());
            return 2;
        }
    };

    if opts.verbose {
        curl_log_request(ctx, &request);
    }

    let backend = ctx
        .network
        .expect("network capability is checked before dispatching per-URL");
    let mut response = match fetch_with_retries(backend, &request, &opts) {
        Ok(r) => r,
        Err(e) => {
            let code = curl_handle_fetch_error(ctx, &e, &opts);
            emit_write_out_for(
                ctx,
                &opts,
                url,
                url_index,
                &HttpResponse::default(),
                Some(&e),
            );
            return code;
        }
    };

    if opts.compressed {
        if let Err(e) = maybe_decompress(&mut response) {
            if !opts.silent || opts.show_error {
                ctx.output
                    .stderr(format!("curl: decompression failed: {e}\n").as_bytes());
            }
            return 61;
        }
    }

    if opts.verbose {
        curl_log_response(ctx, &response);
    }

    if let Some(code) = enforce_response_size(&opts, &response) {
        if !opts.silent || opts.show_error {
            ctx.output.stderr(b"curl: Maximum file size exceeded\n");
        }
        emit_write_out_for(ctx, &opts, url, url_index, &response, None);
        return code;
    }

    if let Some(code) = fail_response_code(&opts, &response) {
        if opts.fail_with_body {
            let _ = write_curl_response(ctx, &opts, &response, url);
        }
        emit_fail_error(ctx, &opts, response.status);
        emit_write_out_for(ctx, &opts, url, url_index, &response, None);
        return code;
    }

    let write_status = write_curl_response(ctx, &opts, &response, url);
    if write_status != 0 {
        return write_status;
    }
    emit_write_out_for(ctx, &opts, url, url_index, &response, None);
    0
}

/// Produce the effective per-URL options.  Currently this just promotes
/// `--remote-name-all` into `remote_name=true`; everything else carries
/// across unchanged.  Cloning is unavoidable because downstream helpers
/// take `&CurlOpts` and the caller borrows the segment's base opts
/// immutably while we may need to override fields.
fn effective_opts_for_url(base: &CurlOpts) -> CurlOpts {
    CurlOpts {
        urls: Vec::new(),
        remote_name_all: base.remote_name_all,
        aws_sigv4: base.aws_sigv4.clone(),
        method: base.method.clone(),
        headers: base.headers.clone(),
        data_pieces: base.data_pieces.clone(),
        form_parts: base.form_parts.clone(),
        output: base.output.clone(),
        output_dir: base.output_dir.clone(),
        create_dirs: base.create_dirs,
        remote_name: base.remote_name || base.remote_name_all,
        remote_header_name: base.remote_header_name,
        include_headers: base.include_headers,
        dump_header: base.dump_header.clone(),
        user: base.user.clone(),
        user_agent: base.user_agent.clone(),
        referer: base.referer.clone(),
        silent: base.silent,
        show_error: base.show_error,
        follow_redirects: base.follow_redirects,
        head_only: base.head_only,
        fail_on_error: base.fail_on_error,
        fail_with_body: base.fail_with_body,
        verbose: base.verbose,
        write_out: base.write_out.clone(),
        max_time_ms: base.max_time_ms,
        connect_timeout_ms: base.connect_timeout_ms,
        max_filesize: base.max_filesize,
        max_redirs: base.max_redirs,
        retry: base.retry,
        retry_delay_ms: base.retry_delay_ms,
        retry_max_time_ms: base.retry_max_time_ms,
        retry_connrefused: base.retry_connrefused,
        retry_all_errors: base.retry_all_errors,
        get_via_url: base.get_via_url,
        compressed: base.compressed,
        range: base.range.clone(),
        upload_file: base.upload_file.clone(),
        cookie: base.cookie.clone(),
        oauth2_bearer: base.oauth2_bearer.clone(),
        time_cond: base.time_cond.clone(),
        netrc_file: base.netrc_file.clone(),
        netrc_enabled: base.netrc_enabled,
    }
}

/// When `--fail`/`--fail-with-body` is set and the response is 4xx/5xx,
/// returns the curl exit code (22) to emit after the body is (optionally)
/// written.
fn fail_response_code(opts: &CurlOpts, response: &HttpResponse) -> Option<i32> {
    if response.status < 400 {
        return None;
    }
    if opts.fail_on_error || opts.fail_with_body {
        return Some(22);
    }
    None
}

fn emit_fail_error(ctx: &mut UtilContext<'_>, opts: &CurlOpts, status: u16) {
    if opts.silent && !opts.show_error {
        return;
    }
    ctx.output
        .stderr(format!("curl: (22) The requested URL returned error: {status}\n").as_bytes());
}

/// Decode `Content-Encoding: gzip`/`deflate` response bodies when
/// `--compressed` was requested.  Other encodings (`br`, `zstd`, `identity`)
/// are either unsupported (returned as an error) or passed through.  On
/// successful decoding the encoding-related headers are normalized so that
/// downstream `-i`/`-D`/`-w` output reflects the decoded body.
fn maybe_decompress(response: &mut HttpResponse) -> Result<(), String> {
    let encoding = match find_header_ref(response, "content-encoding") {
        Some(v) => v.trim().to_ascii_lowercase(),
        None => return Ok(()),
    };
    let decoded = match encoding.as_str() {
        "gzip" => miniz_oxide::inflate::decompress_to_vec_with_limit(
            strip_gzip_framing(&response.body)?,
            64 * 1024 * 1024,
        )
        .map_err(|e| format!("gzip inflate failed: {e:?}"))?,
        "deflate" => miniz_oxide::inflate::decompress_to_vec_zlib(&response.body)
            .or_else(|_| miniz_oxide::inflate::decompress_to_vec(&response.body))
            .map_err(|e| format!("deflate inflate failed: {e:?}"))?,
        "identity" | "" => return Ok(()),
        other => return Err(format!("unsupported Content-Encoding: {other}")),
    };
    response.body = decoded;
    let new_len = response.body.len().to_string();
    response
        .headers
        .retain(|(k, _)| !k.eq_ignore_ascii_case("content-encoding"));
    for (k, v) in &mut response.headers {
        if k.eq_ignore_ascii_case("content-length") {
            v.clone_from(&new_len);
        }
    }
    Ok(())
}

fn find_header_ref<'a>(response: &'a HttpResponse, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Skip the gzip header (RFC 1952) and trailing CRC32/ISIZE, returning the
/// raw DEFLATE payload suitable for `miniz_oxide::inflate::decompress_to_vec`.
fn strip_gzip_framing(data: &[u8]) -> Result<&[u8], String> {
    if data.len() < 10 || data[0] != 0x1f || data[1] != 0x8b {
        return Err("invalid gzip header".into());
    }
    let flags = data[3];
    let mut pos = 10usize;
    if flags & 0x04 != 0 {
        // FEXTRA: 2-byte length, N bytes.
        if data.len() < pos + 2 {
            return Err("truncated gzip extra".into());
        }
        let xlen = usize::from(u16::from_le_bytes([data[pos], data[pos + 1]]));
        pos += 2 + xlen;
    }
    if flags & 0x08 != 0 {
        pos = skip_cstr(data, pos)?;
    }
    if flags & 0x10 != 0 {
        pos = skip_cstr(data, pos)?;
    }
    if flags & 0x02 != 0 {
        pos += 2;
    }
    if data.len() < pos + 8 {
        return Err("truncated gzip stream".into());
    }
    Ok(&data[pos..data.len() - 8])
}

fn skip_cstr(data: &[u8], mut pos: usize) -> Result<usize, String> {
    while pos < data.len() && data[pos] != 0 {
        pos += 1;
    }
    if pos >= data.len() {
        return Err("unterminated string in gzip header".into());
    }
    Ok(pos + 1)
}

fn emit_write_out_for(
    ctx: &mut UtilContext<'_>,
    opts: &CurlOpts,
    url: &str,
    url_index: u32,
    response: &HttpResponse,
    error: Option<&NetworkError>,
) {
    let Some(fmt) = &opts.write_out else { return };
    let default_method = if opts.head_only { "HEAD" } else { "GET" };
    let method = opts.method.as_deref().unwrap_or(default_method);
    let wo = WriteOutCtx {
        url,
        method,
        url_index,
        response,
        error,
    };
    let out = format_write_out(fmt, &wo);
    ctx.output.stdout(out.as_bytes());
}

// ── wget ────────────────────────────────────────────────────────

#[derive(Default)]
struct WgetOpts {
    urls: Vec<String>,
    output: Option<String>,
    quiet: bool,
    headers: Vec<(String, String)>,
    user: Option<String>,
    password: Option<String>,
    post_data: Option<String>,
    tries: u32,
    timeout_ms: Option<u64>,
    content_disposition: bool,
}

fn wget_take_value<'a>(
    flag: &str,
    argv: &'a [&'a str],
    i: &mut usize,
    inline_value: Option<&str>,
) -> Result<String, String> {
    if let Some(v) = inline_value {
        return Ok(v.to_string());
    }
    *i += 1;
    argv.get(*i)
        .map(|s| (*s).to_string())
        .ok_or_else(|| format!("{flag} requires an argument"))
}

/// Recognize both `--flag value` and `--flag=value` forms. Returns
/// `Some(inline_value)` when `=` is present.
fn split_long_equals(arg: &str) -> Option<(&str, &str)> {
    arg.split_once('=')
}

fn parse_wget_long(
    opts: &mut WgetOpts,
    arg: &str,
    argv: &[&str],
    i: &mut usize,
) -> Result<bool, String> {
    let (name, inline) = match split_long_equals(arg) {
        Some((n, v)) => (n, Some(v)),
        None => (arg, None),
    };
    match name {
        "--output-document" => {
            opts.output = Some(wget_take_value(name, argv, i, inline)?);
        }
        "--quiet" => opts.quiet = true,
        "--header" => {
            let h = wget_take_value(name, argv, i, inline)?;
            if let Some((k, v)) = h.split_once(':') {
                opts.headers.push((k.trim().into(), v.trim().into()));
            } else {
                return Err(format!("invalid header format: {h}"));
            }
        }
        "--user" => opts.user = Some(wget_take_value(name, argv, i, inline)?),
        "--password" => opts.password = Some(wget_take_value(name, argv, i, inline)?),
        "--post-data" => opts.post_data = Some(wget_take_value(name, argv, i, inline)?),
        "--tries" => {
            let v = wget_take_value(name, argv, i, inline)?;
            opts.tries = v
                .parse()
                .map_err(|_| format!("--tries: invalid number: {v}"))?;
        }
        "--timeout" | "--connect-timeout" | "--read-timeout" | "--dns-timeout" => {
            let v = wget_take_value(name, argv, i, inline)?;
            let secs: f64 = v
                .parse()
                .map_err(|_| format!("{name}: invalid number: {v}"))?;
            opts.timeout_ms = Some((secs * 1000.0) as u64);
        }
        "--content-disposition" => opts.content_disposition = true,
        "--no-check-certificate"
        | "--no-verbose"
        | "--server-response"
        | "--show-progress"
        | "--no-clobber"
        | "--progress=bar" => {
            // Silent no-ops: TLS/progress/reporting knobs irrelevant in sandbox.
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn parse_combined_wget_flags(
    opts: &mut WgetOpts,
    flags: &str,
    argv: &[&str],
    mut i: usize,
) -> Result<usize, String> {
    let mut j = 0;
    while j < flags.len() {
        match flags.as_bytes()[j] {
            b'q' => opts.quiet = true,
            b'O' => {
                let rest = &flags[j + 1..];
                if rest.is_empty() {
                    i += 1;
                    opts.output = Some(
                        argv.get(i)
                            .ok_or("-O requires a filename argument")?
                            .to_string(),
                    );
                } else {
                    opts.output = Some(rest.to_string());
                }
                return Ok(i);
            }
            _ => return Err(format!("unknown option: -{}", flags.as_bytes()[j] as char)),
        }
        j += 1;
    }
    Ok(i)
}

fn parse_wget_args(argv: &[&str]) -> Result<WgetOpts, String> {
    let mut opts = WgetOpts::default();

    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i];
        match arg {
            "-O" => {
                i += 1;
                opts.output = Some(
                    argv.get(i)
                        .ok_or("-O requires a filename argument")?
                        .to_string(),
                );
            }
            _ if arg.starts_with("--") => {
                if !parse_wget_long(&mut opts, arg, argv, &mut i)? {
                    return Err(format!("unknown option: {arg}"));
                }
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                i = parse_combined_wget_flags(&mut opts, &arg[1..], argv, i)?;
            }
            _ => opts.urls.push(arg.to_string()),
        }
        i += 1;
    }

    Ok(opts)
}

fn filename_from_url(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| {
            let path = u.path();
            let name = path.rsplit('/').next().unwrap_or("");
            if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            }
        })
        .unwrap_or_else(|| "index.html".into())
}

fn wget_combined_basic_auth(opts: &WgetOpts) -> Option<String> {
    let user = opts.user.as_deref()?;
    let pw = opts.password.as_deref().unwrap_or("");
    Some(basic_auth(&format!("{user}:{pw}")))
}

fn build_wget_request(opts: &WgetOpts, url: &str) -> HttpRequest {
    let mut headers = opts.headers.clone();
    if let Some(auth) = wget_combined_basic_auth(opts) {
        if !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        {
            headers.push(("Authorization".into(), auth));
        }
    }
    let (method, body) = match opts.post_data.as_deref() {
        Some(data) => {
            if !headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            {
                headers.push((
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                ));
            }
            ("POST".to_string(), Some(data.as_bytes().to_vec()))
        }
        None => ("GET".to_string(), None),
    };
    HttpRequest {
        url: url.to_string(),
        method,
        headers,
        body,
        follow_redirects: true,
        timeout_ms: opts.timeout_ms,
        connect_timeout_ms: opts.timeout_ms,
        ..HttpRequest::default()
    }
}

fn wget_output_filename(opts: &WgetOpts, url: &str, response: &HttpResponse) -> String {
    if opts.content_disposition {
        if let Some(name) = content_disposition_filename(response) {
            return name;
        }
    }
    opts.output
        .clone()
        .unwrap_or_else(|| filename_from_url(url))
}

fn wget_fetch_one(
    opts: &WgetOpts,
    backend: &dyn NetworkBackend,
    url: &str,
) -> Result<HttpResponse, NetworkError> {
    let request = build_wget_request(opts, url);
    let attempts = opts.tries.max(1);
    let mut last_err = None;
    for _ in 0..attempts {
        match backend.fetch(&request) {
            Ok(r) => return Ok(r),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| NetworkError::Other("no attempts made".into())))
}

fn wget_save_one(ctx: &mut UtilContext<'_>, opts: &WgetOpts, url: &str) -> i32 {
    let backend = ctx
        .network
        .expect("network capability is checked before dispatching per-URL");
    if !opts.quiet {
        ctx.output.stderr(format!("--  {url}\n").as_bytes());
    }
    let response = match wget_fetch_one(opts, backend, url) {
        Ok(r) => r,
        Err(e) => {
            ctx.output.stderr(format!("wget: {e}\n").as_bytes());
            return 1;
        }
    };
    if response.status >= 400 {
        ctx.output
            .stderr(format!("wget: server returned error: HTTP {}\n", response.status).as_bytes());
        return 8;
    }
    if opts.output.as_deref() == Some("-") {
        ctx.output.stdout(&response.body);
        return 0;
    }
    let filename = wget_output_filename(opts, url, &response);
    let path = resolve_path(ctx.cwd, &filename);
    let h = match ctx.fs.open(&path, OpenOptions::write()) {
        Ok(h) => h,
        Err(e) => {
            ctx.output
                .stderr(format!("wget: cannot write to '{filename}': {e}\n").as_bytes());
            return 1;
        }
    };
    if let Err(e) = ctx.fs.write_file(h, &response.body) {
        ctx.output
            .stderr(format!("wget: write error: {e}\n").as_bytes());
        ctx.fs.close(h);
        return 1;
    }
    ctx.fs.close(h);
    if !opts.quiet {
        ctx.output.stderr(
            format!(
                "Saving to: '{}' [{} bytes]\n",
                filename,
                response.body.len()
            )
            .as_bytes(),
        );
    }
    0
}

pub(crate) fn util_wget(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let opts = match parse_wget_args(argv) {
        Ok(o) => o,
        Err(e) => {
            ctx.output.stderr(format!("wget: {e}\n").as_bytes());
            return 2;
        }
    };

    if opts.urls.is_empty() {
        ctx.output.stderr(b"wget: missing URL\n");
        return 2;
    }
    if ctx.network.is_none() {
        ctx.output.stderr(b"wget: network access not available\n");
        return 1;
    }

    let mut last_nonzero = 0;
    for url in &opts.urls {
        let status = wget_save_one(ctx, &opts, url);
        if status != 0 {
            last_nonzero = status;
        }
    }
    last_nonzero
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net_types::{HostAllowlist, HttpResponse, NetworkBackend, NetworkError};
    use crate::VecOutput;
    use std::cell::RefCell;
    use wasmsh_fs::MemoryFs;

    struct MockNetworkBackend {
        allowlist: HostAllowlist,
        response: HttpResponse,
        /// Sequence of errors to return on the first N calls; the (N+1)th
        /// call returns `response`.  Used to test retry behavior.
        error_queue: RefCell<Vec<NetworkError>>,
        captured: RefCell<Vec<HttpRequest>>,
    }

    impl NetworkBackend for MockNetworkBackend {
        fn fetch(&self, request: &HttpRequest) -> Result<HttpResponse, NetworkError> {
            self.captured.borrow_mut().push(request.clone());
            self.allowlist.check(&request.url)?;
            if let Some(e) = self.error_queue.borrow_mut().pop() {
                return Err(e);
            }
            Ok(self.response.clone())
        }
    }

    fn mock_backend(body: &[u8]) -> MockNetworkBackend {
        MockNetworkBackend {
            allowlist: HostAllowlist::new(vec!["example.com".into(), "*.test.co".into()]),
            response: HttpResponse {
                status: 200,
                headers: vec![
                    ("Content-Type".into(), "text/plain".into()),
                    ("Content-Length".into(), body.len().to_string()),
                ],
                body: body.to_vec(),
            },
            error_queue: RefCell::new(Vec::new()),
            captured: RefCell::new(Vec::new()),
        }
    }

    fn run_curl(argv: &[&str], backend: &dyn NetworkBackend) -> (i32, VecOutput) {
        let mut fs = MemoryFs::new();
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut output,
                cwd: "/",
                stdin: None,
                state: None,
                network: Some(backend),
            };
            util_curl(&mut ctx, argv)
        };
        (status, output)
    }

    fn run_curl_with_fs(
        argv: &[&str],
        backend: &dyn NetworkBackend,
        fs: &mut MemoryFs,
    ) -> (i32, VecOutput) {
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs,
                output: &mut output,
                cwd: "/",
                stdin: None,
                state: None,
                network: Some(backend),
            };
            util_curl(&mut ctx, argv)
        };
        (status, output)
    }

    fn seed_file(fs: &mut MemoryFs, path: &str, data: &[u8]) {
        let h = fs.open(path, OpenOptions::write()).unwrap();
        fs.write_file(h, data).unwrap();
        fs.close(h);
    }

    fn read_file(fs: &mut MemoryFs, path: &str) -> Vec<u8> {
        let h = fs.open(path, OpenOptions::read()).unwrap();
        let d = fs.read_file(h).unwrap();
        fs.close(h);
        d
    }

    // ── Legacy behavior still works ─────────────────────────────

    #[test]
    fn curl_no_network() {
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
            util_curl(&mut ctx, &["curl", "http://example.com"])
        };
        assert_eq!(status, 1);
        assert!(String::from_utf8_lossy(&output.stderr).contains("network access not available"));
    }

    #[test]
    fn curl_no_url() {
        let backend = mock_backend(b"");
        let (status, output) = run_curl(&["curl"], &backend);
        assert_eq!(status, 2);
        assert!(String::from_utf8_lossy(&output.stderr).contains("no URL"));
    }

    #[test]
    fn curl_basic_get() {
        let backend = mock_backend(b"hello world");
        let (status, output) = run_curl(&["curl", "http://example.com/data"], &backend);
        assert_eq!(status, 0);
        assert_eq!(output.stdout_str(), "hello world");
    }

    #[test]
    fn curl_host_denied() {
        let backend = mock_backend(b"");
        let (status, output) = run_curl(&["curl", "http://evil.com/hack"], &backend);
        assert_eq!(status, 6);
        assert!(String::from_utf8_lossy(&output.stderr).contains("denied"));
    }

    #[test]
    fn curl_head_only_uses_http11_status_line() {
        let backend = mock_backend(b"body");
        let (status, output) = run_curl(&["curl", "-I", "http://example.com/"], &backend);
        assert_eq!(status, 0);
        let s = output.stdout_str();
        assert!(s.starts_with("HTTP/1.1 200 OK"), "status line: {s}");
        assert!(s.contains("Content-Type: text/plain"));
        assert!(!s.contains("body"));
    }

    #[test]
    fn curl_combined_flags() {
        let backend = mock_backend(b"data");
        let (status, output) = run_curl(&["curl", "-sSL", "http://example.com/"], &backend);
        assert_eq!(status, 0);
        assert_eq!(output.stdout_str(), "data");
    }

    // ── @file and data flags ────────────────────────────────────

    #[test]
    fn curl_data_from_file_strips_newlines() {
        let backend = mock_backend(b"ok");
        let mut fs = MemoryFs::new();
        seed_file(&mut fs, "/body.txt", b"line1\nline2\r\n");
        let _ = run_curl_with_fs(
            &["curl", "-d", "@/body.txt", "http://example.com/api"],
            &backend,
            &mut fs,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.body.as_deref(), Some(b"line1line2".as_ref()));
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "Content-Type" && v == "application/x-www-form-urlencoded"));
    }

    #[test]
    fn curl_data_binary_preserves_newlines() {
        let backend = mock_backend(b"ok");
        let mut fs = MemoryFs::new();
        seed_file(&mut fs, "/body.bin", b"line1\nline2\n");
        let _ = run_curl_with_fs(
            &[
                "curl",
                "--data-binary",
                "@/body.bin",
                "http://example.com/api",
            ],
            &backend,
            &mut fs,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert_eq!(req.body.as_deref(), Some(b"line1\nline2\n".as_ref()));
    }

    #[test]
    fn curl_data_raw_does_not_interpret_at() {
        let backend = mock_backend(b"ok");
        let (status, _) = run_curl(
            &["curl", "--data-raw", "@literal", "http://example.com/api"],
            &backend,
        );
        assert_eq!(status, 0);
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert_eq!(req.body.as_deref(), Some(b"@literal".as_ref()));
    }

    #[test]
    fn curl_data_urlencode_variants() {
        let backend = mock_backend(b"ok");
        let mut fs = MemoryFs::new();
        seed_file(&mut fs, "/msg.txt", b"hello world");
        let _ = run_curl_with_fs(
            &[
                "curl",
                "--data-urlencode",
                "name=a b/c",
                "--data-urlencode",
                "text@/msg.txt",
                "http://example.com/f",
            ],
            &backend,
            &mut fs,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        let body = String::from_utf8(req.body.unwrap()).unwrap();
        assert_eq!(body, "name=a%20b%2Fc&text=hello%20world");
    }

    #[test]
    fn curl_json_flag_sets_headers_and_body() {
        let backend = mock_backend(b"ok");
        let (status, _) = run_curl(
            &["curl", "--json", "{\"k\":1}", "http://example.com/api"],
            &backend,
        );
        assert_eq!(status, 0);
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert_eq!(req.body.as_deref(), Some(b"{\"k\":1}".as_ref()));
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "Content-Type" && v == "application/json"));
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "Accept" && v == "application/json"));
    }

    #[test]
    fn curl_json_file_source() {
        let backend = mock_backend(b"ok");
        let mut fs = MemoryFs::new();
        seed_file(&mut fs, "/p.json", b"{\"ok\":true}");
        let _ = run_curl_with_fs(
            &["curl", "--json", "@/p.json", "http://example.com/api"],
            &backend,
            &mut fs,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert_eq!(req.body.as_deref(), Some(b"{\"ok\":true}".as_ref()));
    }

    // ── Shortcut flags (Tier 1/2) ───────────────────────────────

    #[test]
    fn curl_user_sets_basic_auth() {
        let backend = mock_backend(b"ok");
        let _ = run_curl(
            &["curl", "-u", "alice:s3cret", "http://example.com/"],
            &backend,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        let auth = req
            .headers
            .iter()
            .find(|(k, _)| k == "Authorization")
            .cloned()
            .unwrap();
        let expected = format!("Basic {}", B64_STANDARD.encode(b"alice:s3cret"));
        assert_eq!(auth.1, expected);
    }

    #[test]
    fn curl_user_agent_and_referer_shortcuts() {
        let backend = mock_backend(b"");
        let _ = run_curl(
            &[
                "curl",
                "-A",
                "wasmsh/1",
                "-e",
                "http://referer.example/",
                "http://example.com/",
            ],
            &backend,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "User-Agent" && v == "wasmsh/1"));
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "Referer" && v == "http://referer.example/"));
    }

    #[test]
    fn curl_include_puts_headers_before_body() {
        let backend = mock_backend(b"BODY");
        let (status, output) = run_curl(&["curl", "-i", "http://example.com/"], &backend);
        assert_eq!(status, 0);
        let s = output.stdout_str();
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(s.ends_with("BODY"));
    }

    #[test]
    fn curl_dump_header_to_stdout() {
        let backend = mock_backend(b"B");
        let (status, output) = run_curl(&["curl", "-D", "-", "http://example.com/"], &backend);
        assert_eq!(status, 0);
        let s = output.stdout_str();
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(s.ends_with('B'));
    }

    #[test]
    fn curl_dump_header_to_file() {
        let backend = mock_backend(b"BODY");
        let mut fs = MemoryFs::new();
        let (status, _) = run_curl_with_fs(
            &[
                "curl",
                "-o",
                "/out.bin",
                "-D",
                "/hdr.txt",
                "http://example.com/",
            ],
            &backend,
            &mut fs,
        );
        assert_eq!(status, 0);
        let hdr = String::from_utf8(read_file(&mut fs, "/hdr.txt")).unwrap();
        assert!(hdr.starts_with("HTTP/1.1 200 OK\r\n"));
        assert_eq!(read_file(&mut fs, "/out.bin"), b"BODY");
    }

    #[test]
    fn curl_remote_name_uses_url_basename() {
        let backend = mock_backend(b"DATA");
        let mut fs = MemoryFs::new();
        let (status, _) = run_curl_with_fs(
            &["curl", "-O", "http://example.com/path/foo.tgz"],
            &backend,
            &mut fs,
        );
        assert_eq!(status, 0);
        assert_eq!(read_file(&mut fs, "/foo.tgz"), b"DATA");
    }

    #[test]
    fn curl_remote_header_name_uses_content_disposition() {
        let mut backend = mock_backend(b"CD");
        backend.response.headers.push((
            "Content-Disposition".into(),
            "attachment; filename=\"../escape.bin\"".into(),
        ));
        let mut fs = MemoryFs::new();
        let (status, _) = run_curl_with_fs(
            &["curl", "-O", "-J", "http://example.com/ignored"],
            &backend,
            &mut fs,
        );
        assert_eq!(status, 0);
        // Path traversal must have been stripped.
        assert_eq!(read_file(&mut fs, "/escape.bin"), b"CD");
    }

    #[test]
    fn curl_output_dir_and_create_dirs() {
        let backend = mock_backend(b"XYZ");
        let mut fs = MemoryFs::new();
        let (status, _) = run_curl_with_fs(
            &[
                "curl",
                "--output-dir",
                "/nested/dir",
                "--create-dirs",
                "-o",
                "file.out",
                "http://example.com/",
            ],
            &backend,
            &mut fs,
        );
        assert_eq!(status, 0);
        assert_eq!(read_file(&mut fs, "/nested/dir/file.out"), b"XYZ");
    }

    // ── Multipart ───────────────────────────────────────────────

    #[test]
    fn curl_form_text_parts() {
        let backend = mock_backend(b"ok");
        let _ = run_curl(
            &[
                "curl",
                "-F",
                "a=1",
                "--form-string",
                "b=@literal",
                "http://example.com/api",
            ],
            &backend,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        let ct = req
            .headers
            .iter()
            .find(|(k, _)| k == "Content-Type")
            .cloned()
            .unwrap()
            .1;
        assert!(ct.starts_with("multipart/form-data; boundary="));
        let body = String::from_utf8_lossy(req.body.as_ref().unwrap()).into_owned();
        assert!(body.contains("name=\"a\""));
        assert!(body.contains("\r\n\r\n1\r\n"));
        assert!(body.contains("name=\"b\""));
        assert!(body.contains("\r\n\r\n@literal\r\n"));
    }

    #[test]
    fn curl_form_file_part() {
        let backend = mock_backend(b"ok");
        let mut fs = MemoryFs::new();
        seed_file(&mut fs, "/payload.bin", b"PAY");
        let _ = run_curl_with_fs(
            &["curl", "-F", "up=@/payload.bin", "http://example.com/u"],
            &backend,
            &mut fs,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        let body = String::from_utf8_lossy(req.body.as_ref().unwrap()).into_owned();
        assert!(body.contains("filename=\"payload.bin\""));
        assert!(body.contains("Content-Type: application/octet-stream"));
        assert!(body.contains("\r\n\r\nPAY\r\n"));
    }

    // ── Sandbox limits / retries ────────────────────────────────

    #[test]
    fn curl_max_filesize_exceeded_returns_63() {
        let backend = mock_backend(b"0123456789");
        let (status, output) = run_curl(
            &[
                "curl",
                "-s",
                "-S",
                "--max-filesize",
                "4",
                "http://example.com/",
            ],
            &backend,
        );
        assert_eq!(status, 63);
        assert!(String::from_utf8_lossy(&output.stderr).contains("Maximum file size exceeded"));
    }

    #[test]
    fn curl_max_filesize_within_limit_succeeds() {
        let backend = mock_backend(b"abc");
        let (status, _) = run_curl(
            &["curl", "--max-filesize", "10", "http://example.com/"],
            &backend,
        );
        assert_eq!(status, 0);
    }

    #[test]
    fn curl_limits_are_plumbed_to_request() {
        let backend = mock_backend(b"");
        let _ = run_curl(
            &[
                "curl",
                "--max-time",
                "2.5",
                "--connect-timeout",
                "1",
                "--max-redirs",
                "3",
                "http://example.com/",
            ],
            &backend,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert_eq!(req.timeout_ms, Some(2500));
        assert_eq!(req.connect_timeout_ms, Some(1000));
        assert_eq!(req.max_redirs, Some(3));
    }

    #[test]
    fn curl_retries_on_connection_failure_then_succeeds() {
        let backend = mock_backend(b"ok");
        backend
            .error_queue
            .borrow_mut()
            .extend([NetworkError::Timeout("t1".into())]);
        let (status, output) = run_curl(&["curl", "--retry", "3", "http://example.com/"], &backend);
        assert_eq!(status, 0);
        assert_eq!(output.stdout_str(), "ok");
        assert_eq!(backend.captured.borrow().len(), 2);
    }

    #[test]
    fn curl_retry_all_errors_covers_http_4xx() {
        let mut backend = mock_backend(b"");
        backend.response = HttpResponse {
            status: 418,
            headers: vec![],
            body: b"teapot".to_vec(),
        };
        let (status, _) = run_curl(
            &[
                "curl",
                "--retry",
                "2",
                "--retry-all-errors",
                "-s",
                "http://example.com/",
            ],
            &backend,
        );
        assert_eq!(status, 0);
        // 1 original + 2 retries = 3 attempts.
        assert_eq!(backend.captured.borrow().len(), 3);
    }

    #[test]
    fn curl_retry_connrefused_retries_refused() {
        let backend = mock_backend(b"ok");
        backend
            .error_queue
            .borrow_mut()
            .extend([NetworkError::ConnectionFailed("Connection refused".into())]);
        let (status, _) = run_curl(
            &[
                "curl",
                "--retry",
                "2",
                "--retry-connrefused",
                "http://example.com/",
            ],
            &backend,
        );
        assert_eq!(status, 0);
        assert_eq!(backend.captured.borrow().len(), 2);
    }

    // ── Write-out ───────────────────────────────────────────────

    #[test]
    fn curl_write_out_tokens() {
        let backend = mock_backend(b"abcde");
        let (status, output) = run_curl(
            &[
                "curl",
                "-s",
                "-w",
                "code=%{http_code} size=%{size_download} url=%{url_effective} ct=%{content_type} ver=%{http_version}\\n",
                "http://example.com/path",
            ],
            &backend,
        );
        assert_eq!(status, 0);
        let s = output.stdout_str();
        assert!(
            s.ends_with("code=200 size=5 url=http://example.com/path ct=text/plain ver=1.1\n"),
            "output: {s}"
        );
    }

    // ── Tier-4 rejection ────────────────────────────────────────

    #[test]
    fn curl_rejects_proxy_flag_as_unsupported() {
        let backend = mock_backend(b"");
        let (status, output) = run_curl(
            &["curl", "--proxy", "http://p/", "http://example.com/"],
            &backend,
        );
        assert_eq!(status, 2);
        let msg = String::from_utf8_lossy(&output.stderr);
        assert!(msg.contains("not supported in sandbox"), "stderr: {msg}");
    }

    #[test]
    fn curl_rejects_cookie_jar_flag() {
        // Cookie *jar* (-c/--cookie-jar) is still unsupported because the
        // sandbox does not model response cookies.  The single-request
        // `--cookie` (-b) flag, however, is now implemented.
        let backend = mock_backend(b"");
        let (status, _) = run_curl(
            &["curl", "--cookie-jar", "/tmp/j", "http://example.com/"],
            &backend,
        );
        assert_eq!(status, 2);
    }

    // ── Tier-2 features ─────────────────────────────────────────

    #[test]
    fn curl_get_moves_body_to_query_string() {
        let backend = mock_backend(b"");
        let _ = run_curl(
            &[
                "curl",
                "-G",
                "--data-urlencode",
                "q=hello world",
                "http://example.com/search",
            ],
            &backend,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert_eq!(req.method, "GET");
        assert!(req.body.is_none());
        assert_eq!(req.url, "http://example.com/search?q=hello%20world");
    }

    #[test]
    fn curl_get_appends_to_existing_query() {
        let backend = mock_backend(b"");
        let _ = run_curl(
            &[
                "curl",
                "-G",
                "--data-urlencode",
                "b=2",
                "http://example.com/api?a=1",
            ],
            &backend,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert_eq!(req.url, "http://example.com/api?a=1&b=2");
    }

    #[test]
    fn curl_range_sets_header() {
        let backend = mock_backend(b"");
        let _ = run_curl(&["curl", "-r", "0-1023", "http://example.com/"], &backend);
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "Range" && v == "bytes=0-1023"));
    }

    #[test]
    fn curl_upload_file_puts_file_contents() {
        let backend = mock_backend(b"ok");
        let mut fs = MemoryFs::new();
        seed_file(&mut fs, "/big.bin", b"PAYLOAD");
        let _ = run_curl_with_fs(
            &["curl", "-T", "/big.bin", "http://example.com/put"],
            &backend,
            &mut fs,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert_eq!(req.method, "PUT");
        assert_eq!(req.body.as_deref(), Some(b"PAYLOAD".as_ref()));
    }

    #[test]
    fn curl_cookie_literal_sets_header() {
        let backend = mock_backend(b"");
        let _ = run_curl(
            &[
                "curl",
                "-b",
                "session=abc; tracking=xyz",
                "http://example.com/",
            ],
            &backend,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "Cookie" && v == "session=abc; tracking=xyz"));
    }

    #[test]
    fn curl_cookie_from_file() {
        let backend = mock_backend(b"");
        let mut fs = MemoryFs::new();
        seed_file(
            &mut fs,
            "/cookies.txt",
            b"# comment\nsession=abc\n\nauth=def\n",
        );
        let _ = run_curl_with_fs(
            &["curl", "-b", "@/cookies.txt", "http://example.com/"],
            &backend,
            &mut fs,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        let cookie = req
            .headers
            .iter()
            .find(|(k, _)| k == "Cookie")
            .cloned()
            .unwrap()
            .1;
        assert_eq!(cookie, "session=abc; auth=def");
    }

    #[test]
    fn curl_compressed_sets_accept_encoding_and_decodes_gzip() {
        let original = b"this-is-decompressed-text".to_vec();
        // Build a gzip stream identical to what a real server would send.
        let mut gz = vec![0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xff];
        gz.extend_from_slice(&miniz_oxide::deflate::compress_to_vec(&original, 6));
        // CRC32 and ISIZE (little-endian).
        let crc = crc32_of(&original);
        gz.extend_from_slice(&crc.to_le_bytes());
        gz.extend_from_slice(&(original.len() as u32).to_le_bytes());

        let mut backend = mock_backend(b"");
        backend.response = HttpResponse {
            status: 200,
            headers: vec![
                ("Content-Type".into(), "text/plain".into()),
                ("Content-Encoding".into(), "gzip".into()),
                ("Content-Length".into(), gz.len().to_string()),
            ],
            body: gz,
        };
        let (status, output) = run_curl(
            &["curl", "--compressed", "-i", "http://example.com/"],
            &backend,
        );
        assert_eq!(status, 0);
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "Accept-Encoding" && v == "gzip, deflate"));
        let s = output.stdout_str();
        assert!(s.ends_with(std::str::from_utf8(&original).unwrap()));
        // Content-Encoding must be stripped after decoding.
        assert!(!s.to_ascii_lowercase().contains("content-encoding"));
    }

    fn crc32_of(data: &[u8]) -> u32 {
        // Identical polynomial to `helpers::crc32` but inlined here to
        // avoid a `pub(crate)` leak just for a test helper.
        let mut crc: u32 = 0xFFFF_FFFF;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        !crc
    }

    #[test]
    fn curl_fail_with_body_writes_body_then_fails() {
        let mut backend = mock_backend(b"");
        backend.response = HttpResponse {
            status: 404,
            headers: vec![],
            body: b"<html>not found</html>".to_vec(),
        };
        let (status, output) = run_curl(
            &["curl", "--fail-with-body", "http://example.com/missing"],
            &backend,
        );
        assert_eq!(status, 22);
        assert_eq!(output.stdout_str(), "<html>not found</html>");
        assert!(String::from_utf8_lossy(&output.stderr).contains("returned error: 404"));
    }

    #[test]
    fn curl_fail_still_suppresses_body() {
        let mut backend = mock_backend(b"");
        backend.response = HttpResponse {
            status: 500,
            headers: vec![],
            body: b"server exploded".to_vec(),
        };
        let (status, output) = run_curl(&["curl", "-f", "http://example.com/"], &backend);
        assert_eq!(status, 22);
        // -f (without --fail-with-body) must NOT print the body.
        assert!(output.stdout.is_empty());
    }

    // ── Tier-3 polish: auth, time-cond, netrc, no-ops ───────────

    #[test]
    fn curl_oauth2_bearer_sets_authorization() {
        let backend = mock_backend(b"");
        let _ = run_curl(
            &["curl", "--oauth2-bearer", "token123", "http://example.com/"],
            &backend,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer token123"));
    }

    #[test]
    fn curl_time_cond_sets_if_modified_since() {
        let backend = mock_backend(b"");
        let _ = run_curl(
            &[
                "curl",
                "-z",
                "Wed, 21 Oct 2015 07:28:00 GMT",
                "http://example.com/",
            ],
            &backend,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "If-Modified-Since" && v == "Wed, 21 Oct 2015 07:28:00 GMT"));
    }

    #[test]
    fn curl_netrc_file_supplies_basic_auth() {
        let backend = mock_backend(b"");
        let mut fs = MemoryFs::new();
        seed_file(
            &mut fs,
            "/creds.netrc",
            b"machine example.com\n  login alice\n  password s3cret\n\ndefault\n  login guest\n  password guest\n",
        );
        let _ = run_curl_with_fs(
            &[
                "curl",
                "--netrc-file",
                "/creds.netrc",
                "http://example.com/",
            ],
            &backend,
            &mut fs,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        let auth = req
            .headers
            .iter()
            .find(|(k, _)| k == "Authorization")
            .cloned()
            .unwrap()
            .1;
        let expected = format!("Basic {}", B64_STANDARD.encode(b"alice:s3cret"));
        assert_eq!(auth, expected);
    }

    #[test]
    fn curl_netrc_default_entry_used_when_host_not_listed() {
        let backend = mock_backend(b"");
        let mut fs = MemoryFs::new();
        seed_file(
            &mut fs,
            "/creds.netrc",
            b"machine other.example.com\n  login x\n  password y\n\ndefault\n  login guest\n  password guestpw\n",
        );
        let _ = run_curl_with_fs(
            &[
                "curl",
                "--netrc-file",
                "/creds.netrc",
                "http://example.com/",
            ],
            &backend,
            &mut fs,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        let auth = req
            .headers
            .iter()
            .find(|(k, _)| k == "Authorization")
            .cloned()
            .unwrap()
            .1;
        let expected = format!("Basic {}", B64_STANDARD.encode(b"guest:guestpw"));
        assert_eq!(auth, expected);
    }

    #[test]
    fn curl_user_beats_oauth_and_netrc() {
        let backend = mock_backend(b"");
        let mut fs = MemoryFs::new();
        seed_file(
            &mut fs,
            "/n.txt",
            b"machine example.com\n  login netrcuser\n  password netrcpw\n",
        );
        let _ = run_curl_with_fs(
            &[
                "curl",
                "-u",
                "explicit:pw",
                "--oauth2-bearer",
                "ignored",
                "--netrc-file",
                "/n.txt",
                "http://example.com/",
            ],
            &backend,
            &mut fs,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        let auth = req
            .headers
            .iter()
            .find(|(k, _)| k == "Authorization")
            .cloned()
            .unwrap()
            .1;
        let expected = format!("Basic {}", B64_STANDARD.encode(b"explicit:pw"));
        assert_eq!(auth, expected);
    }

    #[test]
    fn curl_silently_accepts_sandbox_noop_flags() {
        let backend = mock_backend(b"data");
        let (status, _) = run_curl(
            &[
                "curl",
                "--http2",
                "--tcp-nodelay",
                "--ipv4",
                "--path-as-is",
                "--no-keepalive",
                "--tlsv1.3",
                "--resolve",
                "example.com:443:127.0.0.1",
                "http://example.com/",
            ],
            &backend,
        );
        assert_eq!(status, 0);
    }

    #[test]
    fn curl_short_noop_flags_parse() {
        let backend = mock_backend(b"ok");
        // -# (progress bar), -4 (IPv4), -2 (SSLv2 selector) are all no-ops.
        let (status, _) = run_curl(&["curl", "-#4", "http://example.com/"], &backend);
        assert_eq!(status, 0);
    }

    // ── Multi-URL + --next + --remote-name-all ──────────────────

    #[test]
    fn curl_multiple_positional_urls_all_fetched() {
        let backend = mock_backend(b"body");
        let (status, _) = run_curl(
            &[
                "curl",
                "-s",
                "http://example.com/a",
                "http://example.com/b",
                "http://example.com/c",
            ],
            &backend,
        );
        assert_eq!(status, 0);
        let captured = backend.captured.borrow();
        assert_eq!(captured.len(), 3);
        assert_eq!(captured[0].url, "http://example.com/a");
        assert_eq!(captured[1].url, "http://example.com/b");
        assert_eq!(captured[2].url, "http://example.com/c");
    }

    #[test]
    fn curl_remote_name_all_writes_every_url_to_its_basename() {
        let backend = mock_backend(b"DATA");
        let mut fs = MemoryFs::new();
        let (status, _) = run_curl_with_fs(
            &[
                "curl",
                "-s",
                "--remote-name-all",
                "http://example.com/a.txt",
                "http://example.com/b.txt",
            ],
            &backend,
            &mut fs,
        );
        assert_eq!(status, 0);
        assert_eq!(read_file(&mut fs, "/a.txt"), b"DATA");
        assert_eq!(read_file(&mut fs, "/b.txt"), b"DATA");
    }

    #[test]
    fn curl_next_resets_options_between_segments() {
        let backend = mock_backend(b"ok");
        let _ = run_curl(
            &[
                "curl",
                "-s",
                "-H",
                "X-First: yes",
                "http://example.com/first",
                "--next",
                "-H",
                "X-Second: yes",
                "http://example.com/second",
            ],
            &backend,
        );
        let captured = backend.captured.borrow();
        assert_eq!(captured.len(), 2);
        assert!(captured[0].headers.iter().any(|(k, _)| k == "X-First"));
        assert!(!captured[0].headers.iter().any(|(k, _)| k == "X-Second"));
        assert!(captured[1].headers.iter().any(|(k, _)| k == "X-Second"));
        // The second segment must NOT inherit `X-First`.
        assert!(!captured[1].headers.iter().any(|(k, _)| k == "X-First"));
    }

    #[test]
    fn curl_dash_colon_is_next_alias() {
        let backend = mock_backend(b"ok");
        let _ = run_curl(
            &[
                "curl",
                "-s",
                "http://example.com/a",
                "-:",
                "http://example.com/b",
            ],
            &backend,
        );
        assert_eq!(backend.captured.borrow().len(), 2);
    }

    #[test]
    fn curl_parallel_short_flag_is_silent_noop() {
        let backend = mock_backend(b"ok");
        let (status, _) = run_curl(
            &[
                "curl",
                "-Z",
                "-s",
                "http://example.com/a",
                "http://example.com/b",
            ],
            &backend,
        );
        assert_eq!(status, 0);
        assert_eq!(backend.captured.borrow().len(), 2);
    }

    // ── -K / --config file expansion ────────────────────────────

    #[test]
    fn curl_config_file_supplies_options() {
        let backend = mock_backend(b"ok");
        let mut fs = MemoryFs::new();
        seed_file(
            &mut fs,
            "/etc/curl.cfg",
            b"# config\n-s\n-H \"X-Cfg: yes\"\nurl = \"http://example.com/cfg\"\n",
        );
        let (status, _) = run_curl_with_fs(&["curl", "-K", "/etc/curl.cfg"], &backend, &mut fs);
        assert_eq!(status, 0);
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert_eq!(req.url, "http://example.com/cfg");
        assert!(req.headers.iter().any(|(k, v)| k == "X-Cfg" && v == "yes"));
    }

    // ── Richer -w tokens ────────────────────────────────────────

    #[test]
    fn curl_write_out_method_scheme_urlnum_and_header_ref() {
        let mut backend = mock_backend(b"");
        backend
            .response
            .headers
            .push(("X-Custom".into(), "the-value".into()));
        let mut fs = MemoryFs::new();
        let (status, output) = run_curl_with_fs(
            &[
                "curl",
                "-s",
                "-o",
                "/discard",
                "-w",
                "%{method} %{scheme} %{urlnum} %{header{x-custom}}\\n",
                "http://example.com/a",
                "http://example.com/b",
            ],
            &backend,
            &mut fs,
        );
        assert_eq!(status, 0);
        let s = output.stdout_str();
        let lines: Vec<&str> = s.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "GET http 0 the-value");
        assert_eq!(lines[1], "GET http 1 the-value");
    }

    #[test]
    fn curl_write_out_json_token() {
        let backend = mock_backend(b"xyz");
        let (status, output) = run_curl(
            &[
                "curl",
                "-s",
                "-w",
                "%{json}",
                "http://example.com/search?q=1",
            ],
            &backend,
        );
        assert_eq!(status, 0);
        let s = output.stdout_str();
        let json = s.trim_start_matches("xyz");
        assert!(json.contains("\"http_code\":200"));
        assert!(json.contains("\"method\":\"GET\""));
        assert!(json.contains("\"scheme\":\"http\""));
        assert!(json.contains("\"size_download\":3"));
    }

    // ── AWS SigV4 ───────────────────────────────────────────────

    #[test]
    fn curl_aws_sigv4_sets_authorization_and_amz_date() {
        let backend = mock_backend(b"ok");
        let _ = run_curl(
            &[
                "curl",
                "-s",
                "-u",
                "AKIAIOSFODNN7EXAMPLE:wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
                "--aws-sigv4",
                "aws:amz:us-east-1:s3",
                "http://example.com/path",
            ],
            &backend,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        let auth = req
            .headers
            .iter()
            .find(|(k, _)| k == "Authorization")
            .cloned()
            .unwrap()
            .1;
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/"));
        assert!(auth.contains("/us-east-1/s3/aws4_request,"));
        assert!(auth.contains("SignedHeaders="));
        assert!(auth.contains("Signature="));
        // The x-amz-date header must be present and ISO8601-basic.
        let amz = req
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("x-amz-date"))
            .cloned()
            .unwrap()
            .1;
        assert_eq!(amz.len(), 16);
        assert!(amz.ends_with('Z'));
        // Default WASMSH_DATE is 2026-01-01 00:00:00.
        assert_eq!(amz, "20260101T000000Z");
        // There must be no residual Basic Authorization from -u.
        assert!(!auth.contains("Basic "));
    }

    #[test]
    fn curl_aws_sigv4_signature_matches_known_vector() {
        // Pin the signing-key chain output against a hand-computed vector so
        // regressions in HMAC/SHA256 composition or scope formatting fail
        // loudly instead of silently producing garbage signatures.
        let key = sigv4_signing_key(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20150830",
            "us-east-1",
            "iam",
        );
        assert_eq!(
            hex_encode(&key),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
    }

    // ── wget parity ─────────────────────────────────────────────

    fn run_wget(argv: &[&str], backend: &dyn NetworkBackend) -> (i32, VecOutput, MemoryFs) {
        let mut fs = MemoryFs::new();
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut output,
                cwd: "/",
                stdin: None,
                state: None,
                network: Some(backend),
            };
            util_wget(&mut ctx, argv)
        };
        (status, output, fs)
    }

    #[test]
    fn wget_user_password_equals_form() {
        let backend = mock_backend(b"");
        let (_, _, _) = run_wget(
            &[
                "wget",
                "--user=alice",
                "--password=sec",
                "http://example.com/",
            ],
            &backend,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        let expected = format!("Basic {}", B64_STANDARD.encode(b"alice:sec"));
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && *v == expected));
    }

    #[test]
    fn wget_post_data_equals_form() {
        let backend = mock_backend(b"");
        let _ = run_wget(
            &[
                "wget",
                "--post-data=key=value&k2=v2",
                "http://example.com/api",
            ],
            &backend,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.body.as_deref(), Some(b"key=value&k2=v2".as_ref()));
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "Content-Type" && v == "application/x-www-form-urlencoded"));
    }

    #[test]
    fn wget_tries_retries_on_error() {
        let backend = mock_backend(b"late-success");
        backend.error_queue.borrow_mut().extend([
            NetworkError::Timeout("t1".into()),
            NetworkError::ConnectionFailed("c1".into()),
        ]);
        let (status, _, _) = run_wget(
            &["wget", "--tries=3", "-q", "-O", "-", "http://example.com/"],
            &backend,
        );
        assert_eq!(status, 0);
        assert_eq!(backend.captured.borrow().len(), 3);
    }

    #[test]
    fn wget_content_disposition_filename() {
        let mut backend = mock_backend(b"PAYLOAD");
        backend.response.headers.push((
            "Content-Disposition".into(),
            "attachment; filename=\"report-2026.csv\"".into(),
        ));
        let (status, _, mut fs) = run_wget(
            &[
                "wget",
                "--content-disposition",
                "http://example.com/download?id=42",
            ],
            &backend,
        );
        assert_eq!(status, 0);
        assert_eq!(read_file(&mut fs, "/report-2026.csv"), b"PAYLOAD");
    }

    #[test]
    fn wget_multi_url() {
        let backend = mock_backend(b"x");
        let (_, _, _) = run_wget(
            &[
                "wget",
                "-q",
                "-O",
                "-",
                "http://example.com/a",
                "http://example.com/b",
            ],
            &backend,
        );
        assert_eq!(backend.captured.borrow().len(), 2);
    }

    #[test]
    fn wget_timeout_maps_to_request_timeouts() {
        let backend = mock_backend(b"");
        let _ = run_wget(
            &["wget", "--timeout=3", "-q", "http://example.com/"],
            &backend,
        );
        let req = backend.captured.borrow().last().cloned().unwrap();
        assert_eq!(req.timeout_ms, Some(3000));
    }

    // ── Verbose uses HTTP/1.1 status line ───────────────────────

    #[test]
    fn curl_verbose_has_http11_response_line() {
        let backend = mock_backend(b"data");
        let (status, output) = run_curl(&["curl", "-v", "http://example.com/api"], &backend);
        assert_eq!(status, 0);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("> GET http://example.com/api"));
        assert!(stderr.contains("< HTTP/1.1 200 OK"));
    }

    // ── wget still works ────────────────────────────────────────

    #[test]
    fn wget_download_to_file() {
        let backend = mock_backend(b"downloaded data");
        let mut fs = MemoryFs::new();
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut output,
                cwd: "/",
                stdin: None,
                state: None,
                network: Some(&backend),
            };
            util_wget(&mut ctx, &["wget", "http://example.com/report.csv"])
        };
        assert_eq!(status, 0);
        let data = read_file(&mut fs, "/report.csv");
        assert_eq!(data, b"downloaded data");
    }

    #[test]
    fn wget_default_filename() {
        assert_eq!(filename_from_url("http://example.com/"), "index.html");
        assert_eq!(filename_from_url("http://example.com/data.csv"), "data.csv");
        assert_eq!(
            filename_from_url("http://example.com/path/to/file.json"),
            "file.json"
        );
    }
}
