//! Oracle comparison: run shell scripts against local reference shells.
//!
//! Enabled via `WASMSH_ORACLE=1` environment variable. Requires `bash`
//! or `busybox ash` to be installed on the host.

use std::process::Command;

/// Result from an oracle shell execution.
#[derive(Debug)]
pub struct OracleResult {
    pub shell: String,
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Run a script against a reference shell. Returns `None` if the shell
/// is not available or oracle mode is disabled.
pub fn run_oracle(script: &str, shell: &str) -> Option<OracleResult> {
    if std::env::var("WASMSH_ORACLE").is_err() {
        return None;
    }

    let output = Command::new(shell).arg("-c").arg(script).output().ok()?;

    Some(OracleResult {
        shell: shell.to_string(),
        status: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// Compare wasmsh output against oracle output.
pub fn compare_oracle(
    wasmsh_status: i32,
    wasmsh_stdout: &str,
    oracle: &OracleResult,
    ignore_stderr: bool,
) -> Vec<String> {
    let mut diffs = Vec::new();

    if wasmsh_status != oracle.status {
        diffs.push(format!(
            "[{}] status: wasmsh={}, oracle={}",
            oracle.shell, wasmsh_status, oracle.status
        ));
    }

    if wasmsh_stdout != oracle.stdout {
        diffs.push(format!(
            "[{}] stdout differs:\n  wasmsh: {:?}\n  oracle: {:?}",
            oracle.shell, wasmsh_stdout, oracle.stdout
        ));
    }

    if !ignore_stderr {
        // stderr comparison is informational, not enforced by default
    }

    diffs
}
