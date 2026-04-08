# Writing Shell Tests for wasmsh

This tutorial walks through writing a TOML conformance test, running it,
and debugging it when it fails. It assumes you have already cloned the
repository and run `cargo test --workspace` once.

## Where tests live

Tests live under `tests/suite/`, organised by feature area:

| Directory                | Contents |
|--------------------------|----------|
| `tests/suite/builtins/`         | Tests for shell builtins (cd, export, declare, …) |
| `tests/suite/utilities/`        | Tests for utilities (cat, grep, awk, …) |
| `tests/suite/utilities_gaps/`   | Failing or partially-supported utility cases |
| `tests/suite/syntax_gaps/`      | Failing or partially-supported syntax cases |
| `tests/suite/arithmetic_gaps/`  | Arithmetic edge cases |
| `tests/suite/lexer/`            | Lexer corner cases |
| `tests/suite/redirections/`     | Redirection variants |
| `tests/suite/options/`          | `set`, `shopt`, `setopt` behaviour |
| `tests/suite/vfs/`              | VFS operations and path semantics |
| `tests/suite/complex/`          | Multi-feature integration tests |
| `tests/suite/real_world/`       | Excerpts from real shell scripts |

If you are testing a feature that already has a folder, add to it. If
your test crosses categories, `complex/` or `real_world/` is usually
right.

## The TOML test format

Every test is a `.toml` file with up to four sections:

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

### `[test]` — metadata

- `name`: descriptive kebab-case identifier (must be unique within the file).
- `tags`: free-form labels for filtering (`real-world`, `pipeline`, `bug-1234`).
- `tier`: priority level. `P0` cases must always pass; `P1` and `P2` are
  expected eventually but may be skipped in fast checks.
- `requires`: feature gates. The runner skips a test if any required
  feature is not in the implemented set
  (`crates/wasmsh-testkit/src/features.rs`). This lets you write tests
  for not-yet-implemented features without breaking CI.

### `[setup]` — pre-execution state

- `files`: VFS files to create before the script runs. Path → contents.
- `env`: environment variables to set in the shell before execution.

### `[input]` — the script

- `script`: shell code to execute. Use a TOML triple-quoted string for
  multi-line scripts.

### `[expect]` — assertions

| Field              | Meaning |
|--------------------|---------|
| `status`           | Expected exit code |
| `stdout`           | Exact expected stdout (string match) |
| `stdout_contains`  | Array of substrings, each of which must appear in stdout |
| `stderr`           | Exact expected stderr |
| `stderr_contains`  | Array of substrings that must appear in stderr |
| `files`            | Map of `path → expected contents` to verify after the script ran |
| `env`              | Map of `var → expected value` to verify after the script ran |

You don't have to use all of them. Just `status` and one of the output
assertions is enough for most tests.

## A worked example

Suppose you're adding support for a new option to `grep`. Create
`tests/suite/utilities/grep_invert_count.toml`:

```toml
[test]
name = "grep-invert-count"
tags = ["utility", "grep"]
tier = "P1"
requires = ["grep", "pipeline"]

[setup]
files = { "/log.txt" = "INFO ok\nERROR fail\nINFO ok\nWARN slow\n" }

[input]
script = "grep -vc INFO /log.txt"

[expect]
status = 0
stdout = "2\n"
```

This test:

- Seeds `/log.txt` in the VFS.
- Runs `grep -vc INFO /log.txt` (count lines that do *not* match INFO).
- Asserts the exit code is 0 and stdout is `2\n`.

## Running tests

The whole TOML suite:

```bash
cargo test -p wasmsh-testkit --test suite_runner
```

A subset by name (nextest filter syntax):

```bash
cargo nextest run -p wasmsh-testkit -E 'test(grep-invert-count)'
```

Without nextest, the legacy filter still works:

```bash
cargo test -p wasmsh-testkit --test suite_runner -- grep-invert-count
```

Add `--nocapture` to see the runner's output for debugging:

```bash
cargo test -p wasmsh-testkit --test suite_runner -- --nocapture grep-invert-count
```

## When a test fails

The runner prints the actual vs expected values for each assertion that
failed. Common failure modes:

| Symptom                                | Likely cause |
|----------------------------------------|--------------|
| `stdout: expected "X\n" got ""`        | The script silently exited before producing output. Add `set -e` and run with `--nocapture` to see the trace, or simplify the script. |
| `stdout: expected "X\n" got "X"`       | Missing trailing newline. Most utilities print one; check whether yours does. |
| `status: expected 0 got 127`           | Command not found. Either the command isn't implemented yet, or the feature gate is missing in `features.rs`. |
| `status: expected 0 got 1` with no stderr | A builtin returned non-zero without an error message. Check the builtin's implementation. |
| Test is skipped                        | One of the `requires` features is not in the implemented set. Either implement it or remove it from the requirements. |

To run the script manually inside a runtime, use the
[Rust embedding tutorial](getting-started.md) — paste the script into a
small embedding harness and inspect the events vector.

## Differential oracle testing

The testkit can compare wasmsh output against a reference shell. This is
how we catch behavioural drift from bash. The oracle runs the same
script under bash (when `WASMSH_ORACLE_BASH=1` is set in the environment)
and reports any differences in stdout, stderr, or exit code.

This is the mechanism behind ADR-0011 (testing via differential
oracles). For most test authoring you don't need to think about it — the
oracle runs in CI on a subset of tests and surfaces divergences for
review.

## E2E-first integration tests

For changes that touch the host bridge (npm package, Pyodide build,
protocol additions), the policy is **end-to-end test first**. Add a
failing E2E test under `e2e/` (`pyodide-node/`, `pyodide-browser/`,
`standalone/`, etc.) before writing the implementation. See ADR-0020 for
the rationale.

## See Also

- [Reference: Builtins](../reference/builtins.md) and
  [Utilities](../reference/utilities.md) — the surfaces you are testing.
- [Adding a command](../guides/adding-commands.md) — how the test fits
  into the broader workflow of adding a new command.
- [Troubleshooting](../guides/troubleshooting.md) — for diagnosing
  runtime errors that surface in tests.
- `crates/wasmsh-testkit/src/features.rs` — the feature gate registry.
