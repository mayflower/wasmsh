use std::path::PathBuf;

use wasmsh_testkit::runner::{self, TestOutcome};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn prompt_09_invariant_suite_cases_pass() {
    let root = workspace_root();
    for relative in [
        "tests/suite/redirections/r21_stdout_redirect_then_stderr_merge.toml",
        "tests/suite/pipelines/p11_buffered_pipeline_oracle.toml",
    ] {
        let path = root.join(relative);
        match runner::run_toml_file(&path) {
            TestOutcome::Passed => {}
            TestOutcome::Skipped { reason } => {
                panic!("unexpected skip for {}: {reason}", path.display())
            }
            TestOutcome::Failed { reason } => {
                panic!("invariant case {} failed:\n{reason}", path.display())
            }
        }
    }
}
