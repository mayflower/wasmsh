//! Network capability types for wasmsh utilities.
//!
//! Provides a `NetworkBackend` trait that embedding layers implement to give
//! `curl` and `wget` utilities controlled HTTP access.  URL validation against
//! an allowlist happens in Rust before any network call leaves WASM.

use std::fmt;
use url::Url;

/// An HTTP request to be executed by the host.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// Fully-qualified URL (e.g. `https://api.example.com/data`).
    pub url: String,
    /// HTTP method (GET, POST, HEAD, PUT, DELETE, PATCH).
    pub method: String,
    /// Request headers as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// Optional request body.
    pub body: Option<Vec<u8>>,
    /// Whether to follow HTTP 3xx redirects.
    pub follow_redirects: bool,
}

/// An HTTP response returned by the host.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    /// HTTP status code (e.g. 200, 404).
    pub status: u16,
    /// Response headers as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// Response body bytes.
    pub body: Vec<u8>,
}

/// Errors from network operations.
#[derive(Debug)]
pub enum NetworkError {
    /// The target host is not in the allowlist.
    HostDenied(String),
    /// The connection could not be established.
    ConnectionFailed(String),
    /// The request timed out.
    Timeout(String),
    /// The URL could not be parsed.
    InvalidUrl(String),
    /// Any other network error.
    Other(String),
}

impl fmt::Display for NetworkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HostDenied(msg) => write!(f, "host denied: {msg}"),
            Self::ConnectionFailed(msg) => write!(f, "connection failed: {msg}"),
            Self::Timeout(msg) => write!(f, "timeout: {msg}"),
            Self::InvalidUrl(msg) => write!(f, "invalid URL: {msg}"),
            Self::Other(msg) => write!(f, "network error: {msg}"),
        }
    }
}

/// Trait for performing HTTP requests from utility commands.
///
/// Implementations are provided by the embedding layer:
/// - Standalone browser: wasm-bindgen → synchronous `XMLHttpRequest` in Web Worker
/// - Pyodide: Emscripten FFI → synchronous `XMLHttpRequest` in Web Worker
/// - Tests: mock backend with canned responses
///
/// The `fetch` method is synchronous because all utilities run synchronously.
pub trait NetworkBackend {
    /// Execute an HTTP request and return the response.
    ///
    /// Implementations must validate the URL against the host allowlist
    /// before performing any network I/O.
    fn fetch(&self, request: &HttpRequest) -> Result<HttpResponse, NetworkError>;
}

/// Validated set of allowed hosts for network access.
///
/// Supports exact hostnames, wildcard subdomains (`*.example.com`),
/// IP addresses, and host-with-port (`api.example.com:8080`).
/// An empty allowlist denies all requests.
#[derive(Debug, Clone)]
pub struct HostAllowlist {
    patterns: Vec<AllowPattern>,
}

#[derive(Debug, Clone)]
enum AllowPattern {
    /// Exact hostname match (optionally with port).
    Exact { host: String, port: Option<u16> },
    /// Wildcard suffix match: `*.example.com`.
    WildcardSuffix { suffix: String, port: Option<u16> },
}

impl HostAllowlist {
    /// Create a new allowlist from pattern strings.
    #[must_use]
    pub fn new(patterns: Vec<String>) -> Self {
        let parsed = patterns
            .into_iter()
            .map(|p| {
                let (host_part, port) = Self::split_port(&p);
                if let Some(suffix) = host_part.strip_prefix("*.") {
                    AllowPattern::WildcardSuffix {
                        suffix: suffix.to_ascii_lowercase(),
                        port,
                    }
                } else {
                    AllowPattern::Exact {
                        host: host_part.to_ascii_lowercase(),
                        port,
                    }
                }
            })
            .collect();
        Self { patterns: parsed }
    }

    fn split_port(s: &str) -> (&str, Option<u16>) {
        // Handle *.example.com:8080 or api.example.com:8080
        if let Some(colon_pos) = s.rfind(':') {
            let after = &s[colon_pos + 1..];
            if let Ok(port) = after.parse::<u16>() {
                return (&s[..colon_pos], Some(port));
            }
        }
        (s, None)
    }

    /// Check if a URL's host is allowed. Returns `Ok(())` on success,
    /// or `Err(NetworkError::HostDenied)` if the host is not in the list.
    pub fn check(&self, url: &str) -> Result<(), NetworkError> {
        if self.patterns.is_empty() {
            return Err(NetworkError::HostDenied(
                "no hosts allowed (empty allowlist)".into(),
            ));
        }

        let parsed = Url::parse(url).map_err(|e| NetworkError::InvalidUrl(e.to_string()))?;

        let host = parsed
            .host_str()
            .ok_or_else(|| NetworkError::InvalidUrl("URL has no host".into()))?
            .to_ascii_lowercase();
        let port = parsed.port();

        for pattern in &self.patterns {
            match pattern {
                AllowPattern::Exact {
                    host: allowed,
                    port: allowed_port,
                } => {
                    if host == *allowed && Self::port_matches(*allowed_port, port) {
                        return Ok(());
                    }
                }
                AllowPattern::WildcardSuffix {
                    suffix,
                    port: allowed_port,
                } => {
                    let matches_suffix = host == *suffix || host.ends_with(&format!(".{suffix}"));
                    if matches_suffix && Self::port_matches(*allowed_port, port) {
                        return Ok(());
                    }
                }
            }
        }

        Err(NetworkError::HostDenied(format!(
            "host '{host}' not in allowlist"
        )))
    }

    fn port_matches(allowed_port: Option<u16>, actual_port: Option<u16>) -> bool {
        match allowed_port {
            None => true, // pattern has no port constraint → any port ok
            Some(ap) => actual_port == Some(ap),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_allowlist_denies_all() {
        let al = HostAllowlist::new(vec![]);
        assert!(al.check("https://example.com").is_err());
    }

    #[test]
    fn exact_host_match() {
        let al = HostAllowlist::new(vec!["api.example.com".into()]);
        assert!(al.check("https://api.example.com/path").is_ok());
        assert!(al.check("http://api.example.com:443/path").is_ok());
        assert!(al.check("https://other.example.com").is_err());
        assert!(al.check("https://example.com").is_err());
    }

    #[test]
    fn exact_host_case_insensitive() {
        let al = HostAllowlist::new(vec!["API.Example.COM".into()]);
        assert!(al.check("https://api.example.com/path").is_ok());
    }

    #[test]
    fn wildcard_subdomain() {
        let al = HostAllowlist::new(vec!["*.example.com".into()]);
        assert!(al.check("https://api.example.com/path").is_ok());
        assert!(al.check("https://deep.sub.example.com").is_ok());
        assert!(al.check("https://example.com").is_ok()); // bare domain also matches
        assert!(al.check("https://notexample.com").is_err());
    }

    #[test]
    fn ip_address() {
        let al = HostAllowlist::new(vec!["192.168.1.100".into()]);
        assert!(al.check("http://192.168.1.100/data").is_ok());
        assert!(al.check("http://192.168.1.101/data").is_err());
    }

    #[test]
    fn host_with_port() {
        let al = HostAllowlist::new(vec!["localhost:8080".into()]);
        assert!(al.check("http://localhost:8080/api").is_ok());
        assert!(al.check("http://localhost:9090/api").is_err());
        assert!(al.check("http://localhost/api").is_err());
    }

    #[test]
    fn wildcard_with_port() {
        let al = HostAllowlist::new(vec!["*.internal.co:9090".into()]);
        assert!(al.check("http://api.internal.co:9090/x").is_ok());
        assert!(al.check("http://api.internal.co:8080/x").is_err());
    }

    #[test]
    fn invalid_url() {
        let al = HostAllowlist::new(vec!["example.com".into()]);
        assert!(matches!(
            al.check("not a url"),
            Err(NetworkError::InvalidUrl(_))
        ));
    }

    #[test]
    fn multiple_patterns() {
        let al = HostAllowlist::new(vec![
            "api.example.com".into(),
            "*.internal.co".into(),
            "10.0.0.1".into(),
        ]);
        assert!(al.check("https://api.example.com/a").is_ok());
        assert!(al.check("https://svc.internal.co/b").is_ok());
        assert!(al.check("http://10.0.0.1/c").is_ok());
        assert!(al.check("https://evil.com").is_err());
    }
}
