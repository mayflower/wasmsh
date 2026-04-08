# Adding a New Builtin Command

How to implement a new shell builtin in wasmsh.

> **Note**: For a more complete walkthrough that covers all three layers
> (runtime intercept, builtin, utility) and explains how to choose between
> them, see [Adding a command](adding-commands.md). This page is the
> narrow recipe for the builtin layer.

## Step 1: Write the Implementation

In `crates/wasmsh-builtins/src/lib.rs`, add a function with the `BuiltinFn` signature:

```rust
fn builtin_mycommand(ctx: &mut BuiltinContext<'_>, argv: &[&str]) -> i32 {
    // argv[0] is the command name
    // ctx.state gives access to shell state (variables, cwd, etc.)
    // ctx.output.stdout() / ctx.output.stderr() for output
    // ctx.fs for VFS access (Option<&MemoryFs>)
    // ctx.stdin for pipe/heredoc input (Option<&[u8]>)
    // Return value is the exit status

    ctx.output.stdout(b"hello from mycommand\n");
    0
}
```

## Step 2: Register It

In the `BuiltinRegistry::new()` method, add:

```rust
builtins.insert("mycommand", builtin_mycommand);
```

## Step 3: Add the Feature Gate

In `crates/wasmsh-testkit/src/features.rs`, add:

```rust
f.insert("mycommand");
```

## Step 4: Write Tests

Create `tests/suite/builtins/mycommand.toml`:

```toml
[test]
name = "mycommand-basic"
tags = ["builtin"]
requires = ["mycommand"]

[input]
script = "mycommand"

[expect]
status = 0
stdout = "hello from mycommand\n"
```

## Step 5: Special Runtime Handling

If your builtin affects control flow (like `break`, `exit`, `eval`) or
must be intercepted before normal dispatch (like `declare`, `let`,
`shopt`, `alias`, `source`, `mapfile`, `builtin`), intercept it in
`crates/wasmsh-runtime/src/lib.rs` near the other `CMD_*` constants and
the dispatch site rather than registering it as a builtin. See
[Adding a command](adding-commands.md#recipe-3-add-a-runtime-intercept)
for the full recipe.

## Available Context

The `BuiltinContext` provides:

- `state: &mut ShellState` — variables, positional params, cwd, exit status
- `output: &mut dyn OutputSink` — stdout/stderr
- `fs: Option<&MemoryFs>` — virtual filesystem (for `test -f`, etc.)
- `stdin: Option<&[u8]>` — piped/heredoc input data
