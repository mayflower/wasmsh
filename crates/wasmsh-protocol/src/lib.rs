//! Message protocol for wasmsh host adapters.
//!
//! Defines versioned, serializable messages exchanged between a host and
//! `wasmsh-runtime`, including the progressive `StartRun` / `PollRun`
//! execution flow in addition to one-shot `Run`.
//!
//! An experimental typed WIT projection of the same surface lives in
//! `crates/wasmsh-protocol/wit/worker-protocol.wit`. The serde enums remain
//! the canonical contract today; the WIT world is additive and intended for
//! future component-model embedders.
//!
//! ## Protocol version
//! Current version: `0.1.0`

#![warn(missing_docs)]

/// Protocol version string.
pub const PROTOCOL_VERSION: &str = "0.1.0";

/// A command sent from the host to the worker.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum HostCommand {
    /// Initialize the shell runtime with optional configuration.
    Init {
        /// Maximum step budget per execution (0 = unlimited).
        step_budget: u64,
        /// Hostnames/IPs allowed for network access (empty = no network).
        ///
        /// Patterns: exact host (`api.example.com`), wildcard (`*.example.com`),
        /// IP (`192.168.1.100`), host with port (`api.example.com:8080`).
        #[serde(default)]
        allowed_hosts: Vec<String>,
    },
    /// Execute a shell command string.
    Run {
        /// The shell source text to execute.
        input: String,
    },
    /// Start a progressive shell execution without polling it to completion.
    StartRun {
        /// The shell source text to execute.
        input: String,
    },
    /// Poll the active progressive execution.
    PollRun,
    /// Cancel the currently running execution.
    Cancel,
    /// Mount a virtual filesystem at the given path.
    Mount {
        /// Absolute path at which to mount the filesystem.
        path: String,
    },
    /// Read a file from the virtual filesystem.
    ReadFile {
        /// Absolute path of the file to read.
        path: String,
    },
    /// Write data to a file in the virtual filesystem.
    WriteFile {
        /// Absolute path of the file to write.
        path: String,
        /// Raw bytes to write into the file.
        data: Vec<u8>,
    },
    /// List directory contents.
    ListDir {
        /// Absolute path of the directory to list.
        path: String,
    },
}

/// An event sent from the worker to the host.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum WorkerEvent {
    /// Shell produced stdout output.
    Stdout(Vec<u8>),
    /// Shell produced stderr output.
    Stderr(Vec<u8>),
    /// Command execution finished with exit code.
    Exit(i32),
    /// Command execution is still active and needs another poll.
    Yielded,
    /// A diagnostic message (warning, info, trace).
    Diagnostic(DiagnosticLevel, String),
    /// A file in the VFS was changed.
    FsChanged(String),
    /// Protocol version announcement (sent on Init).
    Version(String),
}

/// Diagnostic severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum DiagnosticLevel {
    /// Informational message.
    Info,
    /// Non-fatal warning.
    Warning,
    /// Error-level diagnostic.
    Error,
    /// Low-level trace output.
    Trace,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_version() {
        assert!(!PROTOCOL_VERSION.is_empty());
    }

    #[test]
    fn host_command_variants() {
        let cmd = HostCommand::Run {
            input: "echo hello".into(),
        };
        assert!(matches!(cmd, HostCommand::Run { .. }));

        let cmd = HostCommand::StartRun {
            input: "echo hello".into(),
        };
        assert!(matches!(cmd, HostCommand::StartRun { .. }));

        assert_eq!(HostCommand::PollRun, HostCommand::PollRun);
    }

    #[test]
    fn worker_event_variants() {
        let evt = WorkerEvent::Exit(0);
        assert_eq!(evt, WorkerEvent::Exit(0));

        assert_eq!(WorkerEvent::Yielded, WorkerEvent::Yielded);

        let evt = WorkerEvent::Diagnostic(DiagnosticLevel::Warning, "test".into());
        assert!(matches!(
            evt,
            WorkerEvent::Diagnostic(DiagnosticLevel::Warning, _)
        ));
    }

    #[test]
    fn progressive_commands_roundtrip_json() {
        let start = HostCommand::StartRun {
            input: "echo hello".into(),
        };
        let encoded = serde_json::to_string(&start).unwrap();
        assert_eq!(encoded, r#"{"StartRun":{"input":"echo hello"}}"#);
        let decoded: HostCommand = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, start);

        let encoded = serde_json::to_string(&HostCommand::PollRun).unwrap();
        assert_eq!(encoded, r#""PollRun""#);
        let decoded: HostCommand = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, HostCommand::PollRun);
    }

    #[test]
    fn yielded_event_roundtrips_json() {
        let encoded = serde_json::to_string(&WorkerEvent::Yielded).unwrap();
        assert_eq!(encoded, r#""Yielded""#);
        let decoded: WorkerEvent = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, WorkerEvent::Yielded);
    }
}
