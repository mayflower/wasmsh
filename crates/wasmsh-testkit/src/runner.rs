//! TOML test runner engine.
//!
//! Reads declarative test case files, sets up VFS, executes scripts
//! through `WorkerRuntime`, and compares results against expectations.

use std::path::Path;

use wasmsh_protocol::{HostCommand, WorkerEvent};
use wasmsh_runtime::WorkerRuntime;

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
    let missing = features::missing_features(&case.test.requires);
    if !missing.is_empty() {
        return TestOutcome::Skipped {
            reason: format!("missing features: {}", missing.join(", ")),
        };
    }

    let Some(script) = case.input.script.clone() else {
        return TestOutcome::Failed {
            reason: "no script provided".into(),
        };
    };

    let mut rt = new_runtime();
    seed_files(&mut rt, case);
    seed_env(&mut rt, case);
    let events = rt.handle_command(HostCommand::Run { input: script });
    let status = extract_exit_status(&events);
    let stdout = collect_event_data(&events, |e| matches!(e, WorkerEvent::Stdout(_)));
    let stderr = collect_event_data(&events, |e| matches!(e, WorkerEvent::Stderr(_)));

    let mut failures = Vec::new();
    compare_status(case, status, &mut failures);
    compare_stream(
        "stdout",
        &stdout,
        case.expect.stdout.as_ref(),
        &mut failures,
    );
    compare_contains(
        "stdout",
        &stdout,
        case.expect.stdout_contains.as_ref(),
        &mut failures,
    );
    compare_stream(
        "stderr",
        &stderr,
        case.expect.stderr.as_ref(),
        &mut failures,
    );
    compare_contains(
        "stderr",
        &stderr,
        case.expect.stderr_contains.as_ref(),
        &mut failures,
    );
    compare_files(case, &mut rt, &mut failures);
    compare_env(case, &mut rt, &mut failures);

    if failures.is_empty() {
        TestOutcome::Passed
    } else {
        TestOutcome::Failed {
            reason: failures.join("\n"),
        }
    }
}

fn new_runtime() -> WorkerRuntime {
    let mut rt = WorkerRuntime::new();
    rt.handle_command(HostCommand::Init {
        step_budget: 100_000,
    });
    rt
}

fn seed_files(rt: &mut WorkerRuntime, case: &TomlTestFile) {
    for (path, content) in &case.setup.files {
        rt.handle_command(HostCommand::WriteFile {
            path: path.clone(),
            data: content.as_bytes().to_vec(),
        });
    }
}

fn seed_env(rt: &mut WorkerRuntime, case: &TomlTestFile) {
    if case.setup.env.is_empty() {
        return;
    }
    let env_script = case
        .setup
        .env
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ");
    rt.handle_command(HostCommand::Run { input: env_script });
}

fn extract_exit_status(events: &[WorkerEvent]) -> i32 {
    events
        .iter()
        .find_map(|event| match event {
            WorkerEvent::Exit(status) => Some(*status),
            _ => None,
        })
        .unwrap_or(-1)
}

fn compare_status(case: &TomlTestFile, status: i32, failures: &mut Vec<String>) {
    if let Some(expected_status) = case.expect.status {
        if status != expected_status {
            failures.push(format!("status: expected {expected_status}, got {status}"));
        }
    }
}

fn compare_stream(
    label: &str,
    actual: &str,
    expected: Option<&String>,
    failures: &mut Vec<String>,
) {
    let Some(expected) = expected else {
        return;
    };
    if actual != expected {
        failures.push(format!(
            "{label} mismatch:\n  expected: {expected:?}\n  got:      {actual:?}"
        ));
    }
}

fn compare_contains(
    label: &str,
    actual: &str,
    expected: Option<&Vec<String>>,
    failures: &mut Vec<String>,
) {
    let Some(expected) = expected else {
        return;
    };
    for needle in expected {
        if !actual.contains(needle.as_str()) {
            failures.push(format!("{label} missing: {needle:?}"));
        }
    }
}

fn compare_files(case: &TomlTestFile, rt: &mut WorkerRuntime, failures: &mut Vec<String>) {
    for (path, expected_content) in &case.expect.files {
        let read_events = rt.handle_command(HostCommand::ReadFile { path: path.clone() });
        let file_data = collect_event_data(&read_events, |e| matches!(e, WorkerEvent::Stdout(_)));
        if file_data != *expected_content {
            failures.push(format!(
                "file {path} mismatch:\n  expected: {expected_content:?}\n  got:      {file_data:?}"
            ));
        }
    }
}

fn compare_env(case: &TomlTestFile, rt: &mut WorkerRuntime, failures: &mut Vec<String>) {
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
