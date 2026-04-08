# Reference

Reference pages document the exact surface of wasmsh — what's in the
language, what's in the standard library, what's in the protocol, and what
the sandbox does and does not allow.

Reference is for looking things up. If you want to learn how to use
wasmsh, start with the [Tutorials](../tutorials/index.md). If you want a
recipe for a specific task, see [How-to Guides](../guides/index.md).

## Language

| Page | Contents |
|------|----------|
| [Shell syntax](shell-syntax.md) | Commands, lists, compound commands, redirections, quoting, expansions, arithmetic, globbing |
| [Builtins](builtins.md) | Every builtin command with flags, arguments, and exit codes |
| [Utilities](utilities.md) | All 88 in-process utilities, grouped by source module |

## Runtime

| Page | Contents |
|------|----------|
| [Worker protocol](protocol.md) | `HostCommand` / `WorkerEvent` definitions, JSON wire format, sequence diagram |
| [Sandbox and capabilities](sandbox-and-capabilities.md) | Step budgets, output limits, network allowlist, deterministic system commands, environment variables |

## See Also

- [`SUPPORTED.md`](../../SUPPORTED.md) for the canonical "what works / what
  doesn't" matrix.
- [ADRs](../adr/) for the long-form design rationale behind these surfaces.
