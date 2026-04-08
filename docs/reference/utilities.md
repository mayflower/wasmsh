# Utility Command Reference

Utilities operate on the virtual filesystem and streams. They run
in-process with no OS calls. None of them touches the host filesystem,
network, or process table.

88 utilities are registered in `wasmsh-utils`, organised by source module
(`crates/wasmsh-utils/src/*_ops.rs`). The tables below mirror that
grouping.

## How to read this page

- The tables document the **supported subset** of each command. Anything
  not listed is either not implemented or behaves like the GNU/BSD original.
- Where wasmsh implements a non-trivial reimplementation (`awk`, `jq`,
  `yq`, `sed`, `bc`, `tar`, `rg`, `fd`, `dd`, `xxd`), a "Notes" subsection
  spells out which features work and which do not.
- Several commands are stubs by design, returning a deterministic value
  with no side effects. These are flagged as **stub** in the table.
- All utilities respect `set -e` (the surrounding pipeline) and emit
  errors to stderr in the bash convention `cmd: arg: message`.
- All utilities work in pipelines. Without file arguments they read from
  stdin where it makes sense.

## File Operations (`file_ops`)

| Command | Description | Key Flags |
|---------|-------------|-----------|
| `cat` file... | Concatenate and print files. Reads stdin if no args. | |
| `cp` src dst | Copy a file | |
| `mv` src dst | Move (rename) a file | |
| `rm` file... | Remove files | |
| `touch` file... | Create empty files or update timestamps | |
| `mkdir` dir... | Create directories (auto-creates parents) | |
| `ln` src dst | Create a link (copy-based in VFS) | |
| `chmod` mode file | **Stub.** Returns 0; the VFS has no permission model. | |
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

## Text Processing (`text_ops`)

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
| `paste` [-d delim] [-s] [file...] | Merge lines of files side by side | `-d` delimiter (default tab), `-s` serial |
| `rev` [file...] | Reverse characters of each line | |
| `column` [-t] [-s sep] [file...] | Columnate output | `-t` table mode, `-s` input separator |
| `bat` [file...] | `cat` alternative (no syntax highlighting in sandbox; behaves like `cat`) | |

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

## Data Utilities (`data_ops`)

| Command | Description |
|---------|-------------|
| `seq` [start] [step] end | Generate number sequence |
| `expr` arg... | Evaluate arithmetic expression (`+`, `-`, `*`, `/`, `%`, `=`, `!=`) |
| `basename` path [suffix] | Extract filename from path |
| `dirname` path | Extract directory from path |
| `xargs` [cmd] | Build arguments from stdin (default command: `echo`) |
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

## System / Environment (`system_ops`)

| Command | Description |
|---------|-------------|
| `env` | Print exported variables |
| `printenv` [name] | Print specific or all environment variables |
| `id` | Print virtual user identity (`uid=1000(user)`) |
| `whoami` | Print virtual username (`user`) |
| `uname` [-s\|-a\|-m\|-r\|-n] | Print virtual system information |
| `hostname` | Print virtual hostname (`wasmsh`) |
| `sleep` n | Cooperative no-op (returns immediately in the sandbox) |
| `date` | Print date (deterministic: configurable via `$WASMSH_DATE`) |

All system utilities return deterministic, sandboxed values. They do not
query the host OS.

## Search (`search_ops`)

| Command | Description |
|---------|-------------|
| `rg` [opts] pattern [path...] | Recursive grep (ripgrep-compatible subset) |
| `fd` [pattern] [path] | Modern `find` alternative |

### `rg` notes

Supported flags: `-i`/`--ignore-case`, `-v`/`--invert-match`, `-c`/`--count`,
`-l`/`--files-with-matches`, `-n`/`--line-number`, `-w`/`--word-regexp`,
`-F`/`--fixed-strings`. Patterns are POSIX extended regex (the same engine
used by `grep -E`). Recursive descent across the VFS is the default.

Not supported: PCRE features (lookbehind, named captures), `--type`
filters, `.gitignore` honouring, JSON output, `--multiline`.

### `fd` notes

Supported: glob-style patterns, `-t f` / `-t d` type filter, recursive
descent. The default is to search the current working directory.

Not supported: regex patterns (use glob), `.gitignore` honouring, command
execution (`-x`).

## Hash (`hash_ops`)

| Command | Description |
|---------|-------------|
| `sha1sum` [file...] | Compute SHA-1 checksums |
| `sha512sum` [file...] | Compute SHA-512 checksums |

(Note: `md5sum` and `sha256sum` live in `data_ops` for historical reasons.)

## Binary / Files (`binary_ops`)

| Command | Description |
|---------|-------------|
| `xxd` [-r] [file] | Hex dump (and reverse with `-r`) |
| `dd` if=… of=… bs=… count=… | Block copy with conversion options |
| `strings` [file] | Extract printable strings from binary input |
| `split` [-l N] [-b N] [file] [prefix] | Split file into chunks |
| `file` path... | Identify file type (heuristic) |

## Math (`math_ops`)

| Command | Description |
|---------|-------------|
| `bc` [-l] | Arbitrary-precision calculator (POSIX subset) |

### `bc` notes

Reads expressions from stdin or files, prints results to stdout.
Supported: integer and decimal arithmetic, parentheses, the standard
binary operators (`+`, `-`, `*`, `/`, `%`, `^`), comparison and boolean
operators, `scale`, `length`, `sqrt`, variables, `if`, `while`, `for`,
`define` user functions, `print`, `read`. With `-l` (math library), the
`s`, `c`, `a`, `e`, `l`, `j` functions are available.

Not supported: `bc`'s `--quiet` flag is the default (no banner is ever
printed), `void` functions are accepted but warnings differ slightly,
`!` for shell escape is rejected (no shell escape in the sandbox).

## Diff and Patch (`diff_ops`)

| Command | Description |
|---------|-------------|
| `diff` file1 file2 | Show differences (unified format) |
| `patch` [-p N] | Apply a patch from stdin or file |

## Tree (`tree_ops`)

| Command | Description |
|---------|-------------|
| `tree` [dir] | Recursive directory tree listing |

## Disk (`disk_ops`)

| Command | Description |
|---------|-------------|
| `du` [-sh] [path...] | Disk usage of files / directories in the VFS |
| `df` [path] | Filesystem usage summary |

## AWK / JQ / YQ

| Command | Description |
|---------|-------------|
| `awk` [-F sep] 'program' [file...] | Pattern-action language (subset) |
| `jq` filter [file] | JSON query / transform |
| `yq` filter [file] | YAML query / transform |

### `awk` notes

A clean-room subset, sufficient for the typical "select, transform,
sum, group" tasks that real scripts use it for. Supported:

- `BEGIN` and `END` blocks.
- Pattern–action rules: `pattern { action }`, `/regex/ { action }`,
  `expr { action }`.
- Field references: `$0`, `$1`, `$NF`, etc.
- Variables: `NR`, `NF`, `FS`, `OFS`, `ORS`, plus user-assigned scalars
  and one-dimensional associative arrays.
- Operators: arithmetic, string concatenation by juxtaposition,
  comparison, regex match (`~`, `!~`), assignment, increment.
- Control flow: `if/else`, `while`, `for (init; cond; step)`,
  `for (k in arr)`, `next`, `exit`.
- Built-in functions: `length`, `substr`, `index`, `split`, `sub`, `gsub`,
  `tolower`, `toupper`, `printf`, `print`, `sprintf`, `match`.
- `-F sep` flag to set the field separator (any single char or regex).

Not supported: `getline`, multi-dimensional arrays via `arr[i, j]`,
`gawk` extensions, dynamic regex from a string variable in `~` (use
`match` instead), file I/O (`>`, `>>`, `|`), user-defined functions.

### `jq` notes

A clean-room subset, sufficient for selecting, filtering, and projecting
JSON.

Supported:

- Identity (`.`), field access (`.foo`, `.["bar"]`), array index
  (`.[0]`), array slicing (`.[2:5]`).
- Iteration: `.[]`, `.foo[]`.
- Pipes: `expr | expr`.
- Comma: `expr, expr` (produces multiple outputs).
- Filters: `select(cond)`, `map(filter)`, `length`, `keys`, `values`,
  `type`, `has`.
- Comparison and boolean: `==`, `!=`, `<`, `>`, `and`, `or`, `not`.
- Recursive descent: `..`.
- Output formats: `-r` (raw), default JSON.

Not supported: `--slurp`, `--null-input`, modules, custom function
definitions, `tostream`/`fromstream`, regex functions (`test`, `match`),
date functions.

### `yq` notes

YAML-aware sibling of `jq`. Internally yq parses YAML, converts to JSON,
applies the same filter engine as `jq`, and emits YAML or JSON
depending on flags. Supported filter syntax matches the `jq` subset
above. Supported flags: `-r` (raw), `-o json` (JSON output).

## Archive (`archive_ops`)

| Command | Description |
|---------|-------------|
| `tar` [-cxtf] [archive] [file...] | Create / extract / list tar archives |
| `gzip` [file] | Compress with gzip |
| `gunzip` [file] | Decompress gzip |
| `zcat` [file] | Decompress to stdout |
| `unzip` archive | Extract a zip archive |

### `tar` notes

Supported flags: `-c` (create), `-x` (extract), `-t` (list), `-f file`
(archive name; `-` for stdin/stdout), `-v` (verbose listing), `-z`
(gzip on the fly), `-C dir` (change directory), `--strip-components N`.

Supported formats: ustar (the default and most portable). Plain old tar
is read for compatibility.

Not supported: `--zstd`, `--bzip2`, `--xz` (only gzip is wired up), pax
extended headers, sparse files, ACLs, owner/group preservation (the
VFS has no permission model), incremental listings (`--listed-incremental`).

### `gzip` / `gunzip` / `zcat` / `unzip` notes

`gzip` / `gunzip` use deflate. The `-9` and `-1` flags are accepted but
the compression ratio is fixed at the library default.

`unzip` extracts standard ZIP archives. Encrypted archives are
rejected. Multi-volume archives are not supported.

## Network (`net_ops`)

| Command | Description |
|---------|-------------|
| `curl` [opts] url | HTTP client (gated by the `allowed_hosts` capability) |
| `wget` [opts] url | HTTP downloader (gated by the `allowed_hosts` capability) |

### `curl` notes

Supported flags: `-X METHOD`, `-H "Header: value"`, `-d data`,
`--data-binary @file`, `-o file`, `-s` (silent), `-i` (include
response headers), `-L` (follow redirects, capped at 10 hops),
`-u user:pass` (basic auth), `--max-time N`. The default request method
is GET; with `-d` it becomes POST. Output goes to stdout by default.

Not supported: cookie jars (`-b`/`-c`), uploads via `-F` multipart,
`--http2`, client certs, SOCKS proxies, `--resolve`, retry logic.

### `wget` notes

A simpler counterpart to `curl`. Supported flags: `-O file`,
`-q` (quiet), `--max-redirect N`, `--header "Header: value"`,
`-T timeout`. Always saves to a file unless `-O -` is used.

Not supported: recursive downloads (`-r`), `.wgetrc`, mirroring,
spider mode.

### Capability gating

Network utilities are disabled by default. Hosts must be explicitly
granted via `HostCommand::Init { allowed_hosts: vec![...], .. }`.
Patterns support exact host (`api.example.com`), wildcard
(`*.example.com`), IP, and host with port. With an empty allowlist,
both utilities exit with `Diagnostic(Error, "host not allowed: …")` and
exit code 1 — no network call is made. See
[Sandbox and capabilities](sandbox-and-capabilities.md#network-allowlist)
and [ADR-0021](../adr/adr-0021-network-capability.md) for the model.

## POSIX Coreutils (`trivial_ops`)

Small POSIX coreutils with focused implementations:

| Command | Description |
|---------|-------------|
| `which` cmd | Locate a command (checks builtins, functions, utilities) |
| `rmdir` dir... | Remove empty directories |
| `tac` [file...] | Reverse cat (last line first) |
| `nl` [file] | Number lines |
| `shuf` [file] | Shuffle lines |
| `cmp` file1 file2 | Byte-by-byte comparison |
| `comm` file1 file2 | Compare two sorted files line by line |
| `fold` [-w N] [file] | Wrap lines to fit width |
| `nproc` | **Stub.** Returns `1` in the sandbox. |
| `expand` [file] | Convert tabs to spaces |
| `unexpand` [file] | Convert spaces to tabs |
| `truncate` -s size file | Shrink or extend a file to size |
| `factor` n... | Factor integers into primes |
| `cksum` [file...] | CRC checksum and byte count |
| `tsort` [file] | Topological sort |
| `install` src dst | Copy file (permissions and ownership are no-ops in the VFS) |
| `timeout` duration cmd | Run a command with a timeout (cooperative — observed at the next VM step) |
| `cal` [month] [year] | Display a calendar |

## Pipeline composition examples

A few real-world combinations to show how the utilities fit together:

```sh
# Find the largest file in /workspace
find /workspace -type f | xargs du -b | sort -rn | head -1

# Count how many JSON entries match a condition
jq '.items[] | select(.status == "ok")' < data.json | wc -l

# Tally errors per minute in a log
grep ERROR /var/log/app.log | awk '{print substr($1, 1, 16)}' | sort | uniq -c

# Bundle a directory into a base64 string for transport
tar -czf - /workspace | base64

# Dry-run apply a patch
diff -u old.txt new.txt | patch --dry-run old.txt
```

## See Also

- [Builtins reference](builtins.md) for commands that mutate shell state.
- [Shell syntax reference](shell-syntax.md) for how commands compose.
- [Sandbox and capabilities](sandbox-and-capabilities.md) for the network
  allowlist that gates `curl` and `wget`.
- [Adding a command](../guides/adding-commands.md) if you want to write a
  new utility.
