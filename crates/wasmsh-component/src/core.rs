//! Host-testable `SessionCore` that owns one initialised [`WorkerRuntime`].
//!
//! This module is target-agnostic: it contains no `wit-bindgen` types and
//! links on every host target the workspace already compiles for. The WIT
//! `session` resource in `src/lib.rs` is a thin, `cfg`-gated wrapper that
//! delegates every method to `SessionCore`.

use wasmsh_protocol::HostCommand;
use wasmsh_runtime::WorkerRuntime;

use crate::event::{events_from_worker, ComponentEvent};

/// Default step budget sent with `HostCommand::Init` when the host does not
/// override it. Chosen to match the browser/Pyodide adapter defaults so that
/// the component surface behaves identically on the same inputs.
pub const DEFAULT_STEP_BUDGET: u64 = 100_000;

/// Component-facing mirror of [`HostCommand::Init`] configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub step_budget: u64,
    pub allowed_hosts: Vec<String>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            step_budget: DEFAULT_STEP_BUDGET,
            allowed_hosts: Vec::new(),
        }
    }
}

/// The host-testable, target-agnostic session adapter.
///
/// One `SessionCore` owns one initialised [`WorkerRuntime`]. Every method is
/// a thin wrapper over a single `HostCommand::*` dispatch that returns the
/// emitted event stream as a list of [`ComponentEvent`] values.
///
/// The wit-bindgen-generated `session` resource in the component export
/// layer holds exactly one of these, which is why session state lives
/// per-instance and not in a process-wide singleton. See ADR-0030.
#[allow(
    missing_debug_implementations,
    reason = "WorkerRuntime does not implement Debug"
)]
pub struct SessionCore {
    runtime: WorkerRuntime,
    /// Events emitted by `HostCommand::Init` that have not yet been observed
    /// by the caller. The first call to [`Self::run`] or
    /// [`Self::take_init_events`] drains this buffer so the `Version`
    /// announcement surfaces exactly once, matching the contract documented
    /// on the WIT `session` constructor.
    pending_init_events: Option<Vec<ComponentEvent>>,
}

impl SessionCore {
    /// Build a new session and dispatch `HostCommand::Init` with the given
    /// configuration. The `Version` event emitted by the runtime is captured
    /// and replayed on the first call to [`Self::run`] (or can be drained
    /// explicitly via [`Self::take_init_events`]) so hosts can observe the
    /// protocol version without a separate round-trip.
    pub fn new(config: RuntimeConfig) -> Self {
        let mut runtime = WorkerRuntime::new();
        let init_events = runtime.handle_command(HostCommand::Init {
            step_budget: config.step_budget,
            allowed_hosts: config.allowed_hosts,
        });
        Self {
            runtime,
            pending_init_events: Some(events_from_worker(init_events)),
        }
    }

    /// Drain any events captured at construction time (the runtime's
    /// `Version` announcement and any setup diagnostics). Returns an empty
    /// vector after they have already been observed, either by a previous
    /// call to this method or by [`Self::run`].
    pub fn take_init_events(&mut self) -> Vec<ComponentEvent> {
        self.pending_init_events.take().unwrap_or_default()
    }

    /// Dispatch a `HostCommand::Run` and return the emitted event batch.
    /// The first call also drains any events captured at construction time
    /// so the protocol `Version` announcement surfaces exactly once.
    pub fn run(&mut self, input: impl Into<String>) -> Vec<ComponentEvent> {
        let mut events = self.pending_init_events.take().unwrap_or_default();
        events.extend(events_from_worker(self.runtime.handle_command(
            HostCommand::Run {
                input: input.into(),
            },
        )));
        events
    }

    /// Dispatch a `HostCommand::ReadFile` and return the emitted event batch.
    pub fn read_file(&mut self, path: impl Into<String>) -> Vec<ComponentEvent> {
        events_from_worker(
            self.runtime
                .handle_command(HostCommand::ReadFile { path: path.into() }),
        )
    }

    /// Dispatch a `HostCommand::WriteFile` and return the emitted event batch.
    pub fn write_file(&mut self, path: impl Into<String>, data: Vec<u8>) -> Vec<ComponentEvent> {
        events_from_worker(self.runtime.handle_command(HostCommand::WriteFile {
            path: path.into(),
            data,
        }))
    }

    /// Dispatch a `HostCommand::ListDir` and return the emitted event batch.
    pub fn list_dir(&mut self, path: impl Into<String>) -> Vec<ComponentEvent> {
        events_from_worker(
            self.runtime
                .handle_command(HostCommand::ListDir { path: path.into() }),
        )
    }

    /// Dispatch a `HostCommand::Mount` and return the emitted event batch.
    ///
    /// Mount semantics are intentionally preserved as-is: the runtime treats
    /// mounts as reserved metadata today, and this layer does not add a new
    /// filesystem design.
    pub fn mount(&mut self, path: impl Into<String>) -> Vec<ComponentEvent> {
        events_from_worker(
            self.runtime
                .handle_command(HostCommand::Mount { path: path.into() }),
        )
    }
}

/// Convenience helper for smoke tests and simple hosts: build an ephemeral
/// `SessionCore`, run the input, and return the combined event stream.
///
/// The returned vector contains the `Init` events followed by the `Run`
/// events, so callers see the `Version` announcement first and the final
/// `Exit(status)` last.
pub fn run_once(config: RuntimeConfig, input: impl Into<String>) -> Vec<ComponentEvent> {
    SessionCore::new(config).run(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmsh_protocol::PROTOCOL_VERSION;

    fn stdout_string(events: &[ComponentEvent]) -> String {
        let mut out = Vec::new();
        for event in events {
            if let ComponentEvent::Stdout(data) = event {
                out.extend_from_slice(data);
            }
        }
        String::from_utf8(out).unwrap_or_default()
    }

    fn exit_code(events: &[ComponentEvent]) -> Option<i32> {
        events.iter().find_map(|event| match event {
            ComponentEvent::Exit(status) => Some(*status),
            _ => None,
        })
    }

    fn version_string(events: &[ComponentEvent]) -> Option<String> {
        events.iter().find_map(|event| match event {
            ComponentEvent::Version(version) => Some(version.clone()),
            _ => None,
        })
    }

    #[test]
    fn runtime_config_defaults_to_step_budget_100k_and_no_allowed_hosts() {
        let cfg = RuntimeConfig::default();
        assert_eq!(cfg.step_budget, 100_000);
        assert!(cfg.allowed_hosts.is_empty());
    }

    #[test]
    fn session_constructor_emits_init_version_event() {
        let mut session = SessionCore::new(RuntimeConfig::default());
        let init_events = session.take_init_events();
        assert_eq!(
            version_string(&init_events).as_deref(),
            Some(PROTOCOL_VERSION)
        );
    }

    #[test]
    fn first_run_replays_init_version_event() {
        // Mirrors the contract documented on the WIT `session` constructor:
        // the runtime's initial `Version` event is captured and replayed on
        // the first `run` call so hosts that never call `take_init_events`
        // can still observe the protocol version.
        let mut session = SessionCore::new(RuntimeConfig::default());
        let first = session.run("echo hello");
        assert_eq!(
            version_string(&first).as_deref(),
            Some(PROTOCOL_VERSION),
            "first run should replay the Init version event"
        );
        assert_eq!(stdout_string(&first), "hello\n");

        let second = session.run("echo world");
        assert!(
            version_string(&second).is_none(),
            "subsequent runs must not replay the version event again: {second:?}"
        );
        assert_eq!(stdout_string(&second), "world\n");
    }

    #[test]
    fn take_init_events_suppresses_replay_in_subsequent_run() {
        // Hosts that drain init events explicitly should not also see them
        // duplicated in the first `run` call.
        let mut session = SessionCore::new(RuntimeConfig::default());
        let taken = session.take_init_events();
        assert_eq!(version_string(&taken).as_deref(), Some(PROTOCOL_VERSION));

        let events = session.run("echo hi");
        assert!(
            version_string(&events).is_none(),
            "version should not surface again after take_init_events: {events:?}"
        );
        assert_eq!(stdout_string(&events), "hi\n");
    }

    #[test]
    fn run_maps_to_exit_zero_for_echo() {
        let mut session = SessionCore::new(RuntimeConfig::default());
        session.take_init_events();
        let events = session.run("echo hello");
        assert_eq!(stdout_string(&events), "hello\n");
        assert_eq!(exit_code(&events), Some(0));
    }

    #[test]
    fn write_then_read_roundtrips_bytes() {
        let mut session = SessionCore::new(RuntimeConfig::default());
        let _ = session.write_file("/hello.txt", b"world".to_vec());
        let read_events = session.read_file("/hello.txt");
        assert_eq!(stdout_string(&read_events), "world");
    }

    #[test]
    fn list_dir_returns_expected_entries() {
        let mut session = SessionCore::new(RuntimeConfig::default());
        let _ = session.write_file("/a.txt", Vec::new());
        let _ = session.write_file("/b.txt", Vec::new());
        let listing = stdout_string(&session.list_dir("/"));
        assert!(listing.contains("a.txt"), "listing: {listing:?}");
        assert!(listing.contains("b.txt"), "listing: {listing:?}");
    }

    #[test]
    fn mount_preserves_current_reserved_behavior() {
        let mut session = SessionCore::new(RuntimeConfig::default());
        // Mounts are reserved metadata today. The runtime is free to emit
        // diagnostics but must not crash and must not return an exit event.
        let events = session.mount("/mnt/example");
        assert!(
            exit_code(&events).is_none(),
            "mount should not emit an exit event: {events:?}"
        );
    }

    #[test]
    fn run_once_builds_ephemeral_session_and_executes() {
        let events = run_once(RuntimeConfig::default(), "echo hi");
        assert_eq!(
            version_string(&events).as_deref(),
            Some(PROTOCOL_VERSION),
            "run_once should surface the Init version event first"
        );
        assert_eq!(stdout_string(&events), "hi\n");
        assert_eq!(exit_code(&events), Some(0));
    }
}
