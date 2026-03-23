//! Differential compatibility test harness.
//!
//! Defines a format for origin-authored compatibility cases and a
//! harness that can compare wasmsh behavior against local reference
//! shells (opt-in, skippable if not available).

/// A single compatibility test case.
#[derive(Debug, Clone)]
pub struct CompatCase {
    /// Descriptive name for the test case.
    pub name: &'static str,
    /// Shell input to execute.
    pub input: &'static str,
    /// Expected exit code (None = don't check).
    pub expected_status: Option<i32>,
    /// Expected stdout content (None = don't check).
    pub expected_stdout: Option<&'static str>,
    /// Whether to compare against reference shells.
    pub compare_with_oracle: bool,
}

/// Result of running a compatibility case.
#[derive(Debug)]
pub struct CompatResult {
    pub name: String,
    pub wasmsh_status: Option<i32>,
    pub wasmsh_stdout: Option<String>,
    pub oracle_status: Option<i32>,
    pub oracle_stdout: Option<String>,
    pub passed: bool,
    pub notes: Vec<String>,
}

/// Run a compatibility case against expected values (no oracle).
pub fn check_case(
    case: &CompatCase,
    actual_status: i32,
    actual_stdout: &str,
) -> CompatResult {
    let mut passed = true;
    let mut notes = Vec::new();

    if let Some(expected) = case.expected_status {
        if actual_status != expected {
            passed = false;
            notes.push(format!(
                "status mismatch: expected {expected}, got {actual_status}"
            ));
        }
    }

    if let Some(expected) = case.expected_stdout {
        if actual_stdout != expected {
            passed = false;
            notes.push(format!(
                "stdout mismatch:\n  expected: {expected:?}\n  got:      {actual_stdout:?}"
            ));
        }
    }

    CompatResult {
        name: case.name.to_string(),
        wasmsh_status: Some(actual_status),
        wasmsh_stdout: Some(actual_stdout.to_string()),
        oracle_status: None,
        oracle_stdout: None,
        passed,
        notes,
    }
}

/// Original compatibility test cases authored for this repository.
pub fn core_cases() -> Vec<CompatCase> {
    vec![
        CompatCase {
            name: "true returns 0",
            input: "true",
            expected_status: Some(0),
            expected_stdout: Some(""),
            compare_with_oracle: true,
        },
        CompatCase {
            name: "false returns 1",
            input: "false",
            expected_status: Some(1),
            expected_stdout: Some(""),
            compare_with_oracle: true,
        },
        CompatCase {
            name: "echo hello",
            input: "echo hello",
            expected_status: Some(0),
            expected_stdout: Some("hello\n"),
            compare_with_oracle: true,
        },
        CompatCase {
            name: "echo multiple args",
            input: "echo hello world",
            expected_status: Some(0),
            expected_stdout: Some("hello world\n"),
            compare_with_oracle: true,
        },
        CompatCase {
            name: "echo -n suppresses newline",
            input: "echo -n hello",
            expected_status: Some(0),
            expected_stdout: Some("hello"),
            compare_with_oracle: true,
        },
        CompatCase {
            name: "colon is no-op",
            input: ":",
            expected_status: Some(0),
            expected_stdout: Some(""),
            compare_with_oracle: true,
        },
        CompatCase {
            name: "variable assignment and echo",
            input: "X=hello; echo $X",
            expected_status: Some(0),
            expected_stdout: Some("hello\n"),
            compare_with_oracle: true,
        },
        CompatCase {
            name: "pipeline exit status",
            input: "true | false",
            expected_status: Some(1),
            expected_stdout: Some(""),
            compare_with_oracle: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_case_passes() {
        let case = CompatCase {
            name: "simple",
            input: "echo hi",
            expected_status: Some(0),
            expected_stdout: Some("hi\n"),
            compare_with_oracle: false,
        };
        let result = check_case(&case, 0, "hi\n");
        assert!(result.passed);
    }

    #[test]
    fn check_case_status_mismatch() {
        let case = CompatCase {
            name: "status",
            input: "false",
            expected_status: Some(1),
            expected_stdout: None,
            compare_with_oracle: false,
        };
        let result = check_case(&case, 0, "");
        assert!(!result.passed);
    }

    #[test]
    fn check_case_stdout_mismatch() {
        let case = CompatCase {
            name: "output",
            input: "echo hi",
            expected_status: None,
            expected_stdout: Some("hi\n"),
            compare_with_oracle: false,
        };
        let result = check_case(&case, 0, "bye\n");
        assert!(!result.passed);
    }

    #[test]
    fn core_cases_not_empty() {
        assert!(!core_cases().is_empty());
    }
}
