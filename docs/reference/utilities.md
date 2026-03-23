# Utility Command Reference

Utilities operate on the virtual filesystem and streams. They run in-process with no OS calls.

## File Operations

| Command | Description | Key Flags |
|---------|-------------|-----------|
| `cat` file... | Concatenate and print files. Reads stdin if no args. | |
| `cp` src dst | Copy a file | |
| `mv` src dst | Move (rename) a file | |
| `rm` file... | Remove files | |
| `touch` file... | Create empty files or update timestamps | |
| `mkdir` dir... | Create directories (auto-creates parents) | |
| `ln` src dst | Create a link (copy-based in VFS) | |
| `chmod` mode file | No-op (VFS has no permission model) | |
| `ls` [dir] | List directory contents | |
| `stat` file | Display file metadata (size, type) | |
| `find` [dir] [-name pat] [-type f\|d] | Find files recursively | `-name` with glob, `-type f/d` |
| `readlink` path | Print canonical path | |
| `realpath` path | Print canonical absolute path | |

## Text Processing

| Command | Description | Key Flags |
|---------|-------------|-----------|
| `grep` [opts] pattern [file...] | Search for pattern | `-i` ignore case, `-v` invert, `-c` count, `-n` line numbers |
| `sed` expr [file] | Stream editor | `s/pat/rep/[g]` substitution |
| `sort` [opts] [file] | Sort lines | `-n` numeric, `-r` reverse |
| `uniq` [opts] [file] | Deduplicate adjacent lines | `-c` prefix with count |
| `cut` -d delim -f fields [file] | Extract fields | `-d` delimiter, `-f` field numbers |
| `tr` set1 set2 | Translate characters | `-d` delete characters |
| `head` [-n N] [file] | First N lines (default 10) | |
| `tail` [-n N\|+N] [file] | Last N lines, or from line N | `-n +N` start from line N |
| `wc` [file...] | Count lines, words, bytes | Reads stdin if no args |
| `tee` [-a] [file...] | Copy stdin to stdout and files | `-a` append |
| `xargs` [cmd] | Build arguments from stdin | Default command: `echo` |

## Data Utilities

| Command | Description |
|---------|-------------|
| `seq` [start] end | Generate number sequence |
| `expr` arg... | Evaluate arithmetic expression (`+`, `-`, `*`, `/`, `%`, `=`, `!=`) |
| `basename` path [suffix] | Extract filename from path |
| `dirname` path | Extract directory from path |
| `date` | Print date (deterministic: configurable via `$WASMSH_DATE`) |
| `sleep` n | Cooperative no-op (returns immediately in browser) |

## Environment

| Command | Description |
|---------|-------------|
| `env` | Print exported variables |
| `printenv` [name] | Print specific or all environment variables |

## System (Virtual)

| Command | Description |
|---------|-------------|
| `id` | Print virtual user identity (`uid=1000(user)`) |
| `whoami` | Print virtual username (`user`) |
| `uname` [-s\|-a\|-m\|-r\|-n] | Print virtual system information |
| `hostname` | Print virtual hostname (`wasmsh`) |

All system utilities return deterministic, sandboxed values. They do not query the host OS.
