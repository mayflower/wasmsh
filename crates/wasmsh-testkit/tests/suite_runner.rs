//! Integration test: discovers and runs all TOML test cases from tests/suite/.

use std::path::PathBuf;

use wasmsh_testkit::runner::{self, TestOutcome};

fn suite_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/
        .unwrap()
        .parent() // workspace root
        .unwrap()
        .join("tests/suite")
}

#[test]
fn run_all_suite_cases() {
    let dir = suite_dir();
    let cases = runner::discover_cases(&dir);

    if cases.is_empty() {
        eprintln!("WARNING: no TOML test cases found in {}", dir.display());
        return;
    }

    let mut passed = 0u32;
    let mut skipped = 0u32;
    let mut failures: Vec<(String, String)> = Vec::new();

    for path in &cases {
        let rel = path
            .strip_prefix(&dir)
            .unwrap_or(path)
            .display()
            .to_string();

        match runner::run_toml_file(path) {
            TestOutcome::Passed => {
                passed += 1;
            }
            TestOutcome::Skipped { reason } => {
                skipped += 1;
                eprintln!("  SKIP: {rel} — {reason}");
            }
            TestOutcome::Failed { reason } => {
                failures.push((rel, reason));
            }
        }
    }

    eprintln!(
        "\n=== Suite Summary: {} passed, {} skipped, {} failed (of {} total) ===",
        passed,
        skipped,
        failures.len(),
        cases.len()
    );

    if !failures.is_empty() {
        let mut msg = String::from("Test suite failures:\n");
        for (name, reason) in &failures {
            msg.push_str(&format!("\n--- FAIL: {name} ---\n{reason}\n"));
        }
        panic!("{msg}");
    }
}
