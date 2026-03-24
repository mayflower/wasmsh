# Bash Shell — Complete Reference

Comprehensive reference of all Bash shell features, commands, expressions, and syntax.
Based on Bash 5.x (GNU Bash Reference Manual).

---

## Table of Contents

1. [Shell Grammar & Syntax](#1-shell-grammar--syntax)
2. [Quoting](#2-quoting)
3. [Parameters and Variables](#3-parameters-and-variables)
4. [Shell Expansions](#4-shell-expansions)
5. [Redirections](#5-redirections)
6. [Arithmetic](#6-arithmetic)
7. [Conditional Expressions](#7-conditional-expressions)
8. [Pattern Matching](#8-pattern-matching)
9. [Compound Commands](#9-compound-commands)
10. [Builtin Commands](#10-builtin-commands)
11. [Shell Variables](#11-shell-variables)
12. [Job Control](#12-job-control)
13. [Signals and Traps](#13-signals-and-traps)
14. [Shell Options](#14-shell-options)
15. [Readline / Line Editing](#15-readline--line-editing)
16. [Programmable Completion](#16-programmable-completion)
17. [History](#17-history)
18. [Restricted Shell](#18-restricted-shell)
19. [POSIX Mode](#19-posix-mode)
20. [Common External Commands](#20-common-external-commands)

---

## 1. Shell Grammar & Syntax

### Simple Commands

```
[var=value ...] command [arguments ...] [redirections ...]
```

- The first word (after variable assignments) is the command name.
- Variable assignments preceding a command affect only that command's environment (unless builtin/function).
- Exit status: command's return value, 128+N if killed by signal N, 127 if not found, 126 if not executable.

### Pipelines

```
[time [-p]] [!] command1 [ | command2 ] ...
[time [-p]] [!] command1 [ |& command2 ] ...
```

- `|` connects stdout of command1 to stdin of command2.
- `|&` is shorthand for `2>&1 |` — connects both stdout and stderr.
- Each command runs in its own subshell (except with `lastpipe` shopt).
- Exit status is the last command, unless `pipefail` is set (rightmost non-zero).
- `!` negates the exit status.
- `time` reports timing statistics; `-p` uses POSIX format. Controlled by `TIMEFORMAT`.

### Lists

```
command1 ; command2          # Sequential execution
command1 & command2          # command1 runs asynchronously (background)
command1 && command2         # AND: command2 runs only if command1 succeeds (exit 0)
command1 || command2         # OR: command2 runs only if command1 fails (exit non-zero)
```

- `;` and `&` have equal precedence, lower than `&&` and `||`.
- `&&` and `||` have equal precedence, left-associative.

### Coprocesses

```
coproc [NAME] command [redirections]
```

- Executes `command` asynchronously with a two-way pipe.
- Creates array `NAME` (default `COPROC`): `NAME[0]` = fd for coprocess stdout, `NAME[1]` = fd for coprocess stdin.
- PID stored in `NAME_PID`.
- Only one coprocess may be active at a time.

### Function Definitions

```
name () compound-command [redirections]
function name [()] compound-command [redirections]
```

- `function` keyword is a Bash extension; `name ()` is POSIX.
- Executed in the current shell (not a subshell).
- Positional parameters set to function arguments.
- `FUNCNAME`, `BASH_SOURCE`, `BASH_LINENO` arrays updated.
- `return [n]` exits the function. `local` creates dynamically-scoped variables.
- Exportable with `export -f name`. Recursion limited by `FUNCNEST`.

### Comments

```
# This is a comment (from # to end of line)
```

- `#` must be the first character of a word.
- Controlled by `interactive_comments` shopt.

---

## 2. Quoting

### Backslash (Escape Character)

```
\c
```

- Preserves literal value of next character, except newline.
- `\newline` is line continuation (backslash + newline removed).

### Single Quotes

```
'string'
```

- Preserves literal value of every character. No escapes, no expansions.
- A single quote cannot appear inside single quotes.

### Double Quotes

```
"string"
```

- Preserves literal value except: `$`, `` ` ``, `\`, `!` (history), `"`.
- `$` and `` ` `` retain special meaning (expansion, command substitution).
- `\` only special before `$`, `` ` ``, `"`, `\`, or newline.
- `$@` and `$*` have special behavior inside double quotes.

### ANSI-C Quoting

```
$'string'
```

Escape sequences:

| Escape | Meaning |
|--------|---------|
| `\a` | Alert (bell) |
| `\b` | Backspace |
| `\e`, `\E` | Escape (0x1B) |
| `\f` | Form feed |
| `\n` | Newline |
| `\r` | Carriage return |
| `\t` | Horizontal tab |
| `\v` | Vertical tab |
| `\\` | Backslash |
| `\'` | Single quote |
| `\"` | Double quote |
| `\?` | Question mark |
| `\nnn` | Octal (1–3 digits) |
| `\xHH` | Hex (1–2 digits) |
| `\uHHHH` | Unicode U+HHHH (1–4 hex digits) |
| `\UHHHHHHHH` | Unicode U+HHHHHHHH (1–8 hex digits) |
| `\cx` | Control-x |

### Locale-Specific Translation

```
$"string"
```

- Translated via `gettext` according to current locale.
- In `C`/`POSIX` locale, treated as double-quoted.

---

## 3. Parameters and Variables

### Positional Parameters

```
$1, $2, ..., $9, ${10}, ${11}, ...
```

- Set on invocation, by `set`, or by function calls.
- `${N}` required for N > 9.
- Cannot be assigned directly; use `set` or `shift`.

### Special Parameters

| Parameter | Description |
|-----------|-------------|
| `$*` | All positional params as single word. `"$*"` → joined by first char of `IFS`. |
| `$@` | All positional params as separate words. `"$@"` → each param as separate word. |
| `$#` | Number of positional parameters. |
| `$?` | Exit status of last foreground pipeline. |
| `$-` | Current option flags (from `set` or invocation). |
| `$$` | PID of current shell (unchanged in subshells). |
| `$!` | PID of most recent background job. |
| `$0` | Name of shell or script. |
| `$_` | Last argument of previous command (after expansion). At startup: absolute path of shell/script. |

### Named Variables

```
name=value
name+=value              # Append (string concat; arithmetic add for -i)
declare -i name=value    # Integer attribute
declare -l name=value    # Lowercase on assignment
declare -u name=value    # Uppercase on assignment
declare -r name=value    # Readonly
declare -x name=value    # Export
declare -n name=other    # Nameref (reference to other)
declare -t name          # Trace (DEBUG trap on function calls)
declare -g name=value    # Global scope (within functions)
declare -I name          # Inherit from surrounding scope (local vars)
```

- Names: `[a-zA-Z_][a-zA-Z0-9_]*`
- Unset variables expand to empty (unless `set -u`).

### Indexed Arrays

```
name=(value0 value1 value2 ...)
name=([0]=value0 [3]=value3 ...)    # Sparse
name[index]=value
declare -a name
```

- Zero-indexed. Index is arithmetic expression.
- `${name[index]}` — element. `${name[@]}` / `${name[*]}` — all elements.
- `${!name[@]}` — all indices. `${#name[@]}` — count.
- `name+=(value ...)` — append. `unset 'name[index]'` — remove element.

### Associative Arrays

```
declare -A name
name=([key1]=value1 [key2]=value2 ...)
name[key]=value
```

- Keys are arbitrary strings. Must declare with `declare -A`.
- `${name[key]}` — element. `${!name[@]}` — all keys.

---

## 4. Shell Expansions

Processing order: (1) brace → (2) tilde, parameter, arithmetic, command substitution (left-to-right) → (3) word splitting → (4) filename expansion → (5) quote removal. Process substitution simultaneous with step 2.

### 4.1 Brace Expansion

```
{a,b,c}                  # → a b c
{a..z}                    # → a b c ... z
{1..10}                   # → 1 2 3 ... 10
{01..10}                  # → 01 02 ... 10 (zero-padded)
{1..10..2}                # → 1 3 5 7 9 (increment)
{a..z..3}                 # → a d g j m p s v y
pre{a,b,c}post            # → preapost prebpost precpost
{a,b}{1,2}                # → a1 a2 b1 b2
```

- Not performed in POSIX mode.
- Performed before all other expansions.

### 4.2 Tilde Expansion

| Syntax | Expansion |
|--------|-----------|
| `~` | `$HOME` |
| `~/path` | `$HOME/path` |
| `~user` | Home directory of `user` |
| `~+` | `$PWD` |
| `~-` | `$OLDPWD` |
| `~N`, `~+N`, `~-N` | Directory stack entries |

### 4.3 Parameter Expansion

#### Basic

| Syntax | Description |
|--------|-------------|
| `$name` / `${name}` | Value of variable. |
| `${!name}` | Indirect expansion (expand name, then use as var name). |
| `${!prefix*}` / `${!prefix@}` | All variable names starting with `prefix`. |
| `${!name[@]}` / `${!name[*]}` | All keys/indices of array. |
| `${#name}` | Length in characters (or element count for `@`/`*`). |

#### Default / Assignment / Error / Alternative

| Syntax | Description |
|--------|-------------|
| `${var:-default}` | If unset or null → `default`. |
| `${var-default}` | If unset (not null) → `default`. |
| `${var:=default}` | If unset or null → assign `default`, expand. |
| `${var=default}` | If unset → assign `default`, expand. |
| `${var:?message}` | If unset or null → error with `message`. |
| `${var?message}` | If unset → error with `message`. |
| `${var:+alternate}` | If set and non-null → `alternate`. |
| `${var+alternate}` | If set (even null) → `alternate`. |

The `:` variants test both unset and null; without `:`, only unset.

#### Substring / Offset

```
${var:offset}             # Substring from offset
${var:offset:length}      # Substring from offset, length chars
```

- Zero-based. Negative offset counts from end: `${var: -3}` or `${var:(-3)}`.
- Negative length excludes from end: `${var:0:-2}`.
- For `@`/`*`: offset relative to `$0`/`$1`. Arrays: slice of elements.

#### Pattern Removal (Trimming)

| Syntax | Description |
|--------|-------------|
| `${var#pattern}` | Remove shortest match from beginning. |
| `${var##pattern}` | Remove longest match from beginning. |
| `${var%pattern}` | Remove shortest match from end. |
| `${var%%pattern}` | Remove longest match from end. |

Pattern uses glob syntax. Applied per-element on arrays.

#### Pattern Substitution

| Syntax | Description |
|--------|-------------|
| `${var/pattern/replacement}` | Replace first match. |
| `${var//pattern/replacement}` | Replace all matches. |
| `${var/#pattern/replacement}` | Replace if matches at beginning. |
| `${var/%pattern/replacement}` | Replace if matches at end. |

Omit replacement to delete. Glob syntax. Per-element on arrays.

#### Case Modification

| Syntax | Description |
|--------|-------------|
| `${var^pattern}` | Uppercase first char matching `pattern` (default `?`). |
| `${var^^pattern}` | Uppercase all matching chars. |
| `${var,pattern}` | Lowercase first char matching `pattern`. |
| `${var,,pattern}` | Lowercase all matching chars. |

#### Transformation Operators (Bash 4.4+)

```
${parameter@operator}
```

| Operator | Description |
|----------|-------------|
| `U` | Uppercase entire value. |
| `u` | Uppercase first character. |
| `L` | Lowercase entire value. |
| `Q` | Quote value for reuse as input. |
| `E` | Expand backslash escapes (like `$'...'`). |
| `P` | Expand as prompt string (like PS1). |
| `A` | Produce `declare` command to recreate variable. |
| `K` | (Bash 5.1+) Quoted key-value pairs for associative arrays. |
| `a` | Produce variable's attribute flags. |

### 4.4 Command Substitution

```
$(command)
`command`                # Legacy (avoid)
$(<file)                 # Read file without spawning process (Bash optimization)
```

- Runs in subshell, substitutes stdout. Trailing newlines stripped.
- `$(...)` nests naturally.

### 4.5 Arithmetic Expansion

```
$(( expression ))
```

- Evaluates as integer arithmetic. Variables used without `$` prefix.
- See [Section 6](#6-arithmetic) for operators.

### 4.6 Process Substitution

```
<(command)               # Command output as readable file
>(command)               # Writable file feeding into command
```

- Requires `/dev/fd` or named pipes. No space between `<`/`>` and `(`.
- Common: `diff <(cmd1) <(cmd2)`.

### 4.7 Word Splitting

- After parameter/command/arithmetic expansion (outside double quotes), results split on `IFS`.
- `IFS` default: space, tab, newline.
- IFS whitespace at edges ignored; sequences = single delimiter.
- Non-whitespace IFS chars are standalone delimiters; adjacent ones produce empty fields.
- `IFS=""` (null) → no splitting. Unset → default behavior.
- Explicit null args (`""`, `''`) preserved. Implicit nulls from unset params removed.

### 4.8 Filename Expansion (Globbing)

| Pattern | Matches |
|---------|---------|
| `*` | Any string (not leading `.`) |
| `?` | Any single char (not leading `.`) |
| `[abc]` | Any one of enclosed chars |
| `[a-z]` | Any char in range |
| `[!abc]` / `[^abc]` | Any char NOT in set |
| `[[:class:]]` | POSIX character class |

POSIX classes: `[:alnum:]`, `[:alpha:]`, `[:ascii:]`, `[:blank:]`, `[:cntrl:]`, `[:digit:]`, `[:graph:]`, `[:lower:]`, `[:print:]`, `[:punct:]`, `[:space:]`, `[:upper:]`, `[:xdigit:]`

- Files starting with `.` require explicit `.` or `dotglob`.
- No match → literal (unless `failglob` or `nullglob`).
- Disabled by `set -f` / `noglob`.
- `GLOBIGNORE` excludes patterns. `nocaseglob` for case-insensitive.
- `globstar`: `**` matches recursively.

### 4.9 Extended Globbing (extglob)

Enabled with `shopt -s extglob`.

| Pattern | Matches |
|---------|---------|
| `?(pattern-list)` | Zero or one occurrence. |
| `*(pattern-list)` | Zero or more occurrences. |
| `+(pattern-list)` | One or more occurrences. |
| `@(pattern-list)` | Exactly one of the patterns. |
| `!(pattern-list)` | Anything NOT matching. |

`pattern-list`: patterns separated by `|`. Can be nested.

### 4.10 Quote Removal

After all other expansions, unquoted `\`, `'`, `"` not from expansions are removed.

---

## 5. Redirections

File descriptors: 0 = stdin, 1 = stdout, 2 = stderr. `N` is optional fd number.

| Syntax | Description |
|--------|-------------|
| `[N]< file` | Input: open for reading on fd N (default 0). |
| `[N]> file` | Output: open for writing on fd N (default 1). Creates/truncates. Fails with `noclobber`. |
| `[N]>> file` | Append: open for appending on fd N (default 1). |
| `[N]>| file` | Clobber override (overrides `noclobber`). |
| `[N]<> file` | Input/output: open for read+write on fd N (default 0). No truncate. |
| `&> file` | Redirect stdout + stderr (= `> file 2>&1`). |
| `&>> file` | Append stdout + stderr (= `>> file 2>&1`). |
| `[N]<& fd` | Duplicate input fd. |
| `[N]>& fd` | Duplicate output fd. |
| `[N]<& -` | Close input fd N (default 0). |
| `[N]>& -` | Close output fd N (default 1). |
| `[N]<& fd-` | Move input fd (dup + close source). |
| `[N]>& fd-` | Move output fd (dup + close source). |

### Here Documents

```
[N]<<[-] DELIMITER
    content with $expansion
    ...
DELIMITER
```

- Reads until line containing only `DELIMITER`.
- Quoted delimiter (`'DELIM'`, `"DELIM"`, `\DELIM`) → no expansion in body.
- Unquoted → parameter expansion, command substitution, arithmetic expansion.
- `<<-` strips leading tabs from body and delimiter.

### Here Strings

```
[N]<<< word
```

- `word` undergoes tilde, parameter, command, arithmetic expansion + quote removal.
- Result supplied as string with trailing newline on stdin.
- No word splitting or globbing.

### Order Matters

```
command > file 2>&1      # Both stdout and stderr → file
command 2>&1 > file      # stderr → terminal, stdout → file
```

### Special Device Files (Bash-internal)

- `/dev/fd/N` — duplicate fd N
- `/dev/stdin` — fd 0
- `/dev/stdout` — fd 1
- `/dev/stderr` — fd 2
- `/dev/tcp/host/port` — TCP connection
- `/dev/udp/host/port` — UDP connection

### exec for fd Manipulation

```
exec 3< file             # Open for reading on fd 3
exec 4> file             # Open for writing on fd 4
exec 3<&-                # Close fd 3
exec 5<>/dev/tcp/h/p     # Open TCP connection
```

---

## 6. Arithmetic

Used in `$(( ))`, `(( ))`, `let`, `declare -i`, array indices, `${name[expr]}`.

### Operators (lowest to highest precedence)

| Operator | Description |
|----------|-------------|
| `expr , expr` | Comma (evaluate both, return last) |
| `var = expr` | Assignment |
| `var op= expr` | Compound: `*=`, `/=`, `%=`, `+=`, `-=`, `<<=`, `>>=`, `&=`, `^=`, `\|=` |
| `expr ? expr : expr` | Ternary conditional |
| `expr \|\| expr` | Logical OR |
| `expr && expr` | Logical AND |
| `expr \| expr` | Bitwise OR |
| `expr ^ expr` | Bitwise XOR |
| `expr & expr` | Bitwise AND |
| `expr == expr`, `!= expr` | Equality / inequality |
| `< > <= >=` | Relational |
| `expr << expr`, `>> expr` | Bitwise shift |
| `expr + expr`, `- expr` | Addition / subtraction |
| `expr * expr`, `/ expr`, `% expr` | Mul / div / mod |
| `expr ** expr` | Exponentiation (right-associative) |
| `! expr` | Logical NOT |
| `~ expr` | Bitwise NOT |
| `++ var`, `-- var` | Pre-increment / decrement |
| `var ++`, `var --` | Post-increment / decrement |
| `+ expr`, `- expr` | Unary plus / minus |

### Number Bases

```
0x1F          # Hexadecimal
0177          # Octal (leading 0)
0b1010        # Binary
base#n        # Arbitrary base (2–64): 2#1010, 8#77, 16#ff, 64#@_
```

- Integers (signed, typically 64-bit). Division by zero is an error.
- Variables used without `$` prefix. Overflow wraps silently.

---

## 7. Conditional Expressions

Used in `test`, `[ ]`, and `[[ ]]`.

### File Test Operators

| Operator | True if… |
|----------|----------|
| `-a file` | Exists (deprecated; use `-e`). |
| `-b file` | Block special device. |
| `-c file` | Character special device. |
| `-d file` | Directory. |
| `-e file` | Exists (any type). |
| `-f file` | Regular file. |
| `-g file` | Set-group-ID bit set. |
| `-h file` | Symbolic link (= `-L`). |
| `-k file` | Sticky bit set. |
| `-p file` | Named pipe (FIFO). |
| `-r file` | Readable. |
| `-s file` | Size > 0. |
| `-t fd` | fd is open and terminal. |
| `-u file` | Set-user-ID bit set. |
| `-w file` | Writable. |
| `-x file` | Executable (searchable if directory). |
| `-G file` | Owned by effective group. |
| `-L file` | Symbolic link. |
| `-N file` | Modified since last read. |
| `-O file` | Owned by effective user. |
| `-S file` | Socket. |

### File Comparison

| Operator | True if… |
|----------|----------|
| `f1 -ef f2` | Same device and inode (hard links). |
| `f1 -nt f2` | f1 newer than f2. |
| `f1 -ot f2` | f1 older than f2. |

### String Operators

| Operator | True if… |
|----------|----------|
| `-z string` | Length is zero. |
| `-n string` / `string` | Length is non-zero. |
| `s1 = s2` | Equal (POSIX). |
| `s1 == s2` | Equal (Bash; glob pattern RHS in `[[ ]]`). |
| `s1 != s2` | Not equal (glob pattern RHS in `[[ ]]`). |
| `s1 < s2` | s1 sorts before s2 (locale). |
| `s1 > s2` | s1 sorts after s2. |
| `s1 =~ regex` | **`[[ ]]` only.** Regex match. Captures in `BASH_REMATCH`. |

**`[[ ]]` notes:** `==`/`!=` do glob matching on RHS. Quote RHS for literal. `=~` uses POSIX ERE; do NOT quote the pattern.

### Integer Comparison

| Operator | True if… |
|----------|----------|
| `-eq` | Equal |
| `-ne` | Not equal |
| `-lt` | Less than |
| `-le` | Less or equal |
| `-gt` | Greater than |
| `-ge` | Greater or equal |

### Logical

**In `test` / `[ ]`:**

| Operator | Description |
|----------|-------------|
| `! expr` | NOT |
| `expr -a expr` | AND (deprecated) |
| `expr -o expr` | OR (deprecated) |
| `\( expr \)` | Grouping (escaped parens) |

**In `[[ ]]`:**

| Operator | Description |
|----------|-------------|
| `! expr` | NOT |
| `expr && expr` | AND |
| `expr \|\| expr` | OR |
| `( expr )` | Grouping (no escaping) |

### Variable Tests (Bash 4.2+/4.4+)

| Operator | True if… |
|----------|----------|
| `-v varname` | Variable is set. |
| `-R varname` | Variable is a nameref. |

---

## 8. Pattern Matching

### Basic Glob

| Pattern | Matches |
|---------|---------|
| `*` | Any string (zero or more chars). |
| `?` | Any single character. |
| `[...]` | Any one enclosed char. |
| `[^...]` / `[!...]` | Any char NOT in set. |
| `[a-z]` | Range. |
| `[[:class:]]` | POSIX character class. |

### Extended Globs (extglob)

| Pattern | Matches |
|---------|---------|
| `?(pat\|pat\|...)` | Zero or one. |
| `*(pat\|pat\|...)` | Zero or more. |
| `+(pat\|pat\|...)` | One or more. |
| `@(pat\|pat\|...)` | Exactly one. |
| `!(pat\|pat\|...)` | Not any. |

### Regex in `[[ ]]`

```
[[ string =~ regex ]]
```

- POSIX Extended Regular Expressions. Captures: `BASH_REMATCH[0]` = match, `[1]`... = groups.
- Don't quote the regex. Store in variable for complex patterns: `re='pat'; [[ $s =~ $re ]]`.

### case Pattern Matching

```
case word in
    pattern1 | pattern2) commands ;;     # break
    pattern3) commands ;&               # fall-through (4.0+)
    pattern4) commands ;;&              # continue testing (4.0+)
    *) default ;;
esac
```

---

## 9. Compound Commands

### if / elif / else

```bash
if command-list; then
    command-list
[elif command-list; then
    command-list] ...
[else
    command-list]
fi
```

### for (word list)

```bash
for name [in word ...]; do
    command-list
done
```

If `in word ...` omitted, iterates over `"$@"`.

### for (C-style arithmetic)

```bash
for (( expr1; expr2; expr3 )); do
    command-list
done
```

### while

```bash
while command-list; do
    command-list
done
```

### until

```bash
until command-list; do
    command-list
done
```

### case

```bash
case word in
    [(] pattern [| pattern] ...) command-list ;;
    ...
esac
```

Terminators: `;;` (break), `;&` (fall-through, 4.0+), `;;&` (continue testing, 4.0+).

### select

```bash
select name [in word ...]; do
    command-list
done
```

- Prints numbered menu to stderr. Prompts with `PS3`.
- User input → `REPLY`. Matching word → `name`.
- `break` exits. Omit `in word ...` for `"$@"`.

### Arithmetic Evaluation

```bash
(( expression ))
```

Returns 0 if expression non-zero, 1 if zero. Equivalent to `let "expression"`.

### Conditional Expression

```bash
[[ expression ]]
```

- No word splitting or globbing inside.
- `&&`, `||`, `()`, `!` for logic.
- `==`/`!=` do glob matching; `=~` does regex.

### Group Command (Braces)

```bash
{ command-list; }
```

Executes in current shell. Semicolon before `}` required. Space after `{` required.

### Subshell

```bash
( command-list )
```

Executes in a subshell. Variable changes don't affect parent.

---

## 10. Builtin Commands

### 10.1 Bourne Shell / POSIX Builtins

| Builtin | Description |
|---------|-------------|
| `:` | Null command. Returns 0. |
| `.` / `source` | Execute file in current shell. Searches `PATH` if no `/`. |
| `break [n]` | Exit innermost (or nth) loop. |
| `continue [n]` | Skip to next iteration of innermost (or nth) loop. |
| `eval [args]` | Concatenate args, execute as command. |
| `exec [cmd [args]]` | Replace shell with cmd. Without cmd, redirections apply to shell. |
| `exit [n]` | Exit with status n (default: last command). |
| `export [-fn] [-p] [name[=val]]` | Mark for export. `-f` functions. `-n` remove. `-p` list. |
| `getopts optstring name [args]` | Parse options. `:` prefix suppresses errors. `OPTARG`, `OPTIND`. |
| `hash [-dlr] [-p file] [-t name]` | Command location cache. `-r` clear. `-d` forget. |
| `pwd [-LP]` | Print working directory. `-L` logical, `-P` physical. |
| `readonly [-aAf] [-p] [name[=val]]` | Mark readonly. `-a` array, `-A` assoc, `-f` function. |
| `return [n]` | Return from function/source with status n. |
| `set [opts] [args]` | Shell options and positional params. See [Section 14](#14-shell-options). |
| `shift [n]` | Shift positional params left by n (default 1). |
| `test expr` / `[ expr ]` | Evaluate conditional. See [Section 7](#7-conditional-expressions). |
| `times` | Print accumulated user/system CPU times. |
| `trap [-lp] [action] [signal ...]` | Signal handlers. See [Section 13](#13-signals-and-traps). |
| `umask [-pS] [mode]` | File creation mask. |
| `unset [-fvn] [name ...]` | Remove variables (`-v`), functions (`-f`), nameref (`-n`). |

### 10.2 Bash-Specific Builtins

| Builtin | Description |
|---------|-------------|
| `alias [-p] [name[=val]]` | Define/display aliases. |
| `bind [opts]` | Readline key bindings and variables. |
| `builtin name [args]` | Execute builtin, bypassing functions. |
| `caller [expr]` | Function call context (line, func, file). |
| `command [-pVv] cmd [args]` | Execute bypassing functions. `-v` type. `-V` verbose. |
| `compgen [opts] [word]` | Generate completion matches. |
| `complete [opts] name ...` | Define completions. See [Section 16](#16-programmable-completion). |
| `compopt [-o opt] [+o opt] [name]` | Modify completion options. |
| `declare` / `typeset` | Declare variables with attributes (`-aAfFgiIlnrtux`). |
| `dirs [-clpv] [+N] [-N]` | Display/manage directory stack. |
| `disown [-ahr] [jobspec]` | Remove from job table. `-h` suppress SIGHUP. |
| `echo [-neE] [args]` | Output. `-n` no newline. `-e` escapes. `-E` no escapes. |
| `enable [-adnps] [-f file] [name]` | Enable/disable builtins. `-n` disable. `-f` load. |
| `fc [-e editor] [-lnr] [first] [last]` | List/edit/re-execute history. |
| `fg [jobspec]` | Bring to foreground. |
| `bg [jobspec ...]` | Resume in background. |
| `help [-dms] [pattern]` | Builtin help. |
| `history` | Command history manipulation. `-c` clear, `-a/-n/-r/-w` file ops. |
| `jobs [-lnprs] [jobspec]` | List jobs. |
| `kill [-s sig] [pid\|jobspec]` | Send signal. `-l` list. Default SIGTERM. |
| `let arg [arg ...]` | Evaluate arithmetic. 0 if non-zero result, 1 if zero. |
| `local [-aAfFgiIlnrtux] [name[=val]]` | Declare local variable in function. Dynamic scoping. |
| `logout [n]` | Exit login shell. |
| `mapfile` / `readarray` | Read lines into array. `-d delim -n count -O origin -s skip -t -u fd`. |
| `popd [-n] [+N\|-N]` | Remove from directory stack. |
| `printf [-v var] format [args]` | Formatted output. `%s %d %f %x %o %b %q %(fmt)T`. |
| `pushd [-n] [+N\|-N\|dir]` | Push onto directory stack + cd. |
| `read [-ers] [-a arr] [-d delim] [-n N] [-N N] [-p prompt] [-t timeout] [-u fd] [name ...]` | Read line. |
| `shopt [-pqsu] [-o] [optname]` | Shell options. See [Section 14](#14-shell-options). |
| `suspend [-f]` | Suspend shell (SIGSTOP). |
| `type [-afptP] [name ...]` | Describe command type. |
| `ulimit [-HS] [-abcdefiklmnpqrstuvxPRT] [limit]` | Resource limits. |
| `unalias [-a] [name ...]` | Remove aliases. |
| `wait [-fn] [-p var] [id ...]` | Wait for jobs. `-n` any one. `-f` force. |

### 10.3 POSIX Special Builtins

Variable assignments persist after these; errors exit non-interactive shells:

`break`, `:`, `.`, `continue`, `eval`, `exec`, `exit`, `export`, `readonly`, `return`, `set`, `shift`, `trap`, `unset`

---

## 11. Shell Variables

### 11.1 POSIX Variables

| Variable | Description |
|----------|-------------|
| `CDPATH` | Search path for `cd` with relative paths. |
| `HOME` | Home directory. Used by `cd`, tilde expansion. |
| `IFS` | Field separator (default: space, tab, newline). |
| `MAIL` | Mail file for checking. |
| `MAILPATH` | Colon-separated mail files (overrides `MAIL`). |
| `OPTARG` | Last `getopts` option argument. |
| `OPTIND` | Next `getopts` argument index. |
| `PATH` | Command search path. |
| `PS1` | Primary prompt. |
| `PS2` | Continuation prompt (default `> `). |

### 11.2 Bash Variables

| Variable | Description |
|----------|-------------|
| `BASH` | Pathname of current Bash. |
| `BASHOPTS` | Enabled `shopt` options (read-only). |
| `BASHPID` | PID (updates in subshells, unlike `$$`). |
| `BASH_ALIASES` | Associative array of aliases. |
| `BASH_ARGC` | Parameter count per call frame (extdebug). |
| `BASH_ARGV` | All parameters in call stack (extdebug). |
| `BASH_ARGV0` | (5.0+) Assignable `$0`. |
| `BASH_CMDS` | Command hash table (assoc array). |
| `BASH_COMMAND` | Command being executed. |
| `BASH_COMPAT` | Compatibility level (e.g. `42`, `50`). |
| `BASH_ENV` | File sourced at startup of non-interactive shells. |
| `BASH_EXECUTION_STRING` | Argument to `-c`. |
| `BASH_LINENO` | Line numbers in call stack. |
| `BASH_LOADABLES_PATH` | Path for loadable builtins. |
| `BASH_REMATCH` | Regex match results from `=~` (read-only). |
| `BASH_SOURCE` | Source files in call stack. |
| `BASH_SUBSHELL` | Subshell nesting level. |
| `BASH_VERSINFO` | Version array (read-only). |
| `BASH_VERSION` | Version string. |
| `BASH_XTRACEFD` | fd for `set -x` output (default 2). |
| `CHILD_MAX` | Max remembered child exit statuses. |
| `COLUMNS` | Terminal width. |
| `COMP_CWORD` | Cursor word index during completion. |
| `COMP_KEY` | Key that triggered completion. |
| `COMP_LINE` | Current command line during completion. |
| `COMP_POINT` | Cursor position during completion. |
| `COMP_TYPE` | Completion type (9=normal, 63=list, etc). |
| `COMP_WORDBREAKS` | Word separator chars for completion. |
| `COMP_WORDS` | Words on command line during completion. |
| `COMPREPLY` | Completion results array. |
| `COPROC` | Coprocess fd array. |
| `DIRSTACK` | Directory stack array. |
| `EMACS` | Set to `t` in Emacs shell buffer. |
| `ENV` | Sourced in POSIX-mode interactive shells. |
| `EPOCHREALTIME` | (5.0+) Seconds since epoch with μs precision. |
| `EPOCHSECONDS` | (5.0+) Seconds since epoch (integer). |
| `EUID` | Effective user ID (read-only). |
| `EXECIGNORE` | Patterns to ignore in PATH searches. |
| `FCEDIT` | Default editor for `fc`. |
| `FIGNORE` | Suffixes to ignore in filename completion. |
| `FUNCNAME` | Function call stack array. |
| `FUNCNEST` | Max function nesting depth. |
| `GLOBIGNORE` | Patterns excluded from globbing. |
| `GROUPS` | Group IDs (read-only). |
| `histchars` | History expansion chars (default `!^#`). |
| `HISTCMD` | History number of current command. |
| `HISTCONTROL` | History saving control (ignorespace/dups/both/erasedups). |
| `HISTFILE` | History file (default `~/.bash_history`). |
| `HISTFILESIZE` | Max lines in HISTFILE. |
| `HISTIGNORE` | Patterns to exclude from history. |
| `HISTSIZE` | Max in-memory history entries. |
| `HISTTIMEFORMAT` | strftime format for history timestamps. |
| `HOSTNAME` | Current hostname. |
| `HOSTTYPE` | Architecture (e.g. `x86_64`). |
| `IGNOREEOF` | EOF count before exit. |
| `INPUTRC` | Readline init file (default `~/.inputrc`). |
| `INSIDE_EMACS` | Emacs shell buffer version info. |
| `LANG` | Default locale. |
| `LC_ALL` | Overrides all LC_* and LANG. |
| `LC_COLLATE` | Collation order. |
| `LC_CTYPE` | Character classification. |
| `LC_MESSAGES` | Message language. |
| `LC_NUMERIC` | Number formatting. |
| `LINENO` | Current line number. |
| `LINES` | Terminal height. |
| `MACHTYPE` | System type (`cpu-company-system`). |
| `MAILCHECK` | Mail check interval (default 60s). |
| `MAPFILE` | Default array for `mapfile`. |
| `OLDPWD` | Previous working directory. |
| `OPTERR` | getopts error messages (1=on). |
| `OSTYPE` | OS type (e.g. `linux-gnu`, `darwin`). |
| `PIPESTATUS` | Array of pipeline exit statuses. |
| `POSIXLY_CORRECT` | Enables POSIX mode. |
| `PPID` | Parent PID (read-only). |
| `PROMPT_COMMAND` | Executed before PS1. Array in 5.1+. |
| `PROMPT_DIRTRIM` | Trim directory components in `\w`. |
| `PS0` | (4.4+) Displayed after reading command, before execution. |
| `PS3` | `select` prompt (default `#? `). |
| `PS4` | Debug prompt (default `+ `). First char replicated for nesting. |
| `PWD` | Current directory. |
| `RANDOM` | Random 0–32767. Assignable to seed. |
| `READLINE_LINE` | Readline buffer (in `bind -x`). |
| `READLINE_MARK` | Mark position in readline. |
| `READLINE_POINT` | Cursor position in readline. |
| `REPLY` | Default for `read` and `select`. |
| `SECONDS` | Seconds since shell start. Assignable to reset. |
| `SHELL` | User's login shell path. |
| `SHELLOPTS` | Enabled `set -o` options (read-only). |
| `SHLVL` | Shell nesting level. |
| `SRANDOM` | (5.1+) 32-bit random, better source. Not seedable. |
| `TIMEFORMAT` | Format for `time`. `%R` real, `%U` user, `%S` system, `%P` CPU%. |
| `TMOUT` | Inactivity timeout. Also default `read` timeout. |
| `TMPDIR` | Temp directory. |
| `UID` | Real user ID (read-only). |
| `USER` | Current username. |

### Prompt Escape Sequences

| Escape | Meaning |
|--------|---------|
| `\a` | Bell |
| `\d` | Date: "Weekday Month Day" |
| `\D{fmt}` | strftime format |
| `\e` | Escape (033) |
| `\h` | Hostname to first `.` |
| `\H` | Full hostname |
| `\j` | Job count |
| `\l` | Terminal basename |
| `\n` | Newline |
| `\r` | Carriage return |
| `\s` | Shell name |
| `\t` | Time HH:MM:SS (24h) |
| `\T` | Time HH:MM:SS (12h) |
| `\@` | Time HH:MM AM/PM |
| `\A` | Time HH:MM (24h) |
| `\u` | Username |
| `\v` | Version major.minor |
| `\V` | Version major.minor.patch |
| `\w` | Working directory (~) |
| `\W` | Basename of cwd |
| `\!` | History number |
| `\#` | Command number |
| `\$` | `#` if root, else `$` |
| `\nnn` | Octal character |
| `\\` | Literal backslash |
| `\[` | Begin non-printing |
| `\]` | End non-printing |

---

## 12. Job Control

### Job Specifications

| Spec | Meaning |
|------|---------|
| `%n` | Job number n |
| `%string` | Job starting with string |
| `%?string` | Job containing string |
| `%%` / `%+` | Current job |
| `%-` | Previous job |

### Commands

| Command | Description |
|---------|-------------|
| `jobs [-lnprs]` | List jobs. `-l` PID. `-p` PID only. `-n` changed. `-r` running. `-s` stopped. |
| `fg [jobspec]` | Bring to foreground. |
| `bg [jobspec ...]` | Resume in background. |
| `kill [-s sig] pid\|jobspec` | Send signal. |
| `wait [id ...]` | Wait for completion. |
| `wait -n [-p var]` | Wait for any one. |
| `disown [-ahr] [jobspec]` | Remove from table. `-h` no SIGHUP. |
| `suspend [-f]` | Suspend shell. |

### Settings

- Enabled with `set -m` / `set -o monitor` (default in interactive).
- `set -b` / `notify`: report status immediately.
- `checkjobs` shopt: warn before exit with active jobs.

---

## 13. Signals and Traps

### trap Command

```
trap 'commands' SIGNAL ...     # Set handler
trap '' SIGNAL ...             # Ignore signal
trap - SIGNAL ...              # Reset to default
trap -l                        # List all signals
trap -p [SIGNAL ...]           # Print handlers
```

### Common Signals

| Signal | # | Default | Description |
|--------|---|---------|-------------|
| `SIGHUP` | 1 | Term | Hangup |
| `SIGINT` | 2 | Term | Interrupt (Ctrl-C) |
| `SIGQUIT` | 3 | Core | Quit (Ctrl-\) |
| `SIGILL` | 4 | Core | Illegal instruction |
| `SIGTRAP` | 5 | Core | Trace trap |
| `SIGABRT` | 6 | Core | Abort |
| `SIGFPE` | 8 | Core | Floating-point exception |
| `SIGKILL` | 9 | Term | Kill (uncatchable) |
| `SIGSEGV` | 11 | Core | Segfault |
| `SIGPIPE` | 13 | Term | Broken pipe |
| `SIGALRM` | 14 | Term | Alarm |
| `SIGTERM` | 15 | Term | Termination |
| `SIGUSR1` | 10 | Term | User-defined 1 |
| `SIGUSR2` | 12 | Term | User-defined 2 |
| `SIGCHLD` | 17 | Ignore | Child status changed |
| `SIGCONT` | 18 | Cont | Continue |
| `SIGSTOP` | 19 | Stop | Stop (uncatchable) |
| `SIGTSTP` | 20 | Stop | Terminal stop (Ctrl-Z) |
| `SIGTTIN` | 21 | Stop | Background read |
| `SIGTTOU` | 22 | Stop | Background write |
| `SIGWINCH` | 28 | Ignore | Window resize |

### Pseudo-Signals (Bash)

| Signal | Description |
|--------|-------------|
| `EXIT` (0) | On shell exit. |
| `ERR` | On non-zero exit (like `errexit`). Inherit: `set -E`. |
| `DEBUG` | Before every simple command. Inherit: `set -T`. |
| `RETURN` | After function/source returns. Requires `set -T` or `extdebug`. |

### Notes

- Traps not inherited in subshells (except ignored signals).
- Signals ignored at entry cannot be trapped.
- `set -E`: ERR inherited by functions/subshells.
- `set -T`: DEBUG/RETURN inherited.

---

## 14. Shell Options

### 14.1 set Options

Set: `set -o name` / `set -X`. Unset: `set +o name` / `set +X`.

| Short | `-o` name | Description |
|-------|-----------|-------------|
| `-a` | `allexport` | Auto-export all variables. |
| `-b` | `notify` | Report background status immediately. |
| `-e` | `errexit` | Exit on non-zero (with exceptions: if/while conditions, &&/\|\|, !). |
| `-f` | `noglob` | Disable globbing. |
| `-h` | `hashall` | Hash command locations (default on). |
| `-k` | `keyword` | All assignments in environment. |
| `-m` | `monitor` | Job control (default interactive). |
| `-n` | `noexec` | Syntax check only. |
| `-p` | `privileged` | Restricted startup. |
| `-t` | `onecmd` | Exit after one command. |
| `-u` | `nounset` | Error on unset variables. |
| `-v` | `verbose` | Print input lines. |
| `-x` | `xtrace` | Print expanded commands (prefix: `$PS4`). |
| `-B` | `braceexpand` | Brace expansion (default on). |
| `-C` | `noclobber` | Prevent `>` overwrite. Use `>\|` to override. |
| `-E` | `errtrace` | ERR trap inherited. |
| `-H` | `histexpand` | `!` history (default interactive). |
| `-P` | `physical` | No symlink resolution in cd/pwd. |
| `-T` | `functrace` | DEBUG/RETURN traps inherited. |
| | `emacs` | Emacs editing (default). |
| | `history` | Command history (default interactive). |
| | `ignoreeof` | Don't exit on EOF. |
| | `pipefail` | Pipeline fails on rightmost non-zero. |
| | `posix` | POSIX mode. |
| | `vi` | Vi editing. |

### 14.2 shopt Options

Set: `shopt -s name`. Unset: `shopt -u name`.

| Option | Default | Description |
|--------|---------|-------------|
| `assoc_expand_once` | off | (5.0+) Expand assoc subscripts once. |
| `autocd` | off | cd into directory names. |
| `cdable_vars` | off | cd treats arg as variable. |
| `cdspell` | off | Autocorrect cd typos. |
| `checkhash` | off | Verify hashed commands exist. |
| `checkjobs` | off | Warn on exit with jobs. |
| `checkwinsize` | on | Update LINES/COLUMNS. |
| `cmdhist` | on | Multi-line → one history entry. |
| `compat31`–`compat44` | off | Compatibility modes. |
| `complete_fullquote` | on | Quote metacharacters in completion. |
| `direxpand` | off | Expand dirs in completion. |
| `dirspell` | off | Spell-correct dirs in completion. |
| `dotglob` | off | Glob includes dotfiles. |
| `execfail` | off | Don't exit on exec failure. |
| `expand_aliases` | on(i) | Alias expansion. |
| `extdebug` | off | Extended debugging. |
| `extglob` | on | Extended glob: `?()`, `*()`, `+()`, `@()`, `!()`. |
| `extquote` | on | `$'...'`/`$"..."` inside `${}`. |
| `failglob` | off | Error on no glob match. |
| `force_fignore` | on | FIGNORE even if only match. |
| `globasciiranges` | on | ASCII ordering for ranges. |
| `globskipdots` | on | (5.2+) Never match `.`/`..`. |
| `globstar` | off | `**` recursive matching. |
| `gnu_errfmt` | off | GNU error format. |
| `histappend` | off | Append to HISTFILE (not overwrite). |
| `histreedit` | off | Re-edit failed history subs. |
| `histverify` | off | Load history sub for editing. |
| `hostcomplete` | on | Hostname completion on `@`. |
| `huponexit` | off | SIGHUP all jobs on exit. |
| `inherit_errexit` | off | (4.4+) Subshells inherit errexit. |
| `interactive_comments` | on | Allow `#` comments. |
| `lastpipe` | off | Last pipe cmd in current shell. |
| `lithist` | off | Multi-line with newlines (not `;`). |
| `localvar_inherit` | off | (5.0+) Local inherits value from parent scope. |
| `localvar_unset` | off | (5.0+) Scoped unset behavior. |
| `login_shell` | varies | Is login shell (read-only). |
| `mailwarn` | off | Warn if mail read. |
| `no_empty_cmd_completion` | off | Don't complete on empty line. |
| `nocaseglob` | off | Case-insensitive globbing. |
| `nocasematch` | off | Case-insensitive case/[[. |
| `noexpand_translation` | off | (5.2+) Don't expand `$"..."`. |
| `nullglob` | off | No-match → nothing. |
| `patsub_replacement` | on | (5.2+) `&` = match in `${var/pat/rep}`. |
| `progcomp` | on | Programmable completion. |
| `progcomp_alias` | off | Completion for aliases. |
| `promptvars` | on | Expand variables in prompts. |
| `restricted_shell` | varies | Is restricted (read-only). |
| `shift_verbose` | off | Error on over-shift. |
| `sourcepath` | on | Use PATH for source/`.`. |
| `varredir_close` | off | (5.1+) Auto-close `{var}` redirection fds. |
| `xpg_echo` | off | echo interprets escapes. |

---

## 15. Readline / Line Editing

### Modes

- **Emacs** (default): `set -o emacs`
- **Vi**: `set -o vi`

### Configuration

File: `~/.inputrc` (or `$INPUTRC`)

```
set variable value
"keyseq": function-name
"keyseq": "macro-string"
$if mode=emacs
$endif
$include /etc/inputrc
```

### Key Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `bell-style` | audible | none/visible/audible |
| `colored-stats` | off | Color completions by type |
| `completion-ignore-case` | off | Case-insensitive completion |
| `completion-map-case` | off | `-` and `_` equivalent |
| `editing-mode` | emacs | emacs or vi |
| `enable-bracketed-paste` | on | (5.1+) Bracketed paste |
| `expand-tilde` | off | Expand tilde in completion |
| `history-preserve-point` | off | Keep cursor in history nav |
| `mark-directories` | on | Append `/` to dirs |
| `show-all-if-ambiguous` | off | List all immediately |
| `show-mode-in-prompt` | off | Vi mode indicator |
| `visible-stats` | off | File type indicators |

### Key Emacs Bindings

| Key | Function |
|-----|----------|
| Ctrl-A | beginning-of-line |
| Ctrl-E | end-of-line |
| Ctrl-F / Ctrl-B | forward/backward char |
| Meta-F / Meta-B | forward/backward word |
| Ctrl-L | clear-screen |
| Ctrl-D | delete-char (or EOF) |
| Ctrl-K | kill-line (to end) |
| Ctrl-U | unix-line-discard (to start) |
| Ctrl-W | unix-word-rubout |
| Meta-D | kill-word (forward) |
| Ctrl-Y | yank (paste) |
| Meta-Y | yank-pop (cycle) |
| Ctrl-T | transpose-chars |
| Meta-U/L/C | upcase/downcase/capitalize word |
| Ctrl-P / Ctrl-N | previous/next history |
| Ctrl-R / Ctrl-S | reverse/forward search |
| TAB | complete |
| Ctrl-X Ctrl-E | edit in $EDITOR |
| Ctrl-_ | undo |
| Meta-. | yank-last-arg |

### Vi Mode

- Starts in insert mode. ESC → command mode.
- Command mode: vi-style movement (h/j/k/l/w/b/e/0/$), editing (x/d/c/y/p).
- `v` opens line in `$VISUAL`/`$EDITOR`.

---

## 16. Programmable Completion

### complete

```
complete [-abcdefgjksuv] [-o opt] [-DEI] [-A action] [-G globpat]
         [-W wordlist] [-F function] [-C command] [-X filterpat]
         [-P prefix] [-S suffix] name [name ...]
```

**Actions (`-A`):** alias, arrayvar, binding, builtin, command, directory, disabled, enabled, export, file, function, group, helptopic, hostname, job, keyword, running, service, setopt, shopt, signal, stopped, user, variable

**Shorthands:** `-a`=alias, `-b`=builtin, `-c`=command, `-d`=directory, `-e`=export, `-f`=file, `-g`=group, `-j`=job, `-k`=keyword, `-s`=service, `-u`=user, `-v`=variable

**Options (`-o`):** bashdefault, default, dirnames, filenames, noquote, nosort, nospace, plusdirs

**Special:** `-D` default, `-E` empty line, `-I` initial word

**`-F function`:** Function gets `$1`=cmd, `$2`=word, `$3`=preceding word. Sets `COMPREPLY`.

**`-C command`:** Output lines become completions.

**`-G globpat`:** Glob expanded for completions.

**`-W wordlist`:** Split and matched for completions.

**`-X filterpat`:** Filter completions (! negates).

**`-P prefix` / `-S suffix`:** Added to each completion.

### compgen

```
compgen [options] [word]
```

Same options as `complete`. Generates and prints matches.

### compopt

```
compopt [-o option] [+o option] [name ...]
```

Modify completion options for existing specs or current completion.

---

## 17. History

### History Expansion

Trigger: `!` (with `set -o histexpand`, default in interactive).

#### Event Designators

| Designator | Description |
|------------|-------------|
| `!!` | Previous command. |
| `!n` | Command #n. |
| `!-n` | n commands back. |
| `!string` | Most recent starting with string. |
| `!?string[?]` | Most recent containing string. |
| `^old^new^` | Quick sub on last command. |
| `!#` | Current line so far. |

#### Word Designators

Separated by `:` from event (`:` optional with `^$*-%`).

| Designator | Description |
|------------|-------------|
| `0` | Command (zeroth word). |
| `n` | nth word. |
| `^` | First argument (word 1). |
| `$` | Last argument. |
| `%` | Word from `?string?` search. |
| `x-y` | Range. `-y` = `0-y`. |
| `*` | All except zeroth. |
| `x*` | From x to $. |
| `x-` | From x to $-1. |

#### Modifiers

Preceded by `:`. Chainable.

| Mod | Description |
|-----|-------------|
| `h` | Head (dirname). |
| `t` | Tail (basename). |
| `r` | Remove trailing suffix. |
| `e` | Extension only. |
| `p` | Print, don't execute. |
| `q` | Quote. |
| `x` | Quote and break into words. |
| `s/old/new/` | Substitute first. `&` = match. |
| `&` | Repeat last substitution. |
| `g` / `a` | Global modifier. |
| `G` | Apply `s` once per word. |

### History Builtin

```
history [n]              # Show entries
history -c               # Clear
history -d offset        # Delete entry
history -a [file]        # Append new to file
history -n [file]        # Read new from file
history -r [file]        # Read entire file
history -w [file]        # Write to file
history -p arg ...       # Expand, print
history -s arg ...       # Add without executing
```

---

## 18. Restricted Shell

Invoked as `rbash`, `bash -r`, or `bash --restricted`.

### Restrictions (after startup files)

- No `cd`
- No setting/unsetting `SHELL`, `PATH`, `HISTFILE`, `ENV`, `BASH_ENV`
- No commands with `/` in name
- No `/` in source/`.` filenames
- No output redirection (`>`, `>>`, `>&`, `&>`, `>|`)
- No `exec`
- No `enable -f` (loading)
- No `command -p`
- Cannot disable restricted mode

**Notes:** Startup files run unrestricted. Not a security sandbox (circumvented via awk, python, etc.). Subshells are NOT restricted.

---

## 19. POSIX Mode

Enabled: `set -o posix`, `bash --posix`, `POSIXLY_CORRECT`, invoked as `sh`.

### Key Differences

1. Only `$ENV` sourced at startup (no .bashrc, .bash_profile).
2. Reserved words cannot be aliased.
3. POSIX special builtins found before functions.
4. Special builtin errors exit non-interactive shell.
5. Variable assignments before special builtins persist.
6. `echo` doesn't interpret escapes (unless `xpg_echo`).
7. `printf` lacks `%q`, `%Q`, `%(fmt)T`.
8. Process substitution unavailable.
9. `.` must find file via PATH; error if not found.
10. Subshells inherit `set -e`.
11. Function names must be valid identifiers.
12. Default `HISTFILE` is `~/.sh_history`.
13. `command` doesn't prevent assignment expansion.
14. Tilde expansion only before command name assignments.

---

## 20. Common External Commands

These are not bash builtins but are commonly used in shell scripts. They are separate executables typically found in `/usr/bin` or `/bin`.

### File Operations

| Command | Description |
|---------|-------------|
| `cat` | Concatenate and display files. |
| `cp` | Copy files and directories. `-r` recursive, `-p` preserve. |
| `mv` | Move/rename files. |
| `rm` | Remove files. `-r` recursive, `-f` force. |
| `ln` | Create links. `-s` symbolic. |
| `touch` | Create file or update timestamp. |
| `mkdir` | Create directories. `-p` parents. |
| `rmdir` | Remove empty directories. |
| `chmod` | Change permissions. Octal or symbolic (u+x, g-w). |
| `chown` | Change owner/group. |
| `chgrp` | Change group. |
| `stat` | Display file status/metadata. |
| `file` | Determine file type. |
| `find` | Search for files. `-name`, `-type`, `-exec`, `-mtime`, etc. |
| `locate` / `mlocate` | Find files by name (database). |
| `readlink` | Print symlink target. `-f` canonical. |
| `realpath` | Resolve to absolute path. |
| `mktemp` | Create temporary file/directory. `-d` directory. |
| `install` | Copy files and set attributes. |
| `dd` | Convert and copy files (block-level). |
| `df` | Disk free space. `-h` human-readable. |
| `du` | Disk usage. `-sh` summary human-readable. |
| `mount` / `umount` | Mount/unmount filesystems. |
| `tar` | Archive files. `-czf` create gzip, `-xzf` extract gzip. |
| `gzip` / `gunzip` | Compress/decompress. |
| `bzip2` / `bunzip2` | Compress/decompress (bz2). |
| `xz` / `unxz` | Compress/decompress (xz). |
| `zip` / `unzip` | Zip archives. |

### Text Processing

| Command | Description |
|---------|-------------|
| `grep` | Search text with patterns. `-i` case-insensitive, `-r` recursive, `-E` extended regex, `-v` invert, `-c` count, `-l` files only, `-n` line numbers. |
| `sed` | Stream editor. `s/old/new/g` substitute, `-i` in-place, `-n` suppress, `-e` multiple expressions. |
| `awk` | Pattern scanning and processing. `'{print $1}'` field extraction, `-F` delimiter. |
| `sort` | Sort lines. `-n` numeric, `-r` reverse, `-k` key field, `-u` unique, `-t` delimiter. |
| `uniq` | Remove/report duplicates. `-c` count, `-d` duplicates only, `-u` unique only. |
| `cut` | Extract columns. `-d` delimiter, `-f` fields, `-c` characters. |
| `tr` | Translate/delete characters. `tr 'a-z' 'A-Z'`, `-d` delete, `-s` squeeze. |
| `head` | First N lines. `-n N`. |
| `tail` | Last N lines. `-n N`, `-f` follow, `-n +N` from line N. |
| `wc` | Count lines/words/bytes. `-l` lines, `-w` words, `-c` bytes. |
| `tee` | Read stdin, write to stdout and files. `-a` append. |
| `paste` | Merge lines of files. `-d` delimiter. |
| `join` | Join sorted files on common field. |
| `comm` | Compare sorted files line by line. |
| `diff` | Compare files. `-u` unified, `-r` recursive, `--color`. |
| `patch` | Apply diff patches. |
| `column` | Format into columns. `-t` table. |
| `fold` | Wrap lines at width. `-w N`, `-s` break at spaces. |
| `fmt` | Reformat paragraphs. |
| `nl` | Number lines. |
| `expand` / `unexpand` | Tabs ↔ spaces. |
| `rev` | Reverse lines. |
| `strings` | Extract printable strings from binary. |
| `iconv` | Character encoding conversion. |

### Data Utilities

| Command | Description |
|---------|-------------|
| `seq` | Generate number sequences. `seq 1 10`, `seq 1 2 10`. |
| `expr` | Evaluate expressions. Arithmetic, string, regex. |
| `bc` | Arbitrary precision calculator. `echo "scale=2; 22/7" \| bc`. |
| `basename` | Strip directory and suffix. |
| `dirname` | Strip last component. |
| `date` | Display/set date. `+%Y-%m-%d` format, `-d` parse string. |
| `sleep` | Pause for N seconds. Supports `s/m/h/d` suffixes. |
| `yes` | Repeatedly output string (default "y"). |
| `tput` | Terminal capabilities. `tput cols`, `tput lines`, `tput setaf N`. |
| `numfmt` | Format/parse numbers. `--to=iec`. |
| `factor` | Print prime factors. |
| `shuf` | Random permutation. `-n` count, `-i` range. |

### System Information

| Command | Description |
|---------|-------------|
| `uname` | System info. `-a` all, `-r` release, `-m` machine. |
| `hostname` | Show/set hostname. |
| `whoami` | Current username. |
| `id` | User identity. `-u` uid, `-g` gid, `-n` name. |
| `groups` | User's groups. |
| `who` / `w` | Logged-in users. |
| `uptime` | System uptime and load. |
| `env` | Display/set environment. |
| `printenv` | Print environment variables. |
| `lsb_release` | Distribution info. |
| `arch` | Machine architecture. |

### Process Management

| Command | Description |
|---------|-------------|
| `ps` | Process list. `ps aux`, `ps -ef`. |
| `top` / `htop` | Interactive process monitor. |
| `kill` | Send signal. `kill -9 PID`, `kill -TERM PID`. |
| `killall` | Kill by name. |
| `pkill` / `pgrep` | Signal/find by pattern. |
| `nohup` | Run immune to hangups. |
| `nice` / `renice` | Set/change priority. |
| `timeout` | Run with time limit. |
| `xargs` | Build commands from stdin. `-I{}` placeholder, `-P N` parallel, `-0` null-delimited. |
| `wait` | Wait for process. |
| `watch` | Execute periodically. `-n N` interval. |
| `crontab` | Schedule tasks. `-e` edit, `-l` list. |
| `at` | Schedule one-time job. |
| `flock` | File locking. |
| `lsof` | List open files. |
| `strace` / `ltrace` | Trace system/library calls. |

### Networking

| Command | Description |
|---------|-------------|
| `curl` | Transfer data. `-o` output, `-s` silent, `-L` follow, `-X` method, `-H` header, `-d` data. |
| `wget` | Download files. `-O` output, `-q` quiet, `-r` recursive. |
| `ssh` | Secure shell. `-p` port, `-i` key, `-L/-R` tunnel. |
| `scp` | Secure copy. `-r` recursive, `-P` port. |
| `rsync` | Sync files. `-avz` archive+verbose+compress, `--delete`. |
| `ping` | ICMP echo. `-c N` count. |
| `traceroute` | Trace network path. |
| `netstat` / `ss` | Network statistics/sockets. |
| `dig` / `nslookup` / `host` | DNS lookup. |
| `ifconfig` / `ip` | Network interfaces. |
| `nc` / `netcat` | Network utility. `-l` listen. |
| `openssl` | SSL/TLS utility. Certificates, encryption. |

### Text Search & Manipulation (Advanced)

| Command | Description |
|---------|-------------|
| `jq` | JSON processor. `.key`, `.[0]`, `select()`, `map()`. |
| `yq` | YAML processor. |
| `xmllint` | XML parser/validator. |
| `perl` | Perl one-liners. `perl -pe 's/old/new/g'`, `-i` in-place. |
| `python3 -c` | Python one-liners. |
| `ruby -e` | Ruby one-liners. |

### Version Control

| Command | Description |
|---------|-------------|
| `git` | Version control. add, commit, push, pull, branch, merge, rebase, log, diff, stash. |
| `svn` | Subversion. |

### Package Management

| Command | Description |
|---------|-------------|
| `apt` / `apt-get` | Debian/Ubuntu packages. install, remove, update, upgrade. |
| `yum` / `dnf` | RHEL/Fedora packages. |
| `pacman` | Arch packages. |
| `brew` | macOS packages. |
| `pip` / `pip3` | Python packages. |
| `npm` / `npx` | Node.js packages. |
| `cargo` | Rust packages. |
| `gem` | Ruby packages. |

### Container & Cloud

| Command | Description |
|---------|-------------|
| `docker` | Container management. run, build, exec, ps, logs, compose. |
| `kubectl` | Kubernetes management. get, apply, describe, logs, exec. |
| `aws` | AWS CLI. s3, ec2, lambda, iam. |
| `gcloud` | Google Cloud CLI. |
| `az` | Azure CLI. |
| `terraform` | Infrastructure as code. init, plan, apply. |
| `ansible` | Configuration management. |

### Misc Utilities

| Command | Description |
|---------|-------------|
| `which` / `type` | Locate command. |
| `whereis` | Locate binary/source/manpage. |
| `man` | Manual pages. |
| `info` | GNU info pages. |
| `alias` / `unalias` | Shell aliases. |
| `screen` / `tmux` | Terminal multiplexers. |
| `md5sum` / `sha256sum` | Checksums. |
| `base64` | Base64 encode/decode. |
| `xxd` | Hex dump. |
| `od` | Octal dump. |
| `true` / `false` | Return 0 / 1. |
| `test` / `[` | Conditional evaluation. |
| `getopt` / `getopts` | Parse options. |
| `envsubst` | Substitute environment variables in text. |
| `m4` | Macro processor. |
| `make` | Build automation. |

---

## Appendix: Bash Invocation

### Invocation Options

| Option | Description |
|--------|-------------|
| `-c string` | Execute commands from string. |
| `-i` | Interactive shell. |
| `-l` / `--login` | Login shell. |
| `-r` / `--restricted` | Restricted shell. |
| `-s` | Read commands from stdin. |
| `-v` / `--verbose` | Print input lines. |
| `-x` / `--xtrace` | Print commands after expansion. |
| `-D` / `--dump-strings` | Print `$"..."` strings. |
| `--debugger` | Arrange for debugger profile. |
| `--init-file file` | Read file instead of ~/.bashrc. |
| `--noediting` | Disable readline. |
| `--noprofile` | Skip login files. |
| `--norc` | Skip ~/.bashrc. |
| `--posix` | POSIX mode. |

### Startup File Order

**Interactive login shell:**
1. `/etc/profile`
2. First found of: `~/.bash_profile`, `~/.bash_login`, `~/.profile`
3. On logout: `~/.bash_logout`

**Interactive non-login shell:**
1. `~/.bashrc`

**Non-interactive shell:**
1. `$BASH_ENV` (if set)

**Invoked as `sh`:**
- Login: `/etc/profile`, `~/.profile`
- Non-login interactive: `$ENV`
- POSIX mode behaviors apply

---

*Reference: [GNU Bash Manual](https://www.gnu.org/software/bash/manual/bash.html), Bash 5.x*
