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

Formatted output. Format specifiers: `%s`, `%d`, `%%`. Escapes: `\n`, `\t`, `\\`. Repeats format for excess arguments.

## `test` expr / `[` expr `]`

Evaluate conditional expression. Returns 0 (true) or 1 (false).

**String tests**: `-n str`, `-z str`, `s1 = s2`, `s1 != s2`
**Integer tests**: `n1 -eq n2`, `-ne`, `-lt`, `-gt`, `-le`, `-ge`
**File tests**: `-f file`, `-d dir`, `-e path`, `-s file` (non-empty), `-r`, `-w`, `-x`
**Logic**: `! expr`

## `read` [-r] [var...]

Read one line from stdin, split by IFS, assign to variables. Last variable gets the remainder. Without variables, assigns to `REPLY`. Supports multi-line input across calls.

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

## `set` [options] [-- args]

- `set -- arg1 arg2` — set positional parameters
- `set -e` — exit on error (errexit)
- `set -u` — stored but not enforced
- `set -x` — stored but not enforced

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

Display whether a name is a builtin, function, or not found.

## `command` [-v] name

`command -v name` — check if command exists. Without `-v`, run command bypassing functions.

## `getopts` optstring name

Parse positional parameters for options. Sets `name` to the option character and `OPTIND` to the next index.
