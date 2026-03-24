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
| `mktemp` [-d] [template] | Create a temporary file or directory | `-d` create directory |

### `mktemp` detail

```sh
mktemp                    # creates /tmp/tmp.XXXXXXXXXX, prints path
mktemp -d                 # creates a temporary directory
mktemp /tmp/myapp.XXXXX   # custom template (at least 3 trailing X's)
```

The template must end with at least 3 `X` characters which are replaced with
random alphanumeric characters. The created path is printed to stdout.

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
| `paste` [-d delim] [-s] [file...] | Merge lines of files side by side | `-d` delimiter (default tab), `-s` serial |
| `rev` [file...] | Reverse characters of each line | |
| `column` [-t] [-s sep] [file...] | Columnate output | `-t` table mode, `-s` input separator |

### `paste` detail

```sh
paste file1 file2         # merge corresponding lines with tab
paste -d, file1 file2     # use comma as delimiter
paste -s file             # join all lines of a file onto one line
paste - - < file          # read two columns from stdin
```

### `rev` detail

```sh
echo "hello" | rev        # → olleh
rev file.txt              # reverse each line of file
```

### `column` detail

```sh
column -t data.txt        # auto-align whitespace-separated columns
column -t -s, data.csv    # treat comma as field separator
```

## Data Utilities

| Command | Description |
|---------|-------------|
| `seq` [start] [step] end | Generate number sequence |
| `expr` arg... | Evaluate arithmetic expression (`+`, `-`, `*`, `/`, `%`, `=`, `!=`) |
| `basename` path [suffix] | Extract filename from path |
| `dirname` path | Extract directory from path |
| `date` | Print date (deterministic: configurable via `$WASMSH_DATE`) |
| `sleep` n | Cooperative no-op (returns immediately in browser) |
| `yes` [string] | Repeatedly output `string` (default `y`) until limit |
| `md5sum` [file...] | Compute MD5 checksums |
| `sha256sum` [file...] | Compute SHA-256 checksums |
| `base64` [-d] [-w N] [file...] | Base64 encode or decode |

### `yes` detail

```sh
yes              # prints "y" repeatedly (capped at 65536 lines in sandbox)
yes hello        # prints "hello" repeatedly
yes | head -5    # first 5 "y" lines
```

### `md5sum` / `sha256sum` detail

```sh
md5sum file.txt           # prints:  <hex>  file.txt
sha256sum file.txt        # prints:  <hex>  file.txt
echo -n "text" | md5sum   # hash stdin (prints:  <hex>  -)
```

Output format is compatible with GNU coreutils. Both commands accept multiple
files and read from stdin when no file arguments are given.

### `base64` detail

```sh
echo "hello" | base64        # encode (wraps at 76 columns by default)
echo "aGVsbG8=" | base64 -d  # decode
base64 -w 0 file             # encode without line wrapping
```

- `-d` / `--decode` — decode mode
- `-w N` — wrap encoded output at N columns (0 = no wrap, default 76)
- Reads from stdin if no file arguments are given.
- Decoding ignores whitespace in the input.

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
