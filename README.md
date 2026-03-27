# wasmsh

**A browser-first shell runtime in Rust with Bash-compatible syntax, virtual filesystem, and sandboxed execution.**

[![CI](https://img.shields.io/badge/CI-passing-brightgreen)](.github/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.89+-orange.svg)](https://www.rust-lang.org)

wasmsh is an independent shell implementation — not a port of BusyBox or a fork of Bash. It provides compatible behavior through a clean-room implementation with its own parser, VM, and utility stack.

## Why wasmsh?

- **Dual-target**: Standalone browser Web Worker (`wasm32-unknown-unknown`) or embedded inside Pyodide (`wasm32-unknown-emscripten`) with shared filesystem
- **No OS processes**: All commands execute in-process — builtins, utilities, functions, and `python3`
- **Virtual filesystem**: `MemoryFs` for standalone, `EmscriptenFs` (libc) for Pyodide — shell and Python see the same `/workspace`
- **Bash-compatible syntax**: Supports the shell features real scripts actually use
- **Sandboxed**: Step budgets, output limits, cancellation tokens, capability-gated I/O
- **Clean provenance**: No GPL code — MIT licensed, permissive dependencies only

## Quick Start

```rust
use wasmsh_runtime::WorkerRuntime;
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
git clone https://github.com/mayflower/wasmsh
cd wasmsh
cargo build --workspace    # build the Rust workspace
cargo test --workspace     # run 1379 Rust tests
```

### Requirements

- Rust 1.89+ (pinned via `rust-toolchain.toml`)
- For standalone wasm: `wasm-pack` (`cargo install wasm-pack`)
- For Pyodide: `emcc` (Emscripten SDK), Python 3.13+, `gsed` on macOS

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

Crates with clear boundaries:

| Layer | Crates |
|-------|--------|
| **Syntax** | `wasmsh-lex`, `wasmsh-parse`, `wasmsh-ast` |
| **Semantics** | `wasmsh-expand`, `wasmsh-hir`, `wasmsh-ir` |
| **Execution** | `wasmsh-vm`, `wasmsh-state`, `wasmsh-builtins` |
| **Runtime** | `wasmsh-runtime` (shared core), `wasmsh-protocol` |
| **Platform** | `wasmsh-fs`, `wasmsh-utils` |
| **Standalone** | `wasmsh-browser` (wasm-bindgen Web Worker adapter) |
| **Pyodide** | `wasmsh-pyodide`, `wasmsh-pyodide-probe` (excluded from workspace, require emcc) |
| **Testing** | `wasmsh-testkit` |

## Build Targets

wasmsh supports two build targets from the same `wasmsh-runtime` core:

### Standalone (browser Web Worker)

```bash
just build-standalone     # wasm-pack → e2e/standalone/fixture/pkg/
just test-e2e-standalone  # Playwright browser tests (6 tests)
```

Requirements: Rust 1.89+, `wasm-pack`

### Pyodide (Python + shell same-module)

```bash
just build-pyodide              # custom Pyodide build → dist/pyodide-custom/
just test-e2e-pyodide-node      # Node E2E tests (19 tests)
just test-e2e-pyodide-browser   # Playwright browser tests (4 tests)
```

Requirements: Rust 1.89+, emcc (Emscripten), Python 3.13+, GNU sed (`gsed` on macOS)

Version pins are in `tools/pyodide/versions.env`.

### Troubleshooting

| Problem | Fix |
|---------|-----|
| `emcc not found` | Install Emscripten SDK or run `just build-pyodide` which sets up its own emsdk |
| `gsed not found` | `brew install gnu-sed` (macOS) |
| Python `No module named 'encodings'` | Stdlib zip not mounted — check `build-custom.sh` preRun |
| `MAIN_MODULE=1` export error | Rust mangled symbols with `$` — build uses `MAIN_MODULE=2` |

## Development

```bash
just check    # fmt + clippy + tests (pre-push)
just ci       # full CI locally
just test     # all Rust tests
just coverage # HTML coverage report
just deny     # license/advisory check
```

## Testing

1400+ tests across multiple layers:

- **1379 Rust unit/integration tests** including property-based fuzzing via proptest
- **536 TOML declarative test cases** covering shell semantics, utility behavior, and 60 real-world production script patterns
- **6 Standalone browser E2E tests** (Playwright)
- **19 Pyodide Node E2E tests** (protocol parity, FS sharing, python command)
- **4 Pyodide browser E2E tests** (Playwright)
- **Criterion benchmarks** for parser, expansion, and pipeline performance

## License

[MIT](LICENSE)
