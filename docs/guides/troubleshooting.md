# Troubleshooting

Common errors and how to interpret them. Each entry is keyed on what you
will actually see — a diagnostic message, an exit code, or a behaviour —
followed by what it means and how to fix it.

## Diagnostics

### `Diagnostic(Error, "runtime not initialized")`

You sent `Run` (or another command) before sending `Init`. Send `Init`
once at the start of every session:

```rust
rt.handle_command(HostCommand::Init {
    step_budget: 100_000,
    allowed_hosts: vec![],
});
```

In the JS adapter this happens inside `createNodeSession()` /
`createBrowserWorkerSession()`, so you should never see this from the
adapter directly. If you do, it usually means you constructed a session
yourself rather than using the factory.

### `Diagnostic(Warning, "mount not yet implemented")`

You sent `HostCommand::Mount`. The variant is reserved for future
multi-backend filesystem mounts; the runtime does not yet honour it. Use
`WriteFile` to seed the VFS instead, or compile with the
`wasmsh-runtime/emscripten` feature to swap the entire backend at compile
time.

### `Diagnostic(Warning, "unknown command")`

You sent a `HostCommand` variant the runtime does not recognise. This
usually means a protocol version mismatch — your host is encoding a
newer enum variant than the runtime supports. Update the runtime, or
downgrade the host adapter to match.

### `Diagnostic(Error, "invalid JSON command: …")`

(Pyodide / npm only.) The host sent malformed JSON to
`wasmsh_runtime_handle_json`. Common causes:

- Missing required fields. `Init` requires `step_budget` *and*
  `allowed_hosts` — `serde(default)` makes the latter optional during
  decode but not during a strict round-trip.
- Wrong tagging. `serde_json` uses the default external tagging:
  `{ "Run": { "input": "…" } }`, not `{ "type": "Run", "input": "…" }`.

See [Worker protocol → JSON wire format](../reference/protocol.md#json-wire-format)
for the canonical shapes.

### `Diagnostic(Error, "host not allowed: …")` (`curl` / `wget`)

You ran `curl` or `wget` against a host that is not in `allowed_hosts`.
The runtime denies the request before it touches the network. Add the
host to `allowed_hosts` at `Init` time. Patterns: exact host, wildcard
(`*.example.com`), IP, or host with port.

If you want to deny all network access, leave `allowed_hosts` empty —
that is the safe default. If you accidentally allow nothing and need
debug visibility, the runtime emits a diagnostic on every denied
request, so the failures are noisy and easy to spot.

### `Diagnostic(Warning, "step budget approached")` and `Exit(124)`

The script ran for more steps than `step_budget` allows. The VM stopped
cooperatively at the next instruction boundary. Either:

- Raise `step_budget` at `Init` time (or set `0` to disable, only for
  trusted input).
- Identify and fix the runaway loop in the script — usually a missing
  termination condition.

The budget is per `Run` call, not per session, so a single expensive
script does not affect later commands.

## Exit codes

| Code | Meaning |
|------|---------|
| `0`  | Success |
| `1`  | Generic failure (most failed commands) |
| `2`  | Misuse of a builtin (e.g. `test` syntax error) |
| `124`| Step budget exhausted |
| `127`| Command not found |
| `128 + N` | Killed by signal `N` (in wasmsh, only `130` for SIGINT-equivalent cancellation is used) |
| `130`| Cancelled via `HostCommand::Cancel` |

## Behaviours

### "My pipeline produced no output"

A few likely causes:

- **One stage exited with an error and the rest got no input.** Run the
  stages in isolation to find the offender. `set -o pipefail` makes
  pipeline failures propagate to the final exit code.
- **You're piping into a command that buffers.** The wasmsh pipeline
  model runs each stage to completion before feeding the next, so
  streaming behaviour is not preserved. A `yes | head -n 5` will hit the
  internal output cap on `yes` rather than streaming. See
  [Pipeline execution model](../explanation/design-decisions.md#pipeline-execution-model).
- **You redirected stdout to a file.** Check the VFS with
  `session.readFile("/path/to/file")`.

### "`set -e` doesn't seem to exit on error"

The most common cause: `set -e` does **not** exit when a failed command is
in a pipeline (except the last one), in a conditional context (`if`,
`while`, `||`, `&&`), or when the failure is in a function called from a
condition. This matches bash behaviour.

To trace which command actually failed, enable `set -x` for diagnostic
output:

```sh
set -ex
my_script
```

### "My here-document is being expanded but I want it literal"

Quote the delimiter:

```sh
cat <<'EOF'
literal $variable text
EOF
```

Unquoted delimiters allow `$variable`, `$(...)`, and `\` escapes inside
the body. Quoted delimiters (single or double quotes around the
delimiter token) suppress all expansion.

### "Python 3 isn't available in the standalone build"

`python` and `python3` only exist in the Pyodide build. The standalone
target (`wasm32-unknown-unknown`) has no Python interpreter. To run
Python you need the Pyodide build (the npm package, the PyPI package, or
a custom Pyodide-linked build).

### "I get a different exit code than bash"

For specific gaps, check `SUPPORTED.md` for the relevant builtin or
syntax. wasmsh aims for behavioural parity but several edge cases are
documented divergences. If you find an undocumented divergence,
that's a bug — please file it with a minimal reproducer.

### "`pip install` works but the package can't be imported"

`installPythonPackages` only installs the wheel; it does not import it.
You need to `import` it from a `python3 -c` invocation in the same
session, or write a Python script file and run it. The packages persist
for the lifetime of the session and are reset on the next `Init`.

Also: only pure-Python wheels (`py3-none-any`) are supported. If the
package depends on a native wheel, the install will fail with a
diagnostic. There is no workaround — Pyodide cannot dlopen arbitrary
native code.

### "The runtime is slow on the first call but fast after that"

Pyodide has a multi-megabyte wasm bundle and a Python standard library
that needs to be unpacked on first boot. The first
`createNodeSession()` / `createBrowserWorkerSession()` pays this cost.
Subsequent calls reuse the cached bundle. For benchmarking, always
discard the first call.

If first-call latency is critical, run a tiny `await session.run(":")`
during application startup so the cost is paid before users notice.

### "I want to read host filesystem files"

You can't, and that's a feature. The sandbox has no host FS access. To
get host data into the sandbox:

- Read the file in the host language and call `session.writeFile(...)`.
- For static seed data, pass `initialFiles` to the session factory.
- For long-running sessions, use `session.writeFile()` whenever you need
  to add new data.

## Where to file bugs

If none of the above matches, capture:

- The exact command or `HostCommand` you sent.
- The full event vector you got back.
- The wasmsh version (`session.run("echo $WASMSH_VERSION")` if set, or
  the npm/PyPI package version).

Then open an issue on the wasmsh repository with a minimal reproducer.
The maintainers prefer reproducers as TOML test cases under
`tests/suite/` if the bug is shell-side; the
[Writing tests](../tutorials/writing-tests.md) tutorial covers the
format.

## See Also

- [Worker protocol reference](../reference/protocol.md) for the full
  diagnostic and event vocabulary.
- [Sandbox and capabilities](../reference/sandbox-and-capabilities.md)
  for the security knobs and what they enforce.
- [`SUPPORTED.md`](../../SUPPORTED.md) for the canonical compatibility
  matrix.
