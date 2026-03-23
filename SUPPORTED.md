# Supported Syntax and Commands

## Shell Syntax

### Implemented
- Simple commands: `cmd arg1 arg2 ...`
- Pipelines: `cmd1 | cmd2 | cmd3`
- And/or lists: `cmd1 && cmd2`, `cmd1 || cmd2`
- Semicolon lists: `cmd1; cmd2; cmd3`
- Pipeline negation: `! cmd`
- Variable assignments: `VAR=value`, `VAR=value cmd`
- Redirections: `<`, `>`, `>>`, `<>`
- Here-documents: `<<DELIM`, `<<-DELIM` (tab-stripping)
- Single quoting: `'literal text'`
- Double quoting: `"text with $expansion"`
- Parameter expansion: `$var`, `${var}`, `${var:-default}`, `${var:+alt}`, `${#var}`
- Command substitution: `$(...)` (placeholder — expands to empty)
- Arithmetic expansion: `$((expr))` with `+`, `-`, `*`, `/`, `%`
- Backslash escaping: `\char`
- Comments: `# comment`
- Compound commands: `if/then/elif/else/fi`, `while/do/done`, `until/do/done`, `for/in/do/done`
- Subshells: `( ... )`
- Brace groups: `{ ...; }`
- Function definitions: `name() { ... }`, `function name { ... }`

### Not Yet Implemented
- `case/esac`
- `select`
- `[[ ... ]]` (extended test)
- Brace expansion: `{a,b,c}`
- Tilde expansion: `~`
- Process substitution: `<(cmd)`, `>(cmd)`
- Coprocesses
- Job control (`&`, `fg`, `bg`, `jobs`)
- Signal handling (`trap`)
- Here-strings: `<<<`
- Glob/pathname expansion: `*`, `?`, `[...]`

## Builtins

| Command    | Status | Notes |
|------------|--------|-------|
| `:`        | Done   | No-op, returns 0 |
| `true`     | Done   | Returns 0 |
| `false`    | Done   | Returns 1 |
| `echo`     | Done   | Supports `-n` |
| `printf`   | Done   | `%s`, `%d`, `%%`, `\n`, `\t`, `\\` |
| `pwd`      | Done   | Prints working directory |
| `cd`       | Done   | Supports `cd -`, `cd` (HOME) |
| `export`   | Done   | `export NAME=VALUE`, `export NAME` |
| `unset`    | Done   | Removes variable; respects readonly |
| `readonly` | Done   | `readonly NAME=VALUE`, `readonly NAME` |

## Utilities

| Command | Status | Notes |
|---------|--------|-------|
| `cat`   | Done   | File concatenation, here-doc stdin |
| `ls`    | Done   | Directory listing |
| `mkdir` | Done   | Create directories |
| `rm`    | Done   | Remove files |
| `touch` | Done   | Create empty files |
| `head`  | Done   | First N lines (`-n N`) |
| `tail`  | Done   | Last N lines (`-n N`) |
| `wc`    | Done   | Line/word/byte counts |

## Non-Goals

- Not a BusyBox port or Bash fork
- No real OS processes in the browser
- No kernel/network admin tools
- No TTY/terminal emulation
- No job control or signal handling
