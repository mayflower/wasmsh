# Supported Syntax and Commands

## Shell Syntax

### Implemented

**Commands and lists**
- Simple commands: `cmd arg1 arg2 ...`
- Pipelines: `cmd1 | cmd2 | cmd3`
- Stderr-to-pipe: `cmd1 |& cmd2`
- And/or lists: `cmd1 && cmd2`, `cmd1 || cmd2`
- Semicolon lists: `cmd1; cmd2; cmd3`
- Background execution: `cmd &` (parsed; browser runtime runs synchronously)
- Pipeline negation: `! cmd`
- Variable assignments: `VAR=value`, `VAR=value cmd`
- Append assignments: `VAR+=value`

**Compound commands**
- `if/then/elif/else/fi`
- `while/do/done`
- `until/do/done`
- `for var in words; do/done`
- `for (( init; cond; step )); do/done` (C-style arithmetic for)
- `case/esac` with `;;`, `;&` (fall-through), `;;&` (continue-testing)
- `select/do/done` (menu-driven; repeats until `break` or EOF)
- `(( expr ))` arithmetic command
- `[[ expr ]]` extended test
- Subshells: `( ... )`
- Brace groups: `{ ...; }`
- Function definitions: `name() { ... }`, `function name { ... }`, `function name() { ... }`

**Redirections**
- Input: `<`
- Output (truncate): `>`
- Output (append): `>>`
- Read-write: `<>`
- Here-document: `<<DELIM`, `<<-DELIM` (tab-stripping), quoted delimiters suppress expansion
- Here-string: `<<<`
- FD-prefixed: `2>`, `2>>`, `2>&1`
- Combined stdout+stderr: `&>`

**Quoting and escaping**
- Single quoting: `'literal text'`
- Double quoting: `"text with $expansion"`
- Backslash escaping: `\char`
- ANSI-C quoting: `$'...'` (lexer support)
- Comments: `# comment`

**Expansions**
- Parameter expansion: `$var`, `${var}` and all operators (see section below)
- Command substitution: `$(...)`
- Arithmetic expansion: `$(( expr ))`
- Process substitution: `<(cmd)`, `>(cmd)`
- Brace expansion: `{a,b,c}`, `{1..10}`
- Tilde expansion: `~`, `~/path`
- Field splitting on `IFS`
- Glob/pathname expansion: `*`, `?`, `[...]`, extglob patterns (see below)

### Not Yet Implemented

- Coprocesses: `coproc`
- Full POSIX signal delivery / job-linked signal semantics

---

## Builtins

| Command      | Status | Notes |
|--------------|--------|-------|
| `:`          | Done   | No-op, always returns 0 |
| `true`       | Done   | Returns 0 |
| `false`      | Done   | Returns 1 |
| `echo`       | Done   | `-n` (suppress newline), `-e` (escape sequences: `\n \t \\ \a \b \r \0NNN`) |
| `printf`     | Done   | `%s %d %x %o %f %c %b %q %%`; width, precision, `-` (left-align), `0` (zero-pad); repeats format for extra args |
| `pwd`        | Done   | Prints working directory |
| `cd`         | Done   | `cd -` (OLDPWD), `cd` (HOME); sets PWD and OLDPWD |
| `export`     | Done   | `export NAME=VALUE`, `export NAME`; respects readonly |
| `unset`      | Done   | Removes variable or array element (`unset 'arr[N]'`); respects readonly |
| `readonly`   | Done   | `readonly NAME=VALUE`, `readonly NAME` |
| `test` / `[` | Done   | Unary: `-n -z -f -d -e -s -r -w -x`; binary: `= == != -eq -ne -lt -gt -le -ge`; `!` negation |
| `read`       | Done   | `-r` (raw), `-p prompt`, `-d delim`, `-n N`, `-N N`, `-a array`, `-t timeout`, `-s` (silent); IFS splitting; default var REPLY |
| `shift`      | Done   | `shift [N]`; shifts positional parameters |
| `return`     | Done   | Returns from function with optional exit status |
| `exit`       | Done   | Exits shell with optional status; fires EXIT trap |
| `local`      | Done   | `local VAR=val`; save/restore stack for function scope |
| `type`       | Done   | Reports alias/function/builtin/utility classification |
| `command`    | Done   | `-v` shows command type; bypasses functions |
| `eval`       | Done   | Re-parses and executes concatenated arguments |
| `set`        | Done   | `-e` (errexit), `-u` (nounset), `-x` (xtrace), `-f` (noglob), `-a` (allexport), `-C` (noclobber), `-o pipefail`; `set -- args` sets positionals |
| `getopts`    | Done   | Parses short options from positional parameters; updates OPTIND |
| `trap`       | Done   | `EXIT`, `ERR`, `DEBUG`, `RETURN`, `trap -p`, `trap -l`, reset/ignore; regular signal names are accepted but not delivered in the sandbox |
| `declare` / `typeset` | Done | `-i` (integer), `-a` (indexed array), `-A` (assoc array), `-x` (export), `-r` (readonly), `-l` (lowercase), `-u` (uppercase), `-n` (nameref), `-p` (print); compound assignment `arr=(...)` |
| `let`        | Done   | Evaluates arithmetic expressions; exit status is 0 if last result is non-zero |
| `shopt`      | Done   | `-s` / `-u`; options: `extglob nullglob dotglob globstar nocasematch nocaseglob failglob lastpipe expand_aliases` |
| `alias`      | Done   | Define and list aliases; aliases expand recursively |
| `unalias`    | Done   | `-a` removes all aliases |
| `source` / `.` | Done | Reads and executes a file from VFS; searches PATH for bare names |
| `mapfile` / `readarray` | Done | `-t` (strip newline); default array MAPFILE |
| `builtin`    | Done   | Bypasses aliases and functions; invokes named builtin directly |

---

## Utilities (88)

All utilities operate on the in-process VFS (no OS calls).

### File utilities (14)

| Command      | Status | Notes |
|--------------|--------|-------|
| `cat`        | Done   | Concatenate files; reads stdin when no files given |
| `ls`         | Done   | Directory listing |
| `mkdir`      | Done   | Create directories |
| `rm`         | Done   | Remove files and directories |
| `touch`      | Done   | Create empty files or update timestamps |
| `mv`         | Done   | Move/rename files |
| `cp`         | Done   | Copy files |
| `ln`         | Done   | Create hard and symbolic links |
| `readlink`   | Done   | Read symlink target |
| `realpath`   | Done   | Resolve to absolute path |
| `stat`       | Done   | Show file metadata |
| `find`       | Done   | Search filesystem |
| `chmod`      | Stub   | Returns 0; the VFS has no permission model. |
| `mktemp`     | Done   | Create a temporary file |

### Text utilities (14)

| Command      | Status | Notes |
|--------------|--------|-------|
| `head`       | Done   | First N lines (`-n N`) |
| `tail`       | Done   | Last N lines (`-n N`) |
| `wc`         | Done   | Line/word/byte counts (`-l -w -c`) |
| `grep`       | Done   | Pattern search |
| `sed`        | Done   | Stream editor |
| `sort`       | Done   | Sort lines |
| `uniq`       | Done   | Remove duplicate adjacent lines |
| `cut`        | Done   | Cut fields or characters |
| `tr`         | Done   | Translate or delete characters |
| `tee`        | Done   | Write stdin to file and stdout |
| `paste`      | Done   | Merge lines of files |
| `rev`        | Done   | Reverse characters in each line |
| `column`     | Done   | Format input into columns |
| `bat`        | Done   | Syntax-highlighted file viewer |

### Data and string utilities (9)

| Command      | Status | Notes |
|--------------|--------|-------|
| `seq`        | Done   | Generate sequences of numbers |
| `basename`   | Done   | Strip directory and suffix from path |
| `dirname`    | Done   | Extract directory part of path |
| `expr`       | Done   | Evaluate expression |
| `xargs`      | Done   | Build and execute commands from stdin |
| `yes`        | Done   | Output string repeatedly |
| `md5sum`     | Done   | Compute MD5 checksums (clean-room RFC 1321) |
| `sha256sum`  | Done   | Compute SHA-256 checksums (clean-room FIPS 180-4) |
| `base64`     | Done   | Encode/decode base64 |

### System and environment utilities (8)

| Command      | Status | Notes |
|--------------|--------|-------|
| `env`        | Done   | Print or set environment |
| `printenv`   | Done   | Print environment variables |
| `id`         | Done   | Print user/group identity (static sandbox values) |
| `whoami`     | Done   | Print current user (static sandbox value) |
| `uname`      | Done   | Print system information (static sandbox values) |
| `hostname`   | Done   | Print hostname (static sandbox value) |
| `sleep`      | Done   | Delay (no-op in sandbox; returns immediately) |
| `date`       | Done   | Print date/time |

### Simple utilities (18)

| Command      | Status | Notes |
|--------------|--------|-------|
| `which`      | Done   | Locate a command |
| `rmdir`      | Done   | Remove empty directories |
| `tac`        | Done   | Reverse lines of file |
| `nl`         | Done   | Number lines |
| `shuf`       | Done   | Shuffle lines |
| `cmp`        | Done   | Compare two files byte by byte |
| `comm`       | Done   | Compare two sorted files line by line |
| `fold`       | Done   | Wrap lines to specified width |
| `nproc`      | Done   | Print number of processing units |
| `expand`     | Done   | Convert tabs to spaces |
| `unexpand`   | Done   | Convert spaces to tabs |
| `truncate`   | Done   | Shrink or extend file size |
| `factor`     | Done   | Print prime factors |
| `cksum`      | Done   | Print CRC checksum and byte count |
| `tsort`      | Done   | Topological sort |
| `install`    | Done   | Copy files and set attributes |
| `timeout`    | Done   | Run command with time limit |
| `cal`        | Done   | Display a calendar |

### Diff and patch (2)

| Command      | Status | Notes |
|--------------|--------|-------|
| `diff`       | Done   | Compare files line by line (unified format) |
| `patch`      | Done   | Apply unified diff patches |

### Directory visualization (1)

| Command      | Status | Notes |
|--------------|--------|-------|
| `tree`       | Done   | Recursive directory listing with tree-style output |

### Code search (2)

| Command      | Status | Notes |
|--------------|--------|-------|
| `rg`         | Done   | Ripgrep-compatible search with built-in regex engine |
| `fd`         | Done   | Fast file finder (fd-find compatible) |

### Embedded interpreters (4)

| Command      | Status | Notes |
|--------------|--------|-------|
| `awk`        | Done   | Full AWK interpreter: lexer, parser, evaluator; associative arrays, user functions, regex |
| `jq`         | Done   | JSON processor: handwritten JSON parser, filter language, 90+ built-in functions |
| `yq`         | Done   | YAML processor: handwritten YAML parser, jq-compatible filter subset |
| `bc`         | Done   | Calculator: expression parser, variables, control flow, user-defined functions |

### Hash utilities (2)

| Command      | Status | Notes |
|--------------|--------|-------|
| `sha1sum`    | Done   | Compute SHA-1 checksums (clean-room RFC 3174) |
| `sha512sum`  | Done   | Compute SHA-512 checksums (clean-room FIPS 180-4) |

### Binary utilities (5)

| Command      | Status | Notes |
|--------------|--------|-------|
| `xxd`        | Done   | Hex dump and reverse |
| `dd`         | Done   | Copy and convert data |
| `strings`    | Done   | Print printable strings from binary data |
| `split`      | Done   | Split file into pieces |
| `file`       | Done   | Determine file type via magic bytes |

### Archive and compression (5)

| Command      | Status | Notes |
|--------------|--------|-------|
| `tar`        | Done   | Create, extract, list (`-f -` reads stdin / writes stdout for piping) |
| `gzip`       | Done   | Compress files (DEFLATE, clean-room CRC-32) |
| `gunzip`     | Done   | Decompress gzip files |
| `zcat`       | Done   | Decompress and print to stdout |
| `unzip`      | Done   | Extract ZIP archives |

### Disk usage (2)

| Command      | Status | Notes |
|--------------|--------|-------|
| `du`         | Done   | Estimate file space usage |
| `df`         | Done   | Report filesystem disk space usage |

### Network utilities (2)

| Command      | Status | Notes |
|--------------|--------|-------|
| `curl`       | Done   | HTTP client — GET/POST/HEAD, headers, output to file/stdout, follow redirects, verbose, fail-on-error, write-out |
| `wget`       | Done   | File downloader — download to file or stdout, quiet mode, custom headers |

Network access requires an allowlist of permitted hosts configured at sandbox initialization. Without an allowlist, both commands return an error. See [ADR-0021](docs/adr/adr-0021-network-capability.md).

---

## Parameter Expansion

| Operator                       | Meaning |
|--------------------------------|---------|
| `$var` / `${var}`              | Value of variable |
| `${#var}`                      | String length of value |
| `${var:-word}`                 | Value if set and non-empty, else `word` |
| `${var-word}`                  | Value if set, else `word` |
| `${var:=word}`                 | Value if set and non-empty, else assign and use `word` |
| `${var=word}`                  | Value if set, else assign and use `word` |
| `${var:+word}`                 | `word` if set and non-empty, else empty |
| `${var+word}`                  | `word` if set, else empty |
| `${var:?word}`                 | Value if set and non-empty, else error with `word` |
| `${var#pattern}`               | Remove shortest prefix matching `pattern` |
| `${var##pattern}`              | Remove longest prefix matching `pattern` |
| `${var%pattern}`               | Remove shortest suffix matching `pattern` |
| `${var%%pattern}`              | Remove longest suffix matching `pattern` |
| `${var/pat/rep}`               | Replace first occurrence of `pat` with `rep` |
| `${var//pat/rep}`              | Replace all occurrences of `pat` with `rep` |
| `${var/#pat/rep}`              | Replace `pat` anchored at start |
| `${var/%pat/rep}`              | Replace `pat` anchored at end |
| `${var:offset}`                | Substring from `offset` |
| `${var:offset:length}`         | Substring from `offset`, `length` chars |
| `${var^}`                      | Uppercase first character |
| `${var^^}`                     | Uppercase all characters |
| `${var,}`                      | Lowercase first character |
| `${var,,}`                     | Lowercase all characters |
| `${var@Q}`                     | Quote value for reuse as shell input |
| `${var@E}`                     | Expand backslash escape sequences |
| `${var@U}`                     | Uppercase all characters |
| `${var@L}`                     | Lowercase all characters |
| `${var@u}`                     | Uppercase first character |
| `${var@A}`                     | Assignment statement form (`declare -- var="value"`) |
| `${!var}`                      | Indirect expansion (value of the variable named by `$var`) |
| `${!prefix*}` / `${!prefix@}`  | Names of all variables with the given prefix |
| `${arr[@]}` / `${arr[*]}`      | All elements of indexed or associative array |
| `${arr[N]}`                    | Single element of array by index or key |
| `${#arr[@]}` / `${#arr[*]}`    | Number of elements in array |
| `${!arr[@]}` / `${!arr[*]}`    | All keys/indices of array |
| `$?`                           | Exit status of last command |
| `$#`                           | Number of positional parameters |
| `$@` / `$*`                    | All positional parameters |
| `$0`                           | Script/shell name |
| `$1`–`$N`                      | Positional parameters |

---

## Arithmetic

Arithmetic is available in `$(( ))`, `(( ))`, `let`, and `declare -i` contexts. The evaluator is a full recursive-descent parser.

### Operators (in precedence order, lowest to highest)

| Operator            | Description |
|---------------------|-------------|
| `expr ? a : b`      | Ternary conditional |
| `,`                 | Comma (evaluate both, return right) |
| `= += -= *= /= %= <<= >>= &= ^= \|=` | Assignment operators |
| `\|\|`              | Logical OR |
| `&&`                | Logical AND |
| `\|`                | Bitwise OR |
| `^`                 | Bitwise XOR |
| `&`                 | Bitwise AND |
| `== !=`             | Equality |
| `< > <= >=`         | Comparison |
| `<< >>`             | Bitwise shift |
| `+ -`               | Addition, subtraction |
| `* / %`             | Multiplication, division, modulo |
| `**`                | Exponentiation |
| `! ~ - +`           | Unary NOT, bitwise complement, negate, plus |
| `++ --`             | Prefix and postfix increment/decrement |

### Literal formats

| Format          | Example |
|-----------------|---------|
| Decimal         | `42` |
| Hexadecimal     | `0xff` |
| Binary          | `0b1010` |
| Octal           | `0755` |
| Arbitrary base  | `16#ff`, `2#1010` |

---

## Glob and Pathname Expansion

| Pattern          | Description |
|------------------|-------------|
| `*`              | Match any string (not leading `.` unless `dotglob`) |
| `?`              | Match any single character |
| `[abc]`          | Match any character in the set |
| `[a-z]`          | Match any character in the range |
| `[!abc]`         | Match any character not in the set |
| `**`             | Match zero or more directories (requires `shopt -s globstar`) |
| `?(pat)`         | Match zero or one occurrence (requires `extglob`) |
| `*(pat)`         | Match zero or more occurrences (requires `extglob`) |
| `+(pat)`         | Match one or more occurrences (requires `extglob`) |
| `@(pat)`         | Match exactly one occurrence (requires `extglob`) |
| `!(pat)`         | Match anything except `pat` (requires `extglob`) |

`extglob` is enabled by default. `nullglob`, `dotglob`, `globstar`, `nocasematch`, `nocaseglob`, `failglob` are available via `shopt`.

---

## Shell Options

### `set` options

| Flag      | Long name    | Description |
|-----------|--------------|-------------|
| `-e`      | `errexit`    | Exit on any command failure |
| `-u`      | `nounset`    | Error on unset variable reference |
| `-x`      | `xtrace`     | Print commands before executing (`PS4` prefix) |
| `-f`      | `noglob`     | Disable glob expansion |
| `-a`      | `allexport`  | Auto-export all variable assignments |
| `-C`      | `noclobber`  | Prevent `>` from overwriting existing files |
| `-o pipefail` | `pipefail` | Pipeline exit status is rightmost non-zero stage |

### `shopt` options

| Option           | Default | Description |
|------------------|---------|-------------|
| `extglob`        | on      | Enable extended glob patterns |
| `nullglob`       | off     | Unmatched globs expand to nothing |
| `dotglob`        | off     | Globs match filenames starting with `.` |
| `globstar`       | off     | `**` matches directories recursively |
| `nocasematch`    | off     | Case-insensitive `case` and `[[ =~ ]]` matching |
| `nocaseglob`     | off     | Case-insensitive glob matching |
| `failglob`       | off     | Error when glob matches nothing |
| `lastpipe`       | off     | Last pipeline stage runs in current shell |
| `expand_aliases` | on      | Enable alias expansion |

---

## Special Variables

| Variable       | Description |
|----------------|-------------|
| `?`            | Exit status of last command |
| `$$`           | Virtual shell PID |
| `$!`           | Last background PID slot (currently `0` without real job control) |
| `#`            | Number of positional parameters |
| `@` / `*`      | All positional parameters |
| `$_`           | Last argument of the previous command |
| `$-`           | Active single-letter shell flags |
| `0`            | Shell/script name |
| `IFS`          | Input field separator (default: space, tab, newline) |
| `HOME`         | Home directory for tilde expansion and `cd` |
| `PWD`          | Current working directory |
| `OLDPWD`       | Previous working directory |
| `PATH`         | Colon-separated search path for `source` |
| `OPTIND`       | Current index for `getopts` |
| `REPLY`        | Default variable for `read` |
| `PIPESTATUS`   | Array of exit statuses for last pipeline stages |
| `PS4`          | Prompt prefix for `set -x` xtrace output |
| `LINENO`       | Current line number in executing script |
| `FUNCNAME`     | Stack of currently executing function names |
| `BASH_SOURCE`  | Stack of filenames for `source` calls |
| `MAPFILE`      | Default array for `mapfile` |

---

## Build Targets

| Target | Triple | FS Backend | Python | Build Command |
|--------|--------|------------|--------|---------------|
| **Standalone** | `wasm32-unknown-unknown` | `MemoryFs` (in-process) | N/A | `just build-standalone` |
| **Pyodide** | `wasm32-unknown-emscripten` | `EmscriptenFs` (libc, shared with Python) | In-process via `PyRun_SimpleString` | `just build-pyodide` |

### Pyodide-only commands

| Command | Flags | Description |
|---------|-------|-------------|
| `python` / `python3` | `-c CODE` | Run Python code in-process; stdin from heredoc/pipe also supported |
| `pip` / `pip3` | `install PKG [PKG ...]` | Install pure-Python packages via micropip |

Python stdout and stderr are captured and surfaced as normal `Stdout`/`Stderr` worker events. File I/O from Python goes through the same Emscripten filesystem the shell uses.

### Python package installation (micropip)

The Pyodide build includes [micropip](https://micropip.pyodide.org/) for installing Python packages at runtime. Packages are installed into the in-process virtual filesystem and become importable immediately.

**Supported install methods:**
- `pip install <package>` — resolved from Pyodide CDN or PyPI
- `pip install https://host/pkg-1.0-py3-none-any.whl` — direct URL (requires `allowedHosts`)
- Session API: `session.installPythonPackages("package")` from JavaScript

**What works:**
- Pure-Python wheels (`py3-none-any`): six, attrs, click, packaging, beautifulsoup4, networkx, idna, certifi, pyyaml, jinja2, toml, tomli, markupsafe, chardet, pyparsing, more-itertools, decorator, wrapt, pluggy, and many others
- Pyodide pre-compiled packages with pure-Python fallbacks (e.g., pyyaml)

**What doesn't work yet:**
- C extension packages that require `dlopen` (numpy, pandas, scipy, regex) — install succeeds but import fails because the build uses `MAIN_MODULE=2`. Switching to `MAIN_MODULE=1` with explicit symbol filtering would enable these.

**Security:**
- Network installs require `allowedHosts` configured at session creation
- `file:` URIs are rejected
- `emfs:` installs (from the in-sandbox filesystem) always work
- Installs are session-local and do not persist

---

## Non-Goals

- Not a BusyBox port or Bash fork — clean-room implementation
- No real OS processes in the browser — all commands run in-process
- No kernel or network administration tools
- No TTY/terminal emulation
- No full job control (`fg`, `bg`, `jobs`); `&` parses but runs synchronously
- No POSIX signal delivery; only shell-level traps such as `EXIT`, `ERR`, `DEBUG`, and `RETURN` are executed
- No coprocesses
- GPL/AGPL/SSPL code is forbidden in the core
