//! Security tests for the network allowlist.
//!
//! Verifies that `curl` and `wget` can only reach hosts explicitly listed
//! in the `allowed_hosts` configuration.  Uses real HTTP requests to
//! mayflower.de as the allowed host.
//!
//! These tests require network access and will be skipped if
//! `mayflower.de` is unreachable.

use std::io::Read;

use wasmsh_browser::WorkerRuntime;
use wasmsh_protocol::{HostCommand, WorkerEvent};
use wasmsh_utils::net_types::{
    HostAllowlist, HttpRequest, HttpResponse, NetworkBackend, NetworkError,
};

/// Native network backend using `ureq` for integration testing.
/// Validates URLs against a `HostAllowlist` before making real HTTP requests.
struct NativeNetworkBackend {
    allowlist: HostAllowlist,
}

impl NativeNetworkBackend {
    fn new(allowed_hosts: Vec<String>) -> Self {
        Self {
            allowlist: HostAllowlist::new(allowed_hosts),
        }
    }
}

impl NetworkBackend for NativeNetworkBackend {
    fn fetch(&self, request: &HttpRequest) -> Result<HttpResponse, NetworkError> {
        self.allowlist.check(&request.url)?;

        let ureq_req = ureq::request(&request.method, &request.url);
        let mut req = ureq_req;
        for (key, value) in &request.headers {
            req = req.set(key, value);
        }

        let result = if let Some(ref body) = request.body {
            req.send_bytes(body)
        } else {
            req.call()
        };

        match result {
            Ok(resp) => {
                let status = resp.status();
                let mut headers = Vec::new();
                for name in resp.headers_names() {
                    if let Some(value) = resp.header(&name) {
                        headers.push((name, value.to_string()));
                    }
                }
                let mut body = Vec::new();
                resp.into_reader()
                    .take(10 * 1024 * 1024) // 10 MB limit
                    .read_to_end(&mut body)
                    .unwrap_or(0);
                Ok(HttpResponse {
                    status,
                    headers,
                    body,
                })
            }
            Err(ureq::Error::Status(status, resp)) => {
                let mut body = Vec::new();
                resp.into_reader()
                    .take(1024 * 1024)
                    .read_to_end(&mut body)
                    .unwrap_or(0);
                Ok(HttpResponse {
                    status,
                    headers: vec![],
                    body,
                })
            }
            Err(e) => Err(NetworkError::ConnectionFailed(e.to_string())),
        }
    }
}

fn extract_stdout(events: &[WorkerEvent]) -> String {
    let mut out = Vec::new();
    for event in events {
        if let WorkerEvent::Stdout(data) = event {
            out.extend_from_slice(data);
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn extract_stderr(events: &[WorkerEvent]) -> String {
    let mut out = Vec::new();
    for event in events {
        if let WorkerEvent::Stderr(data) = event {
            out.extend_from_slice(data);
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn extract_exit_code(events: &[WorkerEvent]) -> Option<i32> {
    for event in events {
        if let WorkerEvent::Exit(code) = event {
            return Some(*code);
        }
    }
    None
}

/// Check if mayflower.de is reachable (skip tests if offline).
fn mayflower_reachable() -> bool {
    ureq::get("https://mayflower.de")
        .set("User-Agent", "wasmsh-test/1.0")
        .call()
        .is_ok()
}

fn init_runtime_with_network(allowed_hosts: Vec<String>) -> WorkerRuntime {
    let mut rt = WorkerRuntime::new();
    let backend = NativeNetworkBackend::new(allowed_hosts.clone());
    rt.set_network_backend(Box::new(backend));
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts,
    });
    rt
}

// ── Allowed host tests ──────────────────────────────────────────

#[test]
fn curl_allowed_host_succeeds() {
    if !mayflower_reachable() {
        eprintln!("SKIP: mayflower.de unreachable");
        return;
    }

    let mut rt = init_runtime_with_network(vec!["mayflower.de".into()]);
    let events = rt.handle_command(HostCommand::Run {
        input: "curl -sL https://mayflower.de".into(),
    });

    let stdout = extract_stdout(&events);
    let exit_code = extract_exit_code(&events).unwrap();

    assert_eq!(exit_code, 0, "curl to allowed host should succeed");
    assert!(
        !stdout.is_empty(),
        "curl to allowed host should return content"
    );
    assert!(
        stdout.contains("<!") || stdout.contains("<html") || stdout.contains("<HTML"),
        "expected HTML from mayflower.de, got: {}...",
        &stdout[..stdout.len().min(200)]
    );
}

#[test]
fn wget_allowed_host_succeeds() {
    if !mayflower_reachable() {
        eprintln!("SKIP: mayflower.de unreachable");
        return;
    }

    let mut rt = init_runtime_with_network(vec!["mayflower.de".into()]);
    let events = rt.handle_command(HostCommand::Run {
        input: "wget -qO - https://mayflower.de".into(),
    });

    let stdout = extract_stdout(&events);
    let exit_code = extract_exit_code(&events).unwrap();

    assert_eq!(exit_code, 0, "wget to allowed host should succeed");
    assert!(
        !stdout.is_empty(),
        "wget to allowed host should return content"
    );
}

// ── Denied host tests ───────────────────────────────────────────

#[test]
fn curl_denied_host_blocked() {
    let mut rt = init_runtime_with_network(vec!["mayflower.de".into()]);
    let events = rt.handle_command(HostCommand::Run {
        input: "curl https://example.com".into(),
    });

    let stderr = extract_stderr(&events);
    let exit_code = extract_exit_code(&events).unwrap();

    assert_ne!(exit_code, 0, "curl to denied host must fail");
    assert!(
        stderr.contains("denied"),
        "stderr should mention 'denied', got: {stderr}"
    );
}

#[test]
fn wget_denied_host_blocked() {
    let mut rt = init_runtime_with_network(vec!["mayflower.de".into()]);
    let events = rt.handle_command(HostCommand::Run {
        input: "wget -qO - https://example.com".into(),
    });

    let stderr = extract_stderr(&events);
    let exit_code = extract_exit_code(&events).unwrap();

    assert_ne!(exit_code, 0, "wget to denied host must fail");
    assert!(
        stderr.contains("denied"),
        "stderr should mention 'denied', got: {stderr}"
    );
}

#[test]
fn curl_denied_host_with_subdomain() {
    // Only mayflower.de is allowed, not subdomains
    let mut rt = init_runtime_with_network(vec!["mayflower.de".into()]);
    let events = rt.handle_command(HostCommand::Run {
        input: "curl https://evil.mayflower.de".into(),
    });

    let exit_code = extract_exit_code(&events).unwrap();
    assert_ne!(
        exit_code, 0,
        "curl to subdomain of allowed host must fail (exact match only)"
    );
}

#[test]
fn curl_denied_similar_hostname() {
    let mut rt = init_runtime_with_network(vec!["mayflower.de".into()]);

    for host in [
        "https://notmayflower.de",
        "https://mayflower.de.evil.com",
        "https://mayflower.com",
    ] {
        let events = rt.handle_command(HostCommand::Run {
            input: format!("curl {host}"),
        });
        let exit_code = extract_exit_code(&events).unwrap();
        assert_ne!(exit_code, 0, "curl to '{host}' must be blocked");
    }
}

// ── Wildcard pattern tests ──────────────────────────────────────

#[test]
fn curl_wildcard_allows_subdomains() {
    if !mayflower_reachable() {
        eprintln!("SKIP: mayflower.de unreachable");
        return;
    }

    // *.mayflower.de should allow www.mayflower.de and bare mayflower.de
    let mut rt = init_runtime_with_network(vec!["*.mayflower.de".into()]);
    let events = rt.handle_command(HostCommand::Run {
        input: "curl -sL https://mayflower.de".into(),
    });

    let exit_code = extract_exit_code(&events).unwrap();
    assert_eq!(
        exit_code, 0,
        "curl to mayflower.de should succeed with *.mayflower.de pattern"
    );

    // But example.com is still blocked
    let events = rt.handle_command(HostCommand::Run {
        input: "curl https://example.com".into(),
    });
    let exit_code = extract_exit_code(&events).unwrap();
    assert_ne!(exit_code, 0, "example.com must still be blocked");
}

// ── Empty allowlist tests ───────────────────────────────────────

#[test]
fn curl_empty_allowlist_blocks_everything() {
    let mut rt = init_runtime_with_network(vec![]);
    let events = rt.handle_command(HostCommand::Run {
        input: "curl https://mayflower.de".into(),
    });

    let stderr = extract_stderr(&events);
    let exit_code = extract_exit_code(&events).unwrap();

    assert_ne!(exit_code, 0, "empty allowlist must block all requests");
    assert!(
        stderr.contains("denied") || stderr.contains("allowlist"),
        "stderr should mention denial: {stderr}"
    );
}

// ── No network backend tests ────────────────────────────────────

#[test]
fn curl_no_backend_returns_error() {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 0,
        allowed_hosts: vec![],
    });
    let events = rt.handle_command(HostCommand::Run {
        input: "curl https://mayflower.de".into(),
    });

    let stderr = extract_stderr(&events);
    let exit_code = extract_exit_code(&events).unwrap();

    assert_ne!(exit_code, 0);
    assert!(
        stderr.contains("network access not available"),
        "should report no network access: {stderr}"
    );
}

// ── curl output to file ─────────────────────────────────────────

#[test]
fn curl_output_to_file_allowed_host() {
    if !mayflower_reachable() {
        eprintln!("SKIP: mayflower.de unreachable");
        return;
    }

    let mut rt = init_runtime_with_network(vec!["mayflower.de".into()]);

    let events = rt.handle_command(HostCommand::Run {
        input: "curl -sLo /tmp/mayflower.html https://mayflower.de".into(),
    });
    let exit_code = extract_exit_code(&events).unwrap();
    assert_eq!(exit_code, 0, "curl -o to allowed host should succeed");

    let events = rt.handle_command(HostCommand::Run {
        input: "wc -c /tmp/mayflower.html".into(),
    });
    let stdout = extract_stdout(&events);
    let byte_count: usize = stdout
        .trim()
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    assert!(
        byte_count > 100,
        "downloaded file should have substantial content, got {byte_count} bytes"
    );
}

// ── curl write-out ──────────────────────────────────────────────

#[test]
fn curl_write_out_http_code() {
    if !mayflower_reachable() {
        eprintln!("SKIP: mayflower.de unreachable");
        return;
    }

    let mut rt = init_runtime_with_network(vec!["mayflower.de".into()]);
    let events = rt.handle_command(HostCommand::Run {
        input: "curl -sL -o /dev/null -w '%{http_code}' https://mayflower.de".into(),
    });

    let stdout = extract_stdout(&events);
    let exit_code = extract_exit_code(&events).unwrap();

    assert_eq!(exit_code, 0);
    assert!(
        stdout.contains("200"),
        "expected HTTP 200 from mayflower.de, got: {stdout}"
    );
}

// ── Pipeline: curl | wc ─────────────────────────────────────────

#[test]
fn curl_pipe_to_shell_command() {
    if !mayflower_reachable() {
        eprintln!("SKIP: mayflower.de unreachable");
        return;
    }

    let mut rt = init_runtime_with_network(vec!["mayflower.de".into()]);

    let events = rt.handle_command(HostCommand::Run {
        input: "curl -sL https://mayflower.de | wc -l".into(),
    });

    let stdout = extract_stdout(&events);
    let exit_code = extract_exit_code(&events).unwrap();

    assert_eq!(exit_code, 0);
    let line_count: usize = stdout.trim().parse().unwrap_or(0);
    assert!(
        line_count > 5,
        "expected multiple lines from mayflower.de, got {line_count}"
    );
}
