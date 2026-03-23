# Adding a New Utility Command

How to implement a new shell utility (external command) in wasmsh.

## Builtins vs Utilities

- **Builtins** modify shell state directly (cd, export, set)
- **Utilities** operate on the VFS and streams (cat, grep, sort)

If your command doesn't need to modify shell variables or control flow, it should be a utility.

## Step 1: Write the Implementation

In `crates/wasmsh-utils/src/lib.rs`, add a function with the `UtilFn` signature:

```rust
fn util_myutil(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    // argv[0] is the command name
    // ctx.fs — MemoryFs for file operations
    // ctx.output — stdout/stderr
    // ctx.cwd — current working directory
    // ctx.stdin — piped input data (Option<&[u8]>)
    // ctx.state — shell state (Option<&ShellState>, for env access)

    let args = &argv[1..];
    if args.is_empty() {
        // Read from stdin if available
        if let Some(data) = ctx.stdin {
            ctx.output.stdout(data);
            return 0;
        }
        ctx.output.stderr(b"myutil: missing operand\n");
        return 1;
    }

    // Process file arguments
    for path in args {
        let full = resolve_path(ctx.cwd, path);
        // ... use ctx.fs.open(), ctx.fs.read_file(), etc.
    }
    0
}
```

## Step 2: Register It

In `UtilRegistry::new()`:

```rust
utils.insert("myutil", util_myutil);
```

## Step 3: Support Stdin

Utilities should work in pipelines. When no file arguments are given, read from `ctx.stdin`:

```rust
if args.is_empty() {
    if let Some(data) = ctx.stdin {
        // Process stdin data
        return 0;
    }
}
```

## Step 4: Test It

Create `tests/suite/utilities/myutil_basic.toml` and add the feature gate in `features.rs`.

## Helper Functions

Use these existing helpers:

- `resolve_path(cwd, path)` — resolve relative paths against cwd
- `read_text(fs, path)` — read a file as a UTF-8 string
- `get_input_text(ctx, file_args)` — read from files or stdin
- `simple_glob_match(pattern, name)` — glob matching for find-like utilities
- `grep_matches(line, pattern, ignore_case)` — pattern matching with `^`/`$` anchors
