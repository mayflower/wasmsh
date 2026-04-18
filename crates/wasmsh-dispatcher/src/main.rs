use std::env;

use anyhow::{anyhow, Context, Result};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;
use wasmsh_dispatcher::service::{DispatcherService, ServiceConfig};

const SUPPORTED_DISPATCHER_POLICY: &str = "restore-capacity-only";

fn configured_runner_urls() -> Vec<String> {
    if let Ok(value) = env::var("RUNNER_SERVICE_URLS") {
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

    env::var("RUNNER_SERVICE_URL")
        .ok()
        .map(|value| vec![value])
        .unwrap_or_default()
}

fn parse_port() -> Result<u16> {
    match env::var("PORT") {
        Err(_) => Ok(8080),
        Ok(raw) => raw
            .trim()
            .parse::<u16>()
            .with_context(|| format!("PORT={raw:?} is not a valid u16 TCP port")),
    }
}

fn verify_policy() -> Result<()> {
    match env::var("DISPATCHER_POLICY") {
        Err(_) => Ok(()),
        Ok(value) if value == SUPPORTED_DISPATCHER_POLICY => Ok(()),
        Ok(other) => Err(anyhow!(
            "DISPATCHER_POLICY={other:?} is not supported; only {SUPPORTED_DISPATCHER_POLICY:?} is implemented"
        )),
    }
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
