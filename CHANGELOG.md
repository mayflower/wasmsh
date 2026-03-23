# Changelog

All notable changes to wasmsh will be documented in this file.

## [0.1.0] — 2026-03-23

### Added

- **Shell syntax**: Full recursive-descent parser for Bash-compatible syntax
  - Simple commands, pipelines, and/or lists
  - `if/elif/else/fi`, `while/until/for/case` control flow
  - Functions (POSIX and Bash syntax), `local` variables
  - Subshells with scope isolation
  - Here-documents, here-strings
  - All parameter expansion operators (`:-`, `:=`, `:+`, `:?`, `#`, `%`, `/`)
  - Command substitution `$(...)`
  - Arithmetic expansion `$(( ))`
  - Glob, brace, and tilde expansion
  - `$'...'` ANSI-C quoting
  - Stderr redirection (`2>`, `2>&1`, `&>`)

- **18 shell builtins**: echo, printf, test/[, read, cd, pwd, export, unset, readonly, set, shift, eval, source, trap, type, command, getopts, local

- **38 utilities**: cat, ls, mkdir, rm, touch, mv, cp, ln, head, tail, wc, grep, sed, sort, uniq, cut, tr, tee, xargs, seq, find, stat, basename, dirname, readlink, realpath, chmod, date, sleep, env, printenv, expr, id, whoami, uname, hostname

- **Virtual filesystem**: MemoryFs with directories, files, handles, path normalization

- **Pipeline data flow**: Buffered stdout→stdin between pipeline stages

- **Execution controls**: Step budgets, output byte limits, cancellation tokens, `set -e` (errexit), `trap EXIT`

- **Worker protocol**: Versioned host↔worker message protocol with Init, Run, Cancel, ReadFile, WriteFile, ListDir commands

- **Test infrastructure**: 288 Rust tests + 237 TOML declarative test cases including 40 real-world production script patterns

- **Quality tooling**: CI with clippy, rustfmt, cargo-deny, coverage; property-based fuzzing; Diataxis documentation
