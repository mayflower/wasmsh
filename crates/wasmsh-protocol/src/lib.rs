//! Message protocol for the wasmsh browser worker bridge.
//!
//! Defines versioned, serializable messages exchanged between the host
//! page and the wasmsh Web Worker.
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
    },
    /// Execute a shell command string.
    Run {
        /// The shell source text to execute.
        input: String,
    },
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
    }

    #[test]
    fn worker_event_variants() {
        let evt = WorkerEvent::Exit(0);
        assert_eq!(evt, WorkerEvent::Exit(0));

        let evt = WorkerEvent::Diagnostic(DiagnosticLevel::Warning, "test".into());
        assert!(matches!(
            evt,
            WorkerEvent::Diagnostic(DiagnosticLevel::Warning, _)
        ));
    }
}
