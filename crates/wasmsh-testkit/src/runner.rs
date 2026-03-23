//! TOML test runner engine.
//!
//! Reads declarative test case files, sets up VFS, executes scripts
//! through `WorkerRuntime`, and compares results against expectations.

use std::path::Path;

use wasmsh_browser::WorkerRuntime;
use wasmsh_protocol::{HostCommand, WorkerEvent};

use crate::features;
use crate::toml_case::TomlTestFile;

/// Outcome of running a single test case.
#[derive(Debug)]
pub enum TestOutcome {
    Passed,
    Failed { reason: String },
    Skipped { reason: String },
}

/// Run a TOML test case from a file path.
pub fn run_toml_file(path: &Path) -> TestOutcome {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            return TestOutcome::Failed {
                reason: format!("cannot read {}: {e}", path.display()),
            };
        }
    };
    let case: TomlTestFile = match toml::from_str(&content) {
        Ok(c) => c,
        Err(e) => {
            return TestOutcome::Failed {
                reason: format!("cannot parse {}: {e}", path.display()),
            };
        }
    };
    run_toml_case(&case)
}

/// Run a parsed TOML test case.
pub fn run_toml_case(case: &TomlTestFile) -> TestOutcome {
    // Feature gate check
    let missing = features::missing_features(&case.test.requires);
    if !missing.is_empty() {
        return TestOutcome::Skipped {
            reason: format!("missing features: {}", missing.join(", ")),
        };
    }

    // Get the script
    let script = match &case.input.script {
        Some(s) => s.clone(),
        None => {
            return TestOutcome::Failed {
                reason: "no script provided".into(),
            };
        }
    };

    // Create runtime and initialize
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 100_000,
    });

    // Set up VFS files
    for (path, content) in &case.setup.files {
        rt.handle_command(HostCommand::WriteFile {
            path: path.clone(),
            data: content.as_bytes().to_vec(),
        });
    }

    // Set up environment variables via shell assignments
    if !case.setup.env.is_empty() {
        let env_script: String = case
            .setup
            .env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        rt.handle_command(HostCommand::Run { input: env_script });
    }

    // Execute the script
    let events = rt.handle_command(HostCommand::Run { input: script });

    // Extract results
    let status = events
        .iter()
        .find_map(|e| {
            if let WorkerEvent::Exit(s) = e {
                Some(*s)
            } else {
                None
            }
        })
        .unwrap_or(-1);

    let stdout = collect_event_data(&events, |e| matches!(e, WorkerEvent::Stdout(_)));
    let stderr = collect_event_data(&events, |e| matches!(e, WorkerEvent::Stderr(_)));

    // Compare against expectations
    let mut failures = Vec::new();

    if let Some(expected_status) = case.expect.status {
        if status != expected_status {
            failures.push(format!("status: expected {expected_status}, got {status}"));
        }
    }

    if let Some(expected_stdout) = &case.expect.stdout {
        if stdout != *expected_stdout {
            failures.push(format!(
                "stdout mismatch:\n  expected: {expected_stdout:?}\n  got:      {stdout:?}"
            ));
        }
    }

    if let Some(contains) = &case.expect.stdout_contains {
        for needle in contains {
            if !stdout.contains(needle.as_str()) {
                failures.push(format!("stdout missing: {needle:?}"));
            }
        }
    }

    if let Some(expected_stderr) = &case.expect.stderr {
        if stderr != *expected_stderr {
            failures.push(format!(
                "stderr mismatch:\n  expected: {expected_stderr:?}\n  got:      {stderr:?}"
            ));
        }
    }

    if let Some(contains) = &case.expect.stderr_contains {
        for needle in contains {
            if !stderr.contains(needle.as_str()) {
                failures.push(format!("stderr missing: {needle:?}"));
            }
        }
    }

    // Verify VFS file contents after execution
    for (path, expected_content) in &case.expect.files {
        let read_events = rt.handle_command(HostCommand::ReadFile { path: path.clone() });
        let file_data = collect_event_data(&read_events, |e| matches!(e, WorkerEvent::Stdout(_)));
        if file_data != *expected_content {
            failures.push(format!(
                "file {path} mismatch:\n  expected: {expected_content:?}\n  got:      {file_data:?}"
            ));
        }
    }

    // Verify environment variables after execution
    for (name, expected_val) in &case.expect.env {
        let check_events = rt.handle_command(HostCommand::Run {
            input: format!("echo ${name}"),
        });
        let actual = collect_event_data(&check_events, |e| matches!(e, WorkerEvent::Stdout(_)));
        let actual_trimmed = actual.trim_end_matches('\n');
        if actual_trimmed != expected_val.as_str() {
            failures.push(format!(
                "env ${name} mismatch: expected {expected_val:?}, got {actual_trimmed:?}"
            ));
        }
    }

    if failures.is_empty() {
        TestOutcome::Passed
    } else {
        TestOutcome::Failed {
            reason: failures.join("\n"),
        }
    }
}

fn collect_event_data<F>(events: &[WorkerEvent], pred: F) -> String
where
    F: Fn(&WorkerEvent) -> bool,
{
    let mut buf = Vec::new();
    for e in events {
        if pred(e) {
            match e {
                WorkerEvent::Stdout(data) | WorkerEvent::Stderr(data) => {
                    buf.extend_from_slice(data);
                }
                _ => {}
            }
        }
    }
    String::from_utf8(buf).unwrap_or_default()
}

/// Discover all `.toml` test case files under a directory.
pub fn discover_cases(dir: &Path) -> Vec<std::path::PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "toml") {
                    out.push(path);
                }
            }
        }
    }

    let mut cases = Vec::new();
    if !dir.exists() {
        return cases;
    }
    walk(dir, &mut cases);
    cases.sort();
    cases
}
