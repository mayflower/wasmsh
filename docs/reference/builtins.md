# Builtin Command Reference

Builtins execute in-process and can modify shell state directly.

## `:` (colon)

No-op. Always returns 0.

## `true` / `false`

Return 0 or 1 respectively.

## `echo` [-n] [-e] [args...]

Print arguments separated by spaces, followed by newline.

- `-n` — suppress trailing newline
- `-e` — interpret escape sequences: `\n`, `\t`, `\\`, `\a`, `\b`, `\r`, `\0NNN`

## `printf` format [args...]

Formatted output. Repeats format for excess arguments (POSIX behavior).

**Format specifiers**: `%s`, `%d`, `%x` (hex), `%o` (octal), `%f` (float),
`%c` (character), `%b` (escape sequences), `%q` (shell-quoted), `%%`.

**Width and precision**: `%10s`, `%-10s` (left-align), `%010d` (zero-pad),
`%.3f` (precision), `%10.3f` (width and precision).

**Escape sequences in format**: `\n`, `\t`, `\\`.

## `test` expr / `[` expr `]`

Evaluate conditional expression. Returns 0 (true) or 1 (false).

**String tests**: `-n str`, `-z str`, `s1 = s2`, `s1 != s2`
**Integer tests**: `n1 -eq n2`, `-ne`, `-lt`, `-gt`, `-le`, `-ge`
**File tests**: `-f file`, `-d dir`, `-e path`, `-s file` (non-empty), `-r`, `-w`, `-x`
**Logic**: `! expr`

## `read` [-r] [-p prompt] [-d delim] [-n N] [-N N] [-a array] [-t timeout] [-s] [var...]

Read one line from stdin, split by IFS, assign to variables. Last variable gets the remainder. Without variables, assigns to `REPLY`.

- `-r` — raw mode: backslash does not act as escape
- `-p prompt` — print `prompt` to stderr before reading
- `-d delim` — use `delim` as line delimiter instead of newline
- `-n N` — read at most N characters (stops at delimiter)
- `-N N` — read exactly N characters (ignores delimiter)
- `-a array` — read words into indexed array `array`
- `-t timeout` — accepted but not enforced (browser has no blocking I/O)
- `-s` — silent mode (accepted, no effect in browser)

## `cd` [dir]

Change working directory. `cd` alone goes to `$HOME`. `cd -` goes to `$OLDPWD`.

## `pwd`

Print working directory.

## `export` [name[=value]...]

Mark variables as exported. `export FOO=bar` sets and exports.

## `unset` name...

Remove variables. Respects `readonly`.

## `readonly` [name[=value]...]

Mark variables as readonly. Prevents modification and unsetting.

## `declare` / `typeset` [flags] [name[=value]...]

Declare variables with optional attributes. `typeset` is an alias for `declare`.

**Flags**:

| Flag | Meaning |
|------|---------|
| `-i` | Integer: value is evaluated as arithmetic on assignment |
| `-a` | Indexed array |
| `-A` | Associative array |
| `-x` | Export (same as `export`) |
| `-r` | Readonly |
| `-l` | Lowercase: value is converted to lowercase on assignment |
| `-u` | Uppercase: value is converted to uppercase on assignment |
| `-n` | Nameref: variable is a reference to another variable |
| `-g` | Global scope (silently accepted) |
| `-p` | Print current attributes and values; with names, print only those |

```sh
declare -i x=10+5      # x=15 (arithmetic)
declare -a arr=(a b c) # indexed array
declare -A map=([k]=v) # associative array
declare -p              # print all variables
declare -p PATH         # print one variable
```

## `alias` [name[=value]...]

Define or display aliases.

- `alias` — list all aliases in `alias name='value'` form
- `alias name='value'` — define alias
- `alias name` — print the definition of `name`

## `unalias` [-a] name...

Remove aliases. `-a` removes all aliases.

## `let` expr...

Evaluate arithmetic expressions. Each argument is evaluated as an arithmetic
expression. Returns 0 if the last expression is non-zero, 1 if zero.

```sh
let "x = 5 * 3"     # x=15
let x++ y="x*2"     # increment x, set y
```

## `set` [options] [-- args]

- `set -- arg1 arg2` — set positional parameters
- `set -e` / `set +e` — enable/disable errexit (exit on error)
- `set -u` / `set +u` — enable/disable nounset
- `set -x` / `set +x` — enable/disable xtrace
- `set -f` / `set +f` — enable/disable noglob
- `set -a` / `set +a` — enable/disable allexport
- `set -C` / `set +C` — enable/disable noclobber
- `set -o pipefail` / `set +o pipefail` — enable/disable pipefail
- `set -o errexit` — long-form option name (equivalent to `-e`)

## `shopt` [-s|-u] [optname...]

Query and set shell options.

- `shopt` — list all options and their state (`on`/`off`)
- `shopt optname` — print state of specific option
- `shopt -s optname` — enable option
- `shopt -u optname` — disable option

**Supported options**:

| Option | Default | Effect |
|--------|---------|--------|
| `extglob` | on | Extended glob patterns: `?()`, `*()`, `+()`, `@()`, `!()` |
| `nullglob` | off | Unmatched globs expand to nothing (instead of literal pattern) |
| `dotglob` | off | `*` matches dotfiles |
| `globstar` | off | `**` matches recursively across directories |
| `nocasematch` | off | Pattern matching is case-insensitive |
| `nocaseglob` | off | Pathname expansion is case-insensitive |
| `failglob` | off | Unmatched globs cause an error |
| `lastpipe` | off | Last pipeline stage runs in current shell |
| `expand_aliases` | off | Alias expansion in non-interactive mode |

## `mapfile` / `readarray` [-t] [array]

Read lines from stdin into an indexed array. `readarray` is an alias.

- `-t` — strip the trailing newline from each line
- `array` — name of the array to populate (default: `MAPFILE`)

```sh
mapfile -t lines < file.txt
printf '%s\n' "${lines[@]}"
```

## `builtin` name [args...]

Invoke `name` as a builtin directly, bypassing function and alias lookup.
Returns 1 if `name` is not a shell builtin.

## `shift` [n]

Shift positional parameters left by `n` (default 1).

## `local` name[=value]...

Declare local variables in a function. Restored to previous value when function returns.

## `return` [n]

Return from a function with exit status `n`.

## `exit` [n]

Exit the shell with status `n`.

## `break` [n] / `continue`

Break out of or continue the next iteration of the enclosing loop.

## `eval` args...

Concatenate arguments and execute as shell code.

## `source` file / `.` file

Read and execute commands from file in the current shell context.

## `trap` command signal...

Set a handler for signals. Supported: `EXIT` (fires on shell exit), `ERR` (fires on command failure). Other signals are no-ops in the browser.

## `type` name...

Display whether a name is an alias, function, builtin, utility, or not found.
Checks in that order, consistent with resolution priority.

## `command` [-v] name

`command -v name` — check if command exists. Without `-v`, run command bypassing functions.

## `getopts` optstring name

Parse positional parameters for options. Sets `name` to the option character and `OPTIND` to the next index.
