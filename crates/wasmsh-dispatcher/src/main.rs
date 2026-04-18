use std::env;

use anyhow::{anyhow, Context, Result};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;
use wasmsh_dispatcher::service::{DispatcherService, ServiceConfig};

const SUPPORTED_DISPATCHER_POLICY: &str = "restore-capacity-only";

fn parse_runner_urls_from(urls_env: Option<&str>, single_env: Option<&str>) -> Vec<String> {
    if let Some(value) = urls_env {
        let urls = value
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if !urls.is_empty() {
            return urls;
        }
    }
    single_env.map(|v| vec![v.to_owned()]).unwrap_or_default()
}

fn configured_runner_urls() -> Vec<String> {
    parse_runner_urls_from(
        env::var("RUNNER_SERVICE_URLS").ok().as_deref(),
        env::var("RUNNER_SERVICE_URL").ok().as_deref(),
    )
}

fn parse_port_from(raw: Option<&str>) -> Result<u16> {
    match raw {
        None => Ok(8080),
        Some(value) => value
            .trim()
            .parse::<u16>()
            .with_context(|| format!("PORT={value:?} is not a valid u16 TCP port")),
    }
}

fn parse_port() -> Result<u16> {
    parse_port_from(env::var("PORT").ok().as_deref())
}

fn verify_policy_value(raw: Option<&str>) -> Result<()> {
    match raw {
        None => Ok(()),
        Some(value) if value == SUPPORTED_DISPATCHER_POLICY => Ok(()),
        Some(other) => Err(anyhow!(
            "DISPATCHER_POLICY={other:?} is not supported; only {SUPPORTED_DISPATCHER_POLICY:?} is implemented"
        )),
    }
}

fn verify_policy() -> Result<()> {
    verify_policy_value(env::var("DISPATCHER_POLICY").ok().as_deref())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    verify_policy()?;

    let port = parse_port()?;
    let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let runner_urls = configured_runner_urls();
    if runner_urls.is_empty() {
        return Err(anyhow!(
            "neither RUNNER_SERVICE_URLS nor RUNNER_SERVICE_URL is set; dispatcher has nothing to route to"
        ));
    }

    let listener = TcpListener::bind(format!("{host}:{port}"))
        .await
        .with_context(|| format!("failed to bind {host}:{port}"))?;
    let service = DispatcherService::new(ServiceConfig { runner_urls })
        .context("failed to build dispatcher service")?;

    info!(%host, port, "wasmsh-dispatcher listening");
    axum::serve(listener, service.router())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum server exited with error")?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install ctrl-c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    info!("shutdown signal received; draining");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_runner_urls_splits_and_trims_comma_list() {
        let urls = parse_runner_urls_from(Some("http://a:1 , http://b:2"), None);
        assert_eq!(
            urls,
            vec!["http://a:1".to_string(), "http://b:2".to_string()]
        );
    }

    #[test]
    fn parse_runner_urls_ignores_empty_entries() {
        let urls = parse_runner_urls_from(Some(",http://a:1,,"), None);
        assert_eq!(urls, vec!["http://a:1".to_string()]);
    }

    #[test]
    fn parse_runner_urls_falls_back_to_single_env() {
        let urls = parse_runner_urls_from(None, Some("http://solo:1"));
        assert_eq!(urls, vec!["http://solo:1".to_string()]);
    }

    #[test]
    fn parse_runner_urls_prefers_plural_when_both_set_and_non_empty() {
        let urls = parse_runner_urls_from(Some("http://a:1"), Some("http://b:2"));
        assert_eq!(urls, vec!["http://a:1".to_string()]);
    }

    #[test]
    fn parse_runner_urls_uses_single_when_plural_is_only_whitespace() {
        let urls = parse_runner_urls_from(Some("  , , "), Some("http://fallback:1"));
        assert_eq!(urls, vec!["http://fallback:1".to_string()]);
    }

    #[test]
    fn parse_runner_urls_empty_when_nothing_set() {
        let urls = parse_runner_urls_from(None, None);
        assert!(urls.is_empty());
    }

    #[test]
    fn parse_port_defaults_to_8080_when_unset() {
        assert_eq!(parse_port_from(None).unwrap(), 8080);
    }

    #[test]
    fn parse_port_accepts_valid_u16() {
        assert_eq!(parse_port_from(Some("9001")).unwrap(), 9001);
    }

    #[test]
    fn parse_port_trims_whitespace() {
        assert_eq!(parse_port_from(Some("  9001  ")).unwrap(), 9001);
    }

    #[test]
    fn parse_port_rejects_non_numeric() {
        let err = parse_port_from(Some("abc")).unwrap_err();
        assert!(err.to_string().contains("PORT=\"abc\""));
    }

    #[test]
    fn parse_port_rejects_out_of_range() {
        let err = parse_port_from(Some("99999")).unwrap_err();
        assert!(err.to_string().contains("PORT=\"99999\""));
    }

    #[test]
    fn verify_policy_accepts_unset() {
        assert!(verify_policy_value(None).is_ok());
    }

    #[test]
    fn verify_policy_accepts_supported_value() {
        assert!(verify_policy_value(Some(SUPPORTED_DISPATCHER_POLICY)).is_ok());
    }

    #[test]
    fn verify_policy_rejects_unknown_value() {
        let err = verify_policy_value(Some("legacy-round-robin")).unwrap_err();
        assert!(err.to_string().contains("legacy-round-robin"));
        assert!(err.to_string().contains("restore-capacity-only"));
    }
}
