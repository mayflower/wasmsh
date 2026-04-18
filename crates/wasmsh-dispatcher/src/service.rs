use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::lookup_host;
use tokio::sync::Mutex;
use tracing::warn;
use uuid::Uuid;

use crate::{DispatchError, DispatchRequest, Dispatcher, RunnerId, RunnerSnapshot};

/// Maximum JSON request body accepted by the dispatcher control plane.
///
/// The dispatcher is intended to run behind a trusted mesh — there is no
/// user-facing auth layer here — but we still cap bodies so a misbehaving
/// caller can't exhaust runner memory with a single oversized `initial_files`
/// payload.  8 MiB covers a worst-case base64 seeded-file blob while staying
/// well below the runner's own per-session quota.
const MAX_REQUEST_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Per-request timeout for upstream calls to a runner.
const UPSTREAM_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// TCP connect timeout for upstream calls to a runner.
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
/// Static configuration for the dispatcher service.
pub struct ServiceConfig {
    /// Base URLs of runner instances that this dispatcher may route to.
    pub runner_urls: Vec<String>,
}

#[derive(Debug, Clone)]
/// Constructible dispatcher service that exposes the external session API.
pub struct DispatcherService {
    state: Arc<AppState>,
}

#[derive(Debug)]
struct AppState {
    config: ServiceConfig,
    client: Client,
    routing: Mutex<RoutingState>,
}

#[derive(Debug, Default)]
struct RoutingState {
    dispatcher: Dispatcher,
    runner_urls: HashMap<RunnerId, String>,
}

#[derive(Debug, Deserialize, Serialize)]
/// Inline file seed used during session creation.
///
/// Serialization uses `camelCase` (`contentBase64`) because the runner's
/// wire format expects that key; the inbound deserializer also accepts
/// `snake_case` (`content_base64`) via a serde alias so existing callers
/// written against the Rust-native naming still work.
pub struct InitialFile {
    /// Absolute in-sandbox path.
    pub path: String,
    /// File contents encoded as base64.
    #[serde(rename = "contentBase64", alias = "content_base64")]
    pub content_base64: String,
}

#[derive(Debug, Deserialize, Serialize)]
/// Request payload for creating a new session through the dispatcher.
pub struct CreateSessionRequest {
    #[serde(default)]
    /// Optional caller-provided stable session id used for affinity.
    pub session_id: Option<String>,
    #[serde(default)]
    /// Network allowlist forwarded to the runner.
    pub allowed_hosts: Vec<String>,
    #[serde(default)]
    /// Per-execution step budget forwarded to the runtime.
    pub step_budget: u64,
    #[serde(default)]
    /// Initial files to seed before the first command runs.
    pub initial_files: Vec<InitialFile>,
}

#[derive(Debug, Deserialize, Serialize)]
/// Request payload for `run`.
pub struct RunRequest {
    /// Shell command string to execute.
    pub command: String,
}

#[derive(Debug, Deserialize, Serialize)]
/// Generic path-only request payload.
pub struct PathRequest {
    /// Absolute path in the sandbox filesystem.
    pub path: String,
}

#[derive(Debug, Deserialize, Serialize)]
/// Request payload for `write-file`.
pub struct WriteFileRequest {
    /// Absolute in-sandbox target path.
    pub path: String,
    /// File contents encoded as base64.
    #[serde(rename = "contentBase64", alias = "content_base64")]
    pub content_base64: String,
}

#[derive(Debug, Deserialize)]
struct RunnerSnapshotEnvelope {
    runner: RunnerSnapshot,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunnerCreateSessionRequest<'a> {
    session_id: &'a str,
    allowed_hosts: &'a [String],
    step_budget: u64,
    initial_files: &'a [InitialFile],
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunnerPathRequest {
    path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunnerWriteFileRequest {
    path: String,
    content_base64: String,
}

#[derive(Debug, Serialize)]
struct ReadyResponse {
    ok: bool,
    ready: bool,
    healthy_runners: usize,
}

#[derive(Debug, thiserror::Error)]
enum ServiceError {
    #[error("no healthy runner has free restore capacity")]
    NoRunnerAvailable,
    #[error("unknown session: {0}")]
    UnknownSession(String),
    #[error("runner discovery failed: {0}")]
    Discovery(String),
    #[error("runner request failed: {0}")]
    Upstream(String),
    #[error("runner returned status {status}: {message}")]
    UpstreamStatus { status: StatusCode, message: String },
}

impl IntoResponse for ServiceError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::NoRunnerAvailable | Self::Discovery(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::UnknownSession(_) => StatusCode::NOT_FOUND,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::UpstreamStatus { status, .. } => *status,
        };
        (
            status,
            Json(json!({ "ok": false, "error": self.to_string() })),
        )
            .into_response()
    }
}

impl DispatcherService {
    /// Creates a new dispatcher service instance.
    ///
    /// The inner HTTP client is pre-configured with connect- and
    /// request-timeouts so a hung runner cannot stall `/readyz` or routed
    /// calls indefinitely.
    pub fn new(config: ServiceConfig) -> Result<Self, reqwest::Error> {
        let client = Client::builder()
            .connect_timeout(UPSTREAM_CONNECT_TIMEOUT)
            .timeout(UPSTREAM_REQUEST_TIMEOUT)
            .build()?;
        let state = Arc::new(AppState {
            config,
            client,
            routing: Mutex::new(RoutingState::default()),
        });
        Ok(Self { state })
    }

    /// Builds the Axum router that exposes the dispatcher control-plane API.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/healthz", get(healthz))
            .route("/readyz", get(readyz))
            .route("/sessions", post(create_session))
            .route("/sessions/:session_id", delete(delete_session))
            .route("/sessions/:session_id/init", post(init_session))
            .route("/sessions/:session_id/run", post(run_session))
            .route("/sessions/:session_id/write-file", post(write_file))
            .route("/sessions/:session_id/read-file", post(read_file))
            .route("/sessions/:session_id/list-dir", post(list_dir))
            .route("/sessions/:session_id/close", post(close_session))
            .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
            .with_state(self.state.clone())
    }
}

async fn healthz() -> Json<Value> {
    Json(json!({ "ok": true }))
}

async fn readyz(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ServiceError> {
    let routable_runners = refresh_runners(&state).await?;
    let body = ReadyResponse {
        ok: true,
        ready: routable_runners > 0,
        healthy_runners: routable_runners,
    };
    let status = if body.ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    Ok((status, Json(json!(body))))
}

async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<CreateSessionRequest>,
) -> Result<impl IntoResponse, ServiceError> {
    let session_id = payload
        .session_id
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let runner_url = select_runner_url(&state, &session_id).await?;

    let request = RunnerCreateSessionRequest {
        session_id: &session_id,
        allowed_hosts: &payload.allowed_hosts,
        step_budget: payload.step_budget,
        initial_files: &payload.initial_files,
    };

    let result = forward_post_json(&state.client, &runner_url, "/sessions", &request).await;
    if let Err(ref error) = result {
        warn!(
            session_id = %session_id,
            runner_url = %runner_url,
            error = %error,
            "runner rejected create_session; releasing affinity"
        );
        let mut routing = state.routing.lock().await;
        routing.dispatcher.release_session(&session_id);
    }
    result
}

async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ServiceError> {
    let result = forward_existing_session_delete(&state, &session_id).await?;
    release_session_affinity(&state, &session_id).await;
    Ok(result)
}

async fn close_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ServiceError> {
    let result = forward_existing_session_post(&state, &session_id, "/close", &json!({})).await?;
    release_session_affinity(&state, &session_id).await;
    Ok(result)
}

async fn init_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ServiceError> {
    forward_existing_session_post(&state, &session_id, "/init", &json!({})).await
}

async fn run_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(payload): Json<RunRequest>,
) -> Result<impl IntoResponse, ServiceError> {
    forward_existing_session_post(&state, &session_id, "/run", &payload).await
}

async fn write_file(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(payload): Json<WriteFileRequest>,
) -> Result<impl IntoResponse, ServiceError> {
    let request = RunnerWriteFileRequest {
        path: payload.path,
        content_base64: payload.content_base64,
    };
    forward_existing_session_post(&state, &session_id, "/write-file", &request).await
}

async fn read_file(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(payload): Json<PathRequest>,
) -> Result<impl IntoResponse, ServiceError> {
    let request = RunnerPathRequest { path: payload.path };
    forward_existing_session_post(&state, &session_id, "/read-file", &request).await
}

async fn list_dir(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(payload): Json<PathRequest>,
) -> Result<impl IntoResponse, ServiceError> {
    let request = RunnerPathRequest { path: payload.path };
    forward_existing_session_post(&state, &session_id, "/list-dir", &request).await
}

async fn refresh_runners(state: &Arc<AppState>) -> Result<usize, ServiceError> {
    let mut discovered = Vec::new();
    for base_url in expand_runner_urls(&state.config.runner_urls).await? {
        match fetch_runner_snapshot(&state.client, &base_url).await {
            Ok(snapshot) => {
                discovered.push((snapshot.runner_id.clone(), snapshot, base_url));
            }
            Err(error) => {
                warn!(
                    runner_url = %base_url,
                    error = %error,
                    "runner snapshot fetch failed; runner omitted from this refresh"
                );
            }
        }
    }

    let mut routing = state.routing.lock().await;
    let current_ids = routing.runner_urls.keys().cloned().collect::<HashSet<_>>();
    let discovered_ids = discovered
        .iter()
        .map(|(runner_id, _, _)| runner_id.clone())
        .collect::<HashSet<_>>();

    for runner_id in current_ids.difference(&discovered_ids) {
        routing.dispatcher.remove_runner(runner_id);
        routing.runner_urls.remove(runner_id);
    }

    for (runner_id, snapshot, base_url) in discovered {
        routing.dispatcher.upsert_runner(snapshot);
        routing.runner_urls.insert(runner_id, base_url);
    }

    Ok(routing.dispatcher.routable_runner_count())
}

async fn expand_runner_urls(configured_urls: &[String]) -> Result<Vec<String>, ServiceError> {
    let mut expanded = Vec::new();
    let mut seen = HashSet::new();
    for configured_url in configured_urls {
        let parsed = reqwest::Url::parse(configured_url)
            .map_err(|error| ServiceError::Discovery(error.to_string()))?;
        let host = parsed.host_str().ok_or_else(|| {
            ServiceError::Discovery(format!("runner url has no host: {configured_url}"))
        })?;
        let port = parsed.port_or_known_default().ok_or_else(|| {
            ServiceError::Discovery(format!("runner url has no port: {configured_url}"))
        })?;

        let addresses = lookup_host((host, port))
            .await
            .map_err(|error| ServiceError::Discovery(error.to_string()))?;
        for address in addresses {
            let mut resolved = parsed.clone();
            resolved
                .set_host(Some(&address.ip().to_string()))
                .map_err(|error| {
                    ServiceError::Discovery(format!(
                        "failed to set host for {configured_url}: {error}"
                    ))
                })?;
            resolved.set_port(Some(address.port())).map_err(|()| {
                ServiceError::Discovery(format!("failed to set port for {configured_url}"))
            })?;
            // `Url::to_string` renders an empty path as `/`.  Every downstream
            // caller concatenates a leading-slash path suffix (e.g.
            // `/runner/snapshot`), so we strip the trailing `/` here to avoid
            // producing `http://host:port//runner/snapshot`, which axum would
            // route to 404.
            let resolved_url = resolved.to_string().trim_end_matches('/').to_string();
            if seen.insert(resolved_url.clone()) {
                expanded.push(resolved_url);
            }
        }
    }
    Ok(expanded)
}

async fn fetch_runner_snapshot(
    client: &Client,
    base_url: &str,
) -> Result<RunnerSnapshot, ServiceError> {
    let response = client
        .get(format!("{base_url}/runner/snapshot"))
        .send()
        .await
        .map_err(|error| ServiceError::Discovery(error.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| String::from("failed to read body"));
        return Err(ServiceError::Discovery(format!(
            "snapshot request to {base_url} failed with {status}: {body}"
        )));
    }
    let envelope: RunnerSnapshotEnvelope = response
        .json()
        .await
        .map_err(|error| ServiceError::Discovery(error.to_string()))?;
    Ok(envelope.runner)
}

async fn select_runner_url(
    state: &Arc<AppState>,
    session_id: &str,
) -> Result<String, ServiceError> {
    refresh_runners(state).await?;
    let mut routing = state.routing.lock().await;
    let decision = routing
        .dispatcher
        .dispatch(&DispatchRequest {
            session_id: session_id.to_string(),
        })
        .map_err(|error| match error {
            DispatchError::NoRunnerAvailable => ServiceError::NoRunnerAvailable,
        })?;
    routing
        .runner_urls
        .get(&decision.runner_id)
        .cloned()
        .ok_or_else(|| ServiceError::Discovery("selected runner has no url".to_string()))
}

async fn runner_url_for_existing_session(
    state: &Arc<AppState>,
    session_id: &str,
) -> Result<String, ServiceError> {
    let routing = state.routing.lock().await;
    let runner_id = routing
        .dispatcher
        .affinity_for(session_id)
        .cloned()
        .ok_or_else(|| ServiceError::UnknownSession(session_id.to_string()))?;
    routing
        .runner_urls
        .get(&runner_id)
        .cloned()
        .ok_or_else(|| ServiceError::UnknownSession(session_id.to_string()))
}

async fn release_session_affinity(state: &Arc<AppState>, session_id: &str) {
    let mut routing = state.routing.lock().await;
    routing.dispatcher.release_session(session_id);
}

async fn forward_existing_session_post<T: Serialize>(
    state: &Arc<AppState>,
    session_id: &str,
    suffix: &str,
    payload: &T,
) -> Result<impl IntoResponse, ServiceError> {
    let base_url = runner_url_for_existing_session(state, session_id).await?;
    let path = format!("/sessions/{session_id}{suffix}");
    match forward_post_json(&state.client, &base_url, &path, payload).await {
        Ok(response) => Ok(response),
        Err(ServiceError::UpstreamStatus { status, .. }) if status == StatusCode::NOT_FOUND => {
            release_session_affinity(state, session_id).await;
            Err(ServiceError::UnknownSession(session_id.to_string()))
        }
        Err(error) => Err(error),
    }
}

async fn forward_existing_session_delete(
    state: &Arc<AppState>,
    session_id: &str,
) -> Result<impl IntoResponse, ServiceError> {
    let base_url = runner_url_for_existing_session(state, session_id).await?;
    let response = state
        .client
        .delete(format!("{base_url}/sessions/{session_id}"))
        .send()
        .await
        .map_err(|error| ServiceError::Upstream(error.to_string()))?;
    response_to_json_response(response).await
}

async fn forward_post_json<T: Serialize>(
    client: &Client,
    base_url: &str,
    path: &str,
    payload: &T,
) -> Result<(StatusCode, Json<Value>), ServiceError> {
    let response = client
        .post(format!("{base_url}{path}"))
        .json(payload)
        .send()
        .await
        .map_err(|error| ServiceError::Upstream(error.to_string()))?;
    response_to_json_response(response).await
}

async fn response_to_json_response(
    response: reqwest::Response,
) -> Result<(StatusCode, Json<Value>), ServiceError> {
    let raw_status = response.status().as_u16();
    let status = StatusCode::from_u16(raw_status).unwrap_or_else(|_| {
        warn!(
            raw_status,
            "runner returned invalid HTTP status; coercing to 502 Bad Gateway"
        );
        StatusCode::BAD_GATEWAY
    });
    let value: Value = response
        .json()
        .await
        .map_err(|error| ServiceError::Upstream(error.to_string()))?;
    if status.is_success() {
        Ok((status, Json(value)))
    } else {
        Err(ServiceError::UpstreamStatus {
            status,
            message: value
                .get("error")
                .and_then(Value::as_str)
                .map_or_else(|| value.to_string(), ToString::to_string),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routable_runner_count_excludes_unhealthy_draining_and_full_runners() {
        let mut dispatcher = Dispatcher::new();
        dispatcher.upsert_runner(snapshot("healthy", true, false, 2, 1));
        dispatcher.upsert_runner(snapshot("draining", true, true, 2, 0));
        dispatcher.upsert_runner(snapshot("full", true, false, 1, 1));
        dispatcher.upsert_runner(snapshot("unhealthy", false, false, 2, 0));

        assert_eq!(dispatcher.routable_runner_count(), 1);
    }

    fn snapshot(
        runner_id: &str,
        healthy: bool,
        draining: bool,
        restore_slots: u32,
        inflight_restores: u32,
    ) -> RunnerSnapshot {
        RunnerSnapshot {
            runner_id: RunnerId::from(runner_id),
            restore_slots,
            inflight_restores,
            restore_queue_depth: 0,
            restore_p95_ms: 10.0,
            active_sessions: 0,
            draining,
            healthy,
        }
    }
}
