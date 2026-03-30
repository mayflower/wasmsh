//! Network utilities: `curl` and `wget`.

use crate::helpers::resolve_path;
use crate::net_types::{HttpRequest, NetworkError};
use crate::UtilContext;
use wasmsh_fs::{OpenOptions, Vfs};

// ── curl ────────────────────────────────────────────────────────

#[allow(clippy::struct_excessive_bools)]
struct CurlOpts {
    url: Option<String>,
    method: Option<String>,
    headers: Vec<(String, String)>,
    data: Option<String>,
    output: Option<String>,
    silent: bool,
    show_error: bool,
    follow_redirects: bool,
    head_only: bool,
    fail_on_error: bool,
    verbose: bool,
    write_out: Option<String>,
}

fn parse_curl_args(argv: &[&str]) -> Result<CurlOpts, String> {
    let mut opts = CurlOpts {
        url: None,
        method: None,
        headers: Vec::new(),
        data: None,
        output: None,
        silent: false,
        show_error: false,
        follow_redirects: false,
        head_only: false,
        fail_on_error: false,
        verbose: false,
        write_out: None,
    };

    let mut i = 1; // skip argv[0] ("curl")
    while i < argv.len() {
        let arg = argv[i];
        match arg {
            "-X" | "--request" => {
                i += 1;
                opts.method = Some(
                    argv.get(i)
                        .ok_or("-X requires a method argument")?
                        .to_string(),
                );
            }
            "-H" | "--header" => {
                i += 1;
                let header = *argv.get(i).ok_or("-H requires a header argument")?;
                if let Some((k, v)) = header.split_once(':') {
                    opts.headers
                        .push((k.trim().to_string(), v.trim().to_string()));
                } else {
                    return Err(format!("invalid header format: {header}"));
                }
            }
            "-d" | "--data" => {
                i += 1;
                opts.data = Some(
                    argv.get(i)
                        .ok_or("-d requires a data argument")?
                        .to_string(),
                );
            }
            "-o" | "--output" => {
                i += 1;
                opts.output = Some(
                    argv.get(i)
                        .ok_or("-o requires a filename argument")?
                        .to_string(),
                );
            }
            "-w" | "--write-out" => {
                i += 1;
                opts.write_out = Some(
                    argv.get(i)
                        .ok_or("-w requires a format argument")?
                        .to_string(),
                );
            }
            "-s" | "--silent" => opts.silent = true,
            "-S" | "--show-error" => opts.show_error = true,
            "-L" | "--location" => opts.follow_redirects = true,
            "-I" | "--head" => opts.head_only = true,
            "-f" | "--fail" => opts.fail_on_error = true,
            "-v" | "--verbose" => opts.verbose = true,
            "-sS" | "-Ss" => {
                opts.silent = true;
                opts.show_error = true;
            }
            _ if arg.starts_with('-') => {
                // Handle combined short flags like -sSL
                let flags = &arg[1..];
                let mut j = 0;
                while j < flags.len() {
                    match flags.as_bytes()[j] {
                        b's' => opts.silent = true,
                        b'S' => opts.show_error = true,
                        b'L' => opts.follow_redirects = true,
                        b'I' => opts.head_only = true,
                        b'f' => opts.fail_on_error = true,
                        b'v' => opts.verbose = true,
                        b'o' => {
                            // -o might be combined: -so file.txt
                            let rest = &flags[j + 1..];
                            if rest.is_empty() {
                                i += 1;
                                opts.output = Some(
                                    argv.get(i)
                                        .ok_or("-o requires a filename argument")?
                                        .to_string(),
                                );
                            } else {
                                opts.output = Some(rest.to_string());
                            }
                            j = flags.len(); // consumed rest
                            continue;
                        }
                        _ => {
                            return Err(format!("unknown option: -{}", flags.as_bytes()[j] as char))
                        }
                    }
                    j += 1;
                }
            }
            _ => {
                // Positional: URL
                if opts.url.is_none() {
                    opts.url = Some(arg.to_string());
                } else {
                    return Err(format!("unexpected argument: {arg}"));
                }
            }
        }
        i += 1;
    }

    Ok(opts)
}

pub(crate) fn util_curl(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let opts = match parse_curl_args(argv) {
        Ok(o) => o,
        Err(e) => {
            ctx.output.stderr(format!("curl: {e}\n").as_bytes());
            return 2;
        }
    };

    let Some(url) = &opts.url else {
        ctx.output.stderr(b"curl: no URL specified\n");
        return 2;
    };
    let url = url.clone();

    let Some(backend) = ctx.network else {
        ctx.output.stderr(b"curl: network access not available\n");
        return 1;
    };

    let method = opts.method.clone().unwrap_or_else(|| {
        if opts.head_only {
            "HEAD".into()
        } else if opts.data.is_some() {
            "POST".into()
        } else {
            "GET".into()
        }
    });

    let mut headers = opts.headers.clone();
    if opts.data.is_some()
        && !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
        headers.push((
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        ));
    }

    let request = HttpRequest {
        url: url.clone(),
        method: method.clone(),
        headers,
        body: opts.data.as_ref().map(|d| d.as_bytes().to_vec()),
        follow_redirects: opts.follow_redirects,
    };

    if opts.verbose {
        ctx.output.stderr(format!("> {method} {url}\n").as_bytes());
        for (k, v) in &request.headers {
            ctx.output.stderr(format!("> {k}: {v}\n").as_bytes());
        }
        ctx.output.stderr(b">\n");
    }

    let response = match backend.fetch(&request) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("curl: {e}\n");
            if !opts.silent || opts.show_error {
                ctx.output.stderr(msg.as_bytes());
            }
            return match e {
                NetworkError::HostDenied(_) => 6,
                NetworkError::ConnectionFailed(_) => 7,
                NetworkError::Timeout(_) => 28,
                _ => 1,
            };
        }
    };

    if opts.verbose {
        ctx.output
            .stderr(format!("< HTTP {}\n", response.status).as_bytes());
        for (k, v) in &response.headers {
            ctx.output.stderr(format!("< {k}: {v}\n").as_bytes());
        }
        ctx.output.stderr(b"<\n");
    }

    if opts.fail_on_error && response.status >= 400 {
        if !opts.silent || opts.show_error {
            ctx.output.stderr(
                format!(
                    "curl: (22) The requested URL returned error: {}\n",
                    response.status
                )
                .as_bytes(),
            );
        }
        return 22;
    }

    if opts.head_only {
        // Print response headers
        ctx.output
            .stdout(format!("HTTP {}\r\n", response.status).as_bytes());
        for (k, v) in &response.headers {
            ctx.output.stdout(format!("{k}: {v}\r\n").as_bytes());
        }
        ctx.output.stdout(b"\r\n");
    } else if let Some(ref output_file) = opts.output {
        // Write to file
        let path = resolve_path(ctx.cwd, output_file);
        let h = match ctx.fs.open(&path, OpenOptions::write()) {
            Ok(h) => h,
            Err(e) => {
                ctx.output
                    .stderr(format!("curl: cannot write to '{output_file}': {e}\n").as_bytes());
                return 23;
            }
        };
        if let Err(e) = ctx.fs.write_file(h, &response.body) {
            ctx.output
                .stderr(format!("curl: write error: {e}\n").as_bytes());
            ctx.fs.close(h);
            return 23;
        }
        ctx.fs.close(h);
    } else {
        // Write to stdout
        ctx.output.stdout(&response.body);
    }

    // Handle --write-out
    if let Some(ref fmt) = opts.write_out {
        let out = fmt.replace("%{http_code}", &response.status.to_string());
        ctx.output.stdout(out.as_bytes());
    }

    0
}

// ── wget ────────────────────────────────────────────────────────

struct WgetOpts {
    url: Option<String>,
    output: Option<String>,
    quiet: bool,
    headers: Vec<(String, String)>,
}

fn parse_wget_args(argv: &[&str]) -> Result<WgetOpts, String> {
    let mut opts = WgetOpts {
        url: None,
        output: None,
        quiet: false,
        headers: Vec::new(),
    };

    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i];
        match arg {
            "-O" | "--output-document" => {
                i += 1;
                opts.output = Some(
                    argv.get(i)
                        .ok_or("-O requires a filename argument")?
                        .to_string(),
                );
            }
            "-q" | "--quiet" => opts.quiet = true,
            "--header" => {
                i += 1;
                let header = *argv.get(i).ok_or("--header requires an argument")?;
                if let Some((k, v)) = header.split_once(':') {
                    opts.headers
                        .push((k.trim().to_string(), v.trim().to_string()));
                } else {
                    return Err(format!("invalid header format: {header}"));
                }
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                // Handle combined flags like -qO-
                let flags = &arg[1..];
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
                            j = flags.len();
                            continue;
                        }
                        _ => {
                            return Err(format!("unknown option: -{}", flags.as_bytes()[j] as char))
                        }
                    }
                    j += 1;
                }
            }
            _ => {
                if opts.url.is_none() {
                    opts.url = Some(arg.to_string());
                } else {
                    return Err(format!("unexpected argument: {arg}"));
                }
            }
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

pub(crate) fn util_wget(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let opts = match parse_wget_args(argv) {
        Ok(o) => o,
        Err(e) => {
            ctx.output.stderr(format!("wget: {e}\n").as_bytes());
            return 2;
        }
    };

    let Some(url) = &opts.url else {
        ctx.output.stderr(b"wget: missing URL\n");
        return 2;
    };
    let url = url.clone();

    let Some(backend) = ctx.network else {
        ctx.output.stderr(b"wget: network access not available\n");
        return 1;
    };

    let request = HttpRequest {
        url: url.clone(),
        method: "GET".into(),
        headers: opts.headers.clone(),
        body: None,
        follow_redirects: true,
    };

    if !opts.quiet {
        ctx.output.stderr(format!("--  {url}\n").as_bytes());
    }

    let response = match backend.fetch(&request) {
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

    // Determine output target
    let write_stdout = opts.output.as_deref() == Some("-");

    if write_stdout {
        ctx.output.stdout(&response.body);
    } else {
        let filename = opts
            .output
            .clone()
            .unwrap_or_else(|| filename_from_url(&url));
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
    }

    0
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net_types::{HostAllowlist, HttpResponse, NetworkBackend, NetworkError};
    use crate::VecOutput;
    use wasmsh_fs::MemoryFs;

    struct MockNetworkBackend {
        allowlist: HostAllowlist,
        response: HttpResponse,
    }

    impl NetworkBackend for MockNetworkBackend {
        fn fetch(&self, request: &HttpRequest) -> Result<HttpResponse, NetworkError> {
            self.allowlist.check(&request.url)?;
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

    // ── curl tests ──────────────────────────────────────────────

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
    fn curl_head_only() {
        let backend = mock_backend(b"body");
        let (status, output) = run_curl(&["curl", "-I", "http://example.com/"], &backend);
        assert_eq!(status, 0);
        assert!(output.stdout_str().contains("HTTP 200"));
        assert!(output.stdout_str().contains("Content-Type: text/plain"));
        assert!(!output.stdout_str().contains("body"));
    }

    #[test]
    fn curl_silent() {
        let backend = mock_backend(b"data");
        let (status, output) = run_curl(&["curl", "-s", "http://example.com/"], &backend);
        assert_eq!(status, 0);
        assert_eq!(output.stdout_str(), "data");
    }

    #[test]
    fn curl_output_file() {
        let backend = mock_backend(b"file content");
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
            util_curl(
                &mut ctx,
                &["curl", "-o", "/out.txt", "http://example.com/f"],
            )
        };
        assert_eq!(status, 0);
        assert!(output.stdout.is_empty()); // nothing on stdout
        let h = fs.open("/out.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        assert_eq!(data, b"file content");
    }

    #[test]
    fn curl_fail_on_error() {
        let backend = MockNetworkBackend {
            allowlist: HostAllowlist::new(vec!["example.com".into()]),
            response: HttpResponse {
                status: 404,
                headers: vec![],
                body: b"not found".to_vec(),
            },
        };
        let (status, output) = run_curl(&["curl", "-f", "http://example.com/missing"], &backend);
        assert_eq!(status, 22);
        assert!(String::from_utf8_lossy(&output.stderr).contains("404"));
    }

    #[test]
    fn curl_write_out_http_code() {
        let backend = mock_backend(b"ok");
        let (status, output) = run_curl(
            &["curl", "-s", "-w", "%{http_code}", "http://example.com/"],
            &backend,
        );
        assert_eq!(status, 0);
        assert!(output.stdout_str().ends_with("200"));
    }

    #[test]
    fn curl_verbose() {
        let backend = mock_backend(b"data");
        let (status, output) = run_curl(&["curl", "-v", "http://example.com/api"], &backend);
        assert_eq!(status, 0);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("> GET http://example.com/api"));
        assert!(stderr.contains("< HTTP 200"));
    }

    #[test]
    fn curl_combined_flags() {
        let backend = mock_backend(b"data");
        let (status, output) = run_curl(&["curl", "-sSL", "http://example.com/"], &backend);
        assert_eq!(status, 0);
        assert_eq!(output.stdout_str(), "data");
    }

    #[test]
    fn curl_post_with_data() {
        let backend = mock_backend(b"ok");
        let (status, _) = run_curl(
            &["curl", "-d", "key=value", "http://example.com/api"],
            &backend,
        );
        assert_eq!(status, 0);
    }

    // ── wget tests ──────────────────────────────────────────────

    #[test]
    fn wget_no_network() {
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
            util_wget(&mut ctx, &["wget", "http://example.com/file.txt"])
        };
        assert_eq!(status, 1);
        assert!(String::from_utf8_lossy(&output.stderr).contains("network access not available"));
    }

    #[test]
    fn wget_no_url() {
        let backend = mock_backend(b"");
        let (status, output, _) = run_wget(&["wget"], &backend);
        assert_eq!(status, 2);
        assert!(String::from_utf8_lossy(&output.stderr).contains("missing URL"));
    }

    #[test]
    fn wget_download_to_file() {
        let backend = mock_backend(b"downloaded data");
        let (status, _, mut fs) = run_wget(&["wget", "http://example.com/report.csv"], &backend);
        assert_eq!(status, 0);
        let h = fs.open("/report.csv", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        assert_eq!(data, b"downloaded data");
    }

    #[test]
    fn wget_output_to_stdout() {
        let backend = mock_backend(b"stdout data");
        let (status, output, _) =
            run_wget(&["wget", "-qO", "-", "http://example.com/data"], &backend);
        assert_eq!(status, 0);
        assert_eq!(output.stdout_str(), "stdout data");
    }

    #[test]
    fn wget_output_to_named_file() {
        let backend = mock_backend(b"named file");
        let (status, _, mut fs) = run_wget(
            &["wget", "-O", "/tmp/out.txt", "http://example.com/data"],
            &backend,
        );
        assert_eq!(status, 0);
        let h = fs.open("/tmp/out.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        assert_eq!(data, b"named file");
    }

    #[test]
    fn wget_quiet() {
        let backend = mock_backend(b"data");
        let (status, output, _) =
            run_wget(&["wget", "-q", "-O", "-", "http://example.com/"], &backend);
        assert_eq!(status, 0);
        assert!(output.stderr.is_empty());
    }

    #[test]
    fn wget_host_denied() {
        let backend = mock_backend(b"");
        let (status, output, _) = run_wget(&["wget", "http://evil.com/file"], &backend);
        assert_eq!(status, 1);
        assert!(String::from_utf8_lossy(&output.stderr).contains("denied"));
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

    #[test]
    fn wget_server_error() {
        let backend = MockNetworkBackend {
            allowlist: HostAllowlist::new(vec!["example.com".into()]),
            response: HttpResponse {
                status: 500,
                headers: vec![],
                body: b"error".to_vec(),
            },
        };
        let (status, output, _) = run_wget(&["wget", "http://example.com/broken"], &backend);
        assert_eq!(status, 8);
        assert!(String::from_utf8_lossy(&output.stderr).contains("500"));
    }
}
