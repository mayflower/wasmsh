# Writing Shell Tests for wasmsh

This tutorial shows you how to write declarative TOML test cases that verify shell behavior.

## The TOML Test Format

Every test is a `.toml` file in `tests/suite/` with four sections:

```toml
[test]
name = "my-test-name"
tags = ["category", "P0"]
tier = "P0"
requires = ["echo", "pipeline"]

[setup]
files = { "/data/input.txt" = "line1\nline2\n" }
env = { HOME = "/home/user" }

[input]
script = """
cat /data/input.txt | wc -l
"""

[expect]
status = 0
stdout = "2\n"
```

## Section by Section

### `[test]` — Metadata

- `name`: Descriptive kebab-case identifier
- `tags`: Categories for filtering (`real-world`, `builtin`, `pipeline`)
- `tier`: Priority level (`P0` = core, `P1` = standard, `P2` = extended)
- `requires`: Feature gates — test is skipped if any feature is missing

### `[setup]` — Pre-execution State

- `files`: VFS files to create before the script runs
- `env`: Environment variables to set

### `[input]` — The Script

- `script`: Shell code to execute (multiline with `"""`)

### `[expect]` — Assertions

- `status`: Expected exit code
- `stdout`: Exact expected stdout
- `stdout_contains`: Partial matches (array of strings that must appear)
- `stderr`: Exact expected stderr
- `stderr_contains`: Partial stderr matches
- `files`: Expected VFS file contents after execution
- `env`: Expected environment variable values after execution

## Running Tests

```bash
# Run all TOML suite tests
cargo test -p wasmsh-testkit --test suite_runner

# With output details
cargo test -p wasmsh-testkit --test suite_runner -- --nocapture
```

## Example: Testing a Pipeline

```toml
[test]
name = "grep-count-pipeline"
tags = ["pipeline", "grep"]
requires = ["grep", "pipeline", "echo"]

[setup]
files = { "/log.txt" = "ERROR fail\nINFO ok\nERROR bad\n" }

[input]
script = "grep -c ERROR /log.txt"

[expect]
status = 0
stdout = "2\n"
```

## Feature Gates

If your test uses a feature that might not be implemented yet, add it to `requires`. The test runner checks against the feature registry and skips gracefully:

```toml
requires = ["glob-expansion", "brace-expansion"]
```

See `crates/wasmsh-testkit/src/features.rs` for the complete feature list.
