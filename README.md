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

**Shell syntax**: Pipelines, and/or lists, `if/elif/else`, `while/until/for/case`, C-style `for (( ))`, functions, subshells, command substitution `$(...)`, arithmetic `$((...))`, `(( ))` standalone, `[[ ]]` conditional expressions, all parameter expansion operators, glob/brace/tilde expansion, extended globbing (`extglob`), globstar (`**`), here-docs, here-strings, redirections including `2>` and `&>`, `set -euo pipefail`, `trap EXIT`, `break/continue`, `local`, indexed and associative arrays, full arithmetic operator set

**Builtins** (35): `echo`, `printf`, `test`/`[`, `[[`, `read`, `cd`, `pwd`, `export`, `unset`, `readonly`, `set`, `shift`, `return`, `exit`, `eval`, `source`/`.`, `trap`, `type`, `command`, `builtin`, `getopts`, `local`, `break`, `continue`, `declare`/`typeset`, `let`, `alias`/`unalias`, `shopt`, `mapfile`/`readarray`, `:`/`true`/`false`

**Utilities** (86): `cat`, `ls`, `mkdir`, `rm`, `touch`, `mv`, `cp`, `ln`, `head`, `tail`, `wc`, `grep`, `sed`, `sort`, `uniq`, `cut`, `tr`, `tee`, `paste`, `rev`, `column`, `bat`, `xargs`, `seq`, `find`, `stat`, `basename`, `dirname`, `readlink`, `realpath`, `chmod`, `mktemp`, `date`, `sleep`, `env`, `printenv`, `expr`, `id`, `whoami`, `uname`, `hostname`, `yes`, `md5sum`, `sha256sum`, `sha1sum`, `sha512sum`, `base64`, `which`, `rmdir`, `tac`, `nl`, `shuf`, `cmp`, `comm`, `fold`, `nproc`, `expand`, `unexpand`, `truncate`, `factor`, `cksum`, `tsort`, `install`, `timeout`, `cal`, `diff`, `patch`, `tree`, `rg`, `fd`, `awk`, `jq`, `yq`, `bc`, `xxd`, `dd`, `strings`, `split`, `file`, `tar`, `gzip`, `gunzip`, `zcat`, `unzip`, `du`, `df`

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

960 tests across two layers:

- **506 Rust unit/integration tests** (400 in wasmsh-utils alone) including property-based fuzzing via proptest
- **454 TOML declarative test cases** covering shell semantics, utility behavior, and 60 real-world production script patterns (CI/CD, log analysis, ETL pipelines, deployment automation, etc.)
- **Criterion benchmarks** for parser, expansion, and pipeline performance

## License

[MIT](LICENSE)
