# wasmsh

**A browser-first shell runtime in Rust with Bash-compatible syntax, virtual filesystem, and sandboxed execution.**

[![CI](https://img.shields.io/badge/CI-passing-brightgreen)](.github/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.75+-orange.svg)](https://www.rust-lang.org)

wasmsh is an independent shell implementation — not a port of BusyBox or a fork of Bash. It provides compatible behavior through a clean-room implementation with its own parser, VM, and utility stack.

## Why wasmsh?

- **Browser-first**: Compiles to `wasm32-unknown-unknown`, runs in a Web Worker
- **No OS processes**: All commands execute in-process — builtins, utilities, and functions
- **Virtual filesystem**: MemoryFS for ephemeral sessions, OPFS adapter for persistence
- **Bash-compatible syntax**: Supports the shell features real scripts actually use
- **Sandboxed**: Step budgets, output limits, cancellation tokens, capability-gated I/O
- **Clean provenance**: No GPL code — MIT licensed, permissive dependencies only

## Quick Start

```rust
use wasmsh_browser::WorkerRuntime;
use wasmsh_protocol::HostCommand;

let mut rt = WorkerRuntime::new();
rt.handle_command(HostCommand::Init { step_budget: 100_000 });

let events = rt.handle_command(HostCommand::Run {
    input: "echo hello world".into(),
});
// events contains: [Stdout(b"hello world\n"), Exit(0)]
```

## What Works

wasmsh supports a broad subset of Bash syntax and BusyBox-style utilities:

**Shell syntax**: Pipelines, and/or lists, `if/elif/else`, `while/until/for/case`, functions, subshells, command substitution `$(...)`, arithmetic `$((...))`, all parameter expansion operators, glob/brace/tilde expansion, here-docs, here-strings, redirections including `2>` and `&>`, `set -e`, `trap EXIT`, `break/continue`, `local`

**Builtins** (18): `echo`, `printf`, `test`/`[`, `read`, `cd`, `pwd`, `export`, `unset`, `readonly`, `set`, `shift`, `eval`, `source`, `trap`, `type`, `command`, `getopts`, `local`

**Utilities** (38): `cat`, `ls`, `mkdir`, `rm`, `touch`, `mv`, `cp`, `ln`, `head`, `tail`, `wc`, `grep`, `sed`, `sort`, `uniq`, `cut`, `tr`, `tee`, `xargs`, `seq`, `find`, `stat`, `basename`, `dirname`, `readlink`, `realpath`, `chmod`, `date`, `sleep`, `env`, `printenv`, `expr`, `id`, `whoami`, `uname`, `hostname`, and more

See [SUPPORTED.md](SUPPORTED.md) for the complete feature matrix.

## Installation

```bash
# From source
git clone https://github.com/user/wasmsh
cd wasmsh
cargo build --workspace

# Run tests
cargo test --workspace
```

### Requirements

- Rust 1.75+ (pinned via `rust-toolchain.toml`)
- For wasm: `rustup target add wasm32-unknown-unknown`

## Documentation

| Section | Description |
|---------|-------------|
| [Tutorials](docs/tutorials/) | Step-by-step guides to get started |
| [How-to Guides](docs/guides/) | Task-oriented recipes for common operations |
| [Reference](docs/reference/) | Shell syntax, builtins, utilities, protocol |
| [Explanation](docs/explanation/) | Architecture, design decisions, trade-offs |
| [ADRs](docs/adr/) | Architectural Decision Records |
| [Supported Features](SUPPORTED.md) | Complete syntax and command matrix |

## Architecture

```
source → lexer → parser → AST → HIR → IR → VM → builtins/utilities → VFS → protocol events
```

14 crates with clear boundaries:

| Layer | Crates |
|-------|--------|
| **Syntax** | `wasmsh-lex`, `wasmsh-parse`, `wasmsh-ast` |
| **Semantics** | `wasmsh-expand`, `wasmsh-hir`, `wasmsh-ir` |
| **Execution** | `wasmsh-vm`, `wasmsh-state`, `wasmsh-builtins` |
| **Platform** | `wasmsh-fs`, `wasmsh-utils` |
| **Embedding** | `wasmsh-browser`, `wasmsh-protocol` |
| **Testing** | `wasmsh-testkit` |

## Development

```bash
just check    # fmt + clippy + tests (pre-push)
just ci       # full CI locally
just test     # all tests
just coverage # HTML coverage report
just deny     # license/advisory check
```

## Testing

525 tests across two layers:

- **288 Rust unit/integration tests** including property-based fuzzing
- **237 TOML declarative test cases** covering shell semantics, utility behavior, and 40 real-world production script patterns (CI/CD, log analysis, ETL pipelines, deployment automation, etc.)

## License

[MIT](LICENSE)
