# Shell Syntax Reference

Complete reference of shell syntax supported by wasmsh.

## Commands

### Simple Commands

```sh
command arg1 arg2 ...
VAR=value command args    # environment prefix
```

### Pipelines

```sh
cmd1 | cmd2 | cmd3       # stdout of each feeds stdin of next
! pipeline                # negate exit status
```

### Lists

```sh
cmd1 ; cmd2              # sequential
cmd1 && cmd2             # run cmd2 only if cmd1 succeeds
cmd1 || cmd2             # run cmd2 only if cmd1 fails
```

## Compound Commands

### if / elif / else

```sh
if condition; then
  body
elif condition; then
  body
else
  body
fi
```

### while / until

```sh
while condition; do body; done
until condition; do body; done
```

### for

```sh
for var in word1 word2 ...; do body; done
for var; do body; done           # iterates over "$@"
```

### case

```sh
case word in
  pattern1) body ;;
  pattern2 | pattern3) body ;;
  *) default ;;
esac
```

### Grouping

```sh
{ cmd1; cmd2; }    # brace group (same scope)
( cmd1; cmd2 )     # subshell (isolated scope)
```

### Functions

```sh
name() { body; }           # POSIX
function name { body; }    # Bash
```

## Quoting

| Syntax | Behavior |
|--------|----------|
| `'text'` | Literal — no expansion |
| `"text"` | Allows `$`, `` ` ``, `\` expansion |
| `\char` | Escape next character |
| `$'text'` | ANSI-C escapes: `\n`, `\t`, `\xNN`, `\0NNN` |

## Expansions

Performed in this order:

1. **Brace expansion**: `{a,b,c}`, `{1..10}`
2. **Tilde expansion**: `~` → `$HOME`
3. **Parameter expansion**: `$var`, `${var}`, operators below
4. **Command substitution**: `$(command)`
5. **Arithmetic expansion**: `$((expr))`
6. **Word splitting**: on IFS (default: space, tab, newline)
7. **Pathname expansion**: `*`, `?`, `[...]`
8. **Quote removal**

### Parameter Expansion Operators

| Operator | Meaning |
|----------|---------|
| `${var:-word}` | Use `word` if `var` is unset or empty |
| `${var:=word}` | Assign `word` if `var` is unset or empty |
| `${var:+word}` | Use `word` if `var` is set and non-empty |
| `${var:?msg}` | Error with `msg` if `var` is unset or empty |
| `${#var}` | String length |
| `${var#pattern}` | Remove shortest prefix match |
| `${var##pattern}` | Remove longest prefix match |
| `${var%pattern}` | Remove shortest suffix match |
| `${var%%pattern}` | Remove longest suffix match |
| `${var/pat/rep}` | Replace first match |
| `${var//pat/rep}` | Replace all matches |
| `${var:offset}` | Substring from offset |
| `${var:offset:length}` | Substring with length |

### Special Parameters

| Parameter | Meaning |
|-----------|---------|
| `$?` | Exit status of last command |
| `$#` | Number of positional parameters |
| `$@` | All positional parameters (separate words) |
| `$*` | All positional parameters (single word) |
| `$0` | Shell name (`wasmsh`) |
| `$1`..`$9`, `${10}` | Positional parameters |

## Redirections

| Syntax | Meaning |
|--------|---------|
| `< file` | Redirect stdin from file |
| `> file` | Redirect stdout to file (truncate) |
| `>> file` | Append stdout to file |
| `<> file` | Open file for read/write |
| `2> file` | Redirect stderr to file |
| `2>&1` | Merge stderr into stdout |
| `&> file` | Redirect both stdout and stderr to file |
| `<<DELIM` | Here-document |
| `<<-DELIM` | Here-document with tab stripping |
| `<<<word` | Here-string |

## Arithmetic

Supported in `$((...))`:

- Operators: `+`, `-`, `*`, `/`, `%`
- Precedence: `*`/`/`/`%` bind tighter than `+`/`-`
- Variables: `$((x + 1))` (no `$` needed inside)
- Division by zero returns 0
