# Shell Syntax Reference

Complete reference of shell syntax supported by wasmsh. For the canonical
"what's done / what's not" matrix see [`SUPPORTED.md`](../../SUPPORTED.md)
at the repository root.

## Commands

### Simple Commands

```sh
command arg1 arg2 ...
VAR=value command args    # environment prefix
```

### Pipelines

```sh
cmd1 | cmd2 | cmd3       # stdout of each feeds stdin of next
cmd1 |& cmd2             # stdout and stderr of cmd1 feed stdin of cmd2
! pipeline                # negate exit status
```

`|&` is shorthand for `2>&1 |` — both stdout and stderr are piped.

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
for (( init; cond; step )); do body; done   # C-style arithmetic loop
```

The C-style `for (( ... ))` loop evaluates `init` once, tests `cond` before
each iteration (exits when zero), and evaluates `step` after each body.

### case

```sh
case word in
  pattern1) body ;;          # break after match
  pattern2) body ;&          # fall through to next clause (no re-test)
  pattern3) body ;;&         # continue testing remaining patterns
  pattern4 | pattern5) body ;;
  *) default ;;
esac
```

Patterns support all glob operators including extglob patterns when
`extglob` is enabled.

### select

```sh
select name [in word ...]; do
  body
done
```

Prints a numbered menu to stderr, reads a choice from stdin, sets `name` to
the chosen word and `REPLY` to the raw input. Loops until `break` or EOF.
Without `in words`, iterates over `"$@"`.

### Arithmetic Command

```sh
(( expression ))
```

Evaluates `expression` as arithmetic. Returns exit status 0 if the result is
non-zero, 1 if zero. Supports all operators listed in the Arithmetic section.

### Extended Test

```sh
[[ expression ]]
```

Evaluates `expression` without word splitting or pathname expansion on
unquoted variables. Supports:

| Operator | Meaning |
|----------|---------|
| `str == pattern` | Glob match (right side unquoted = glob) |
| `str != pattern` | Glob non-match |
| `str =~ regex` | POSIX ERE match; captures in `BASH_REMATCH` |
| `str < str` | Lexicographic less-than |
| `str > str` | Lexicographic greater-than |
| `expr1 && expr2` | Logical and (short-circuit) |
| `expr1 \|\| expr2` | Logical or (short-circuit) |
| `! expr` | Logical negation |

All `test`/`[` operators (`-f`, `-z`, `-n`, `-eq`, etc.) are also supported.

`BASH_REMATCH[0]` holds the full match; `BASH_REMATCH[1]`, `[2]`, ... hold
capture groups.

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
| `$"text"` | Locale quoting — passthrough in sandbox (treated as `"text"`) |

## Expansions

Performed in this order:

1. **Brace expansion**: `{a,b,c}`, `{1..10}`, `{1..10..2}`
2. **Tilde expansion**: `~` → `$HOME`
3. **Parameter expansion**: `$var`, `${var}`, operators below
4. **Command substitution**: `$(command)`
5. **Arithmetic expansion**: `$((expr))`
6. **Word splitting**: on IFS (default: space, tab, newline)
7. **Pathname expansion**: `*`, `?`, `[...]`, extglob, globstar
8. **Quote removal**

### Brace Expansion

```sh
{a,b,c}          # → a b c
{1..5}           # → 1 2 3 4 5
{1..10..2}       # → 1 3 5 7 9  (step)
{a..e}           # → a b c d e
{z..a..-2}       # → z x v t r p n l j h f d b  (negative step)
pre{A,B}suf      # → preAsuf preBsuf
{a,b}{1,2}       # → a1 a2 b1 b2  (cartesian product when adjacent)
{a,b}-{c,d}      # → a-c a-d b-c b-d
```

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
| `${var/#pat/rep}` | Replace match anchored at start |
| `${var/%pat/rep}` | Replace match anchored at end |
| `${var:offset}` | Substring from offset |
| `${var:offset:length}` | Substring with length |
| `${var^}` | Uppercase first character |
| `${var^^}` | Uppercase all characters |
| `${var,}` | Lowercase first character |
| `${var,,}` | Lowercase all characters |
| `${!name}` | Indirect expansion — expand `name`, then use result as variable name |
| `${!prefix*}` | Names of all variables whose name starts with `prefix` (space-separated) |
| `${!prefix@}` | Same, but quoted separately when in `"..."` |
| `${var@Q}` | Quoted form of `var` suitable for re-input |
| `${var@E}` | Expand escape sequences in `var` (like `$'...'`) |
| `${var@U}` | Uppercase value of `var` |
| `${var@L}` | Lowercase value of `var` |
| `${var@u}` | Uppercase first character of `var` |
| `${var@a}` | Attribute flags of `var` |

### Special Parameters

| Parameter | Meaning |
|-----------|---------|
| `$?` | Exit status of last command |
| `$#` | Number of positional parameters |
| `$@` | All positional parameters (separate words) |
| `$*` | All positional parameters (single word) |
| `$0` | Shell name (`wasmsh`) |
| `$1`..`$9`, `${10}` | Positional parameters |

### Dynamic Variables

These variables are evaluated on every read rather than stored as fixed
values. See [Sandbox and capabilities](sandbox-and-capabilities.md#recognised-environment-variables)
for the full list and the variables that hosts can configure.

| Variable | Meaning |
|----------|---------|
| `$RANDOM` | 16-bit value from an XorShift PRNG. Writable to reseed. |
| `$LINENO` | Current source line being executed. |
| `$SECONDS` | Seconds since shell init. Writable to reset the origin. |
| `$FUNCNAME` | Current function name (within a function). |
| `$BASH_SOURCE` | Current source file (within a `source`d file). |
| `$PIPESTATUS` | Indexed array of exit codes from the most recent pipeline. |
| `$BASH_REMATCH` | Capture groups from the most recent `[[ … =~ … ]]` match. `[0]` is the full match. |

### Not Yet Implemented

The following constructs are recognised parser-side but are not yet
honoured by the runtime, or are not implemented at all:

- Coprocesses: `coproc`
- Background execution semantics: `cmd &` is parsed but the runtime
  executes synchronously (the sandbox has no process table)
- Signal handling beyond `EXIT` and `ERR` traps
- The `time` keyword
- `$$`, `$!`, `$_`, `$-`, `$BASHPID` special parameters

For an authoritative list see the "Not Yet Implemented" section of
[`SUPPORTED.md`](../../SUPPORTED.md).

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

Supported in `$((...))` and `((...))`:

### Operators (high to low precedence)

| Category | Operators |
|----------|-----------|
| Postfix | `x++`, `x--` |
| Prefix unary | `++x`, `--x`, `+x`, `-x`, `!`, `~` |
| Exponentiation | `x ** y` (right-associative) |
| Multiplicative | `*`, `/`, `%` |
| Additive | `+`, `-` |
| Bitwise shift | `<<`, `>>` |
| Comparison | `<`, `>`, `<=`, `>=` |
| Equality | `==`, `!=` |
| Bitwise AND | `&` |
| Bitwise XOR | `^` |
| Bitwise OR | `\|` |
| Logical AND | `&&` |
| Logical OR | `\|\|` |
| Ternary | `cond ? then : else` |
| Assignment | `=`, `+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `\|=`, `^=`, `<<=`, `>>=`, `**=` |
| Comma | `,` (evaluate both, yield right) |

### Literals

- Decimal: `42`
- Hexadecimal: `0xFF`, `0xff`
- Octal: `0755`
- Binary: `2#1010`, `0b1010`
- Arbitrary base: `N#digits` (e.g. `16#FF`, `36#zz`)

### Notes

- Variables inside `$((...))` do not need a `$` prefix.
- Division by zero yields 0.
- All arithmetic is 64-bit signed integer.

## Pathname Expansion (Globbing)

### Basic Globs

| Pattern | Matches |
|---------|---------|
| `*` | Any string (not crossing `/`, not dotfiles by default) |
| `?` | Any single character |
| `[abc]` | Character class |
| `[a-z]` | Character range |
| `[!abc]` | Negated character class |

### Globstar

Enabled with `shopt -s globstar`.

| Pattern | Matches |
|---------|---------|
| `**` | Zero or more directories (recursive) |
| `**/file` | `file` anywhere in the tree |
| `src/**/*.rs` | All `.rs` files under `src/` recursively |

### Extended Globbing

Enabled with `shopt -s extglob` (on by default in wasmsh).

| Pattern | Matches |
|---------|---------|
| `?(pat)` | Zero or one occurrence of `pat` |
| `*(pat)` | Zero or more occurrences of `pat` |
| `+(pat)` | One or more occurrences of `pat` |
| `@(pat)` | Exactly one occurrence of `pat` |
| `!(pat)` | Anything that does not match `pat` |

Multiple alternatives separated by `|` are supported: `*(foo|bar)`.

## See Also

- [`SUPPORTED.md`](../../SUPPORTED.md) — canonical compatibility matrix.
- [Builtins reference](builtins.md) — commands that operate on the language constructs above.
- [Utilities reference](utilities.md) — in-process commands that the syntax dispatches to.
- [Sandbox and capabilities](sandbox-and-capabilities.md) — environment variables, dynamic variables, and what the sandbox enforces.
- [Architecture: Pipeline](../explanation/architecture.md#pipeline-overview) — how source text becomes events.
