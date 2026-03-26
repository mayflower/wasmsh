# wasmsh Utility Flag Audit

Systematic audit of all 86 utilities against POSIX/GNU standard flags.
For each utility, "Supported" lists flags actually parsed in the source code.
Missing flags are grouped by priority:
- **P0**: Commonly used flags that agents/developers expect
- **P1**: Useful flags that expand functionality
- **P2**: Nice-to-have completeness

> Note: wasmsh runs on a VFS with no real OS processes, so some flags
> (e.g., `tail -f`, permission bits, process signals) are inherently
> limited. These are still noted where agents would expect them.

---

## File Utilities

### cat
Supported: (none — raw passthrough of file contents, no flags parsed)
Missing P0: `-n` (number all output lines), `-b` (number non-blank lines)
Missing P1: `-s` (squeeze blank lines), `-v` (show non-printing), `-E` (show line ends)
Missing P2: `-A` (equivalent to -vET), `-T` (show tabs)

### ls
Supported: (positional arguments only — flags are consumed but not interpreted)
Missing P0: `-l` (long listing format), `-a` (show hidden files), `-la`/`-al` combined, `-1` (one file per line), `-R` (recursive)
Missing P1: `-h` (human-readable sizes), `-r` (reverse order), `-t` (sort by time), `-S` (sort by size), `-d` (list directory entries)
Missing P2: `--color` (colorized output), `-i` (show inodes), `-F` (classify with indicators)

### mkdir
Supported: `-p` / `--parents`
Missing P0: (none — `-p` is the critical flag)
Missing P1: `-m` (set permissions — no-op in VFS but should be accepted)
Missing P2: `-v` (verbose)

### rm
Supported: `-r`, `-R` (recursive), `-f` (force)
Missing P0: (none — `-rf` is the critical combo)
Missing P1: `-d` (remove empty directories), `-i` (interactive — no-op in sandbox)
Missing P2: `-v` (verbose), `--no-preserve-root`

### touch
Supported: (no flags — creates/updates file)
Missing P0: (none for basic use)
Missing P1: `-c` (do not create file if it doesn't exist), `-a` (change access time only), `-m` (change modification time only)
Missing P2: `-d` (use specified date), `-t` (use specified timestamp), `-r` (reference file)

### mv
Supported: (basic two-arg move, no flags parsed)
Missing P0: `-f` (force), `-n` (no overwrite)
Missing P1: `-i` (interactive), `-v` (verbose), `-t` (target directory)
Missing P2: `--backup`

### cp
Supported: (basic two-arg copy, no flags parsed)
Missing P0: `-r` / `-R` (recursive copy — critical for directories), `-f` (force)
Missing P1: `-n` (no overwrite), `-v` (verbose), `-a` (archive/preserve), `-p` (preserve attributes), `-t` (target directory)
Missing P2: `-l` (hard link), `-s` (symbolic link), `--backup`

### ln
Supported: (basic copy-as-link, no flags parsed)
Missing P0: `-s` (symbolic link — most common usage), `-f` (force)
Missing P1: `-n` (no-dereference), `-v` (verbose)
Missing P2: `-r` (relative)

### readlink
Supported: (basic path resolution, no flags parsed)
Missing P0: `-f` (canonicalize, follow all symlinks — most common usage)
Missing P1: `-e` (canonicalize, all components must exist), `-m` (canonicalize without requirements)
Missing P2: (none)

### realpath
Supported: (basic path resolution, no flags parsed)
Missing P0: (none — basic behavior is correct)
Missing P1: `--relative-to` (print relative path), `--relative-base`, `-e` (all must exist)
Missing P2: `-s` (no symlink resolution)

### stat
Supported: (basic file/size/type output, no flags parsed)
Missing P0: `-c` / `--format` (custom format string — heavily used by scripts)
Missing P1: `--printf` (format without trailing newline), `-f` (filesystem status)
Missing P2: `-L` (dereference)

### find
Supported: directory arg, `-name` (glob), `-type` (f/d)
Missing P0: `-exec` (execute command — extremely common), `-print` (explicit print), `-maxdepth`, `-mindepth`
Missing P1: `-delete`, `-path` / `-ipath`, `-iname` (case-insensitive), `-mtime` / `-newer`, `-size`, `-not` / `!`, `-o` (or), `-and`
Missing P2: `-perm`, `-user`, `-group`, `-empty`, `-regex`, `-prune`, `-print0`

### chmod
Supported: (no-op — VFS has no permissions)
Missing P0: (no-op is acceptable; should accept and silently ignore mode args)
Missing P1: `-R` (recursive — should be accepted)
Missing P2: `--reference`

### mktemp
Supported: `-d` (directory), template argument
Missing P0: (none — covers main use)
Missing P1: `-p` / `--tmpdir` (use directory as prefix), `-t` (use template with TMPDIR)
Missing P2: `-u` (unsafe, dry run), `--suffix`

---

## Text Utilities

### head
Supported: `-n N`, `-N` (line count)
Missing P0: `-c N` (first N bytes — commonly used)
Missing P1: `-q` (quiet, no headers), `-v` (verbose, always headers)
Missing P2: `--lines`, `--bytes`

### tail
Supported: `-n N`, `+N` (from start), `-N` (line count)
Missing P0: `-c N` (last N bytes), `-f` (follow — impossible in VFS but should be accepted with no-op)
Missing P1: `-q` (quiet), `-v` (verbose)
Missing P2: `--pid`, `--retry`, `-F` (follow by name)

### wc
Supported: `-l` (lines), `-w` (words), `-c` / `-m` (bytes/chars)
Missing P0: (none — covers the main flags)
Missing P1: `-L` (max line length)
Missing P2: `--files0-from`

### grep
Supported: `-i` (ignore case), `-v` (invert), `-c` (count), `-n` (line numbers)
Missing P0: `-r` / `-R` (recursive — extremely common), `-l` (files-with-matches), `-E` (extended regex), `-w` (word match), `-o` (only matching), `-q` (quiet)
Missing P1: `-A N` (after context), `-B N` (before context), `-C N` (context), `-F` (fixed strings), `-e` (pattern), `-f` (pattern file), `--include` / `--exclude` (file filters), `-m` (max count), `-h` (no filename), `-H` (with filename)
Missing P2: `-P` (PCRE), `-z` (null delimited), `--color`

### sed
Supported: `s/pattern/replacement/` with `g` flag, custom delimiter
Missing P0: `-i` (in-place edit — extremely common), `-e` (expression), `-n` (suppress auto-print)
Missing P1: `-f` (script file), `d` (delete command), `p` (print command), address ranges (line numbers, `/regex/`), `a`/`i`/`c` (append/insert/change), `y` (transliterate), multiple commands with `;`
Missing P2: `-E` / `-r` (extended regex), `w` (write to file), `q` (quit), hold/pattern space commands (h/H/g/G/x)

### sort
Supported: `-n` (numeric), `-r` (reverse)
Missing P0: `-u` (unique — very common), `-k` (key field — very common), `-t` (field separator)
Missing P1: `-o` (output file), `-f` (ignore case), `-s` (stable), `-V` (version sort), `-h` (human-numeric)
Missing P2: `-c` (check sorted), `-m` (merge), `-z` (null delimiter), `-b` (ignore leading blanks)

### uniq
Supported: `-c` (count)
Missing P0: `-d` (only duplicates), `-u` (only unique)
Missing P1: `-i` (ignore case), `-f N` (skip fields), `-s N` (skip chars), `-w N` (compare N chars)
Missing P2: `-z` (null delimiter)

### cut
Supported: `-d` (delimiter), `-f` (fields)
Missing P0: `-c` (character positions — common for fixed-width), field ranges (e.g., `-f1-3`, `-f2-`)
Missing P1: `--complement` (invert selection), `-s` (only lines with delimiter), `--output-delimiter`
Missing P2: `-b` (byte positions), `-z` (null delimiter)

### tr
Supported: SET1 SET2 (character translation), `-d` (delete)
Missing P0: `-s` (squeeze repeats — very common), character classes ([:upper:], [:lower:], [:digit:], etc.), character ranges (a-z)
Missing P1: `-c` / `-C` (complement), `-t` (truncate SET1)
Missing P2: (none)

### tee
Supported: `-a` (append)
Missing P0: (none — `-a` is the key flag)
Missing P1: `-i` (ignore interrupt signals)
Missing P2: `--output-error`

### paste
Supported: `-d` (delimiter), `-s` (serial), `-` (stdin)
Missing P0: (none — well covered)
Missing P1: (none)
Missing P2: `-z` (null delimiter)

### rev
Supported: (no flags — correct for this utility)
Missing P0: (none)
Missing P1: (none)
Missing P2: (none)

### column
Supported: `-t` (table mode), `-s` (input delimiter)
Missing P0: (none for basic use)
Missing P1: `-o` (output separator), `-c` (column width)
Missing P2: `-n` (disable merging), `-J` (JSON output)

---

## Data/String Utilities

### seq
Supported: `FIRST`, `FIRST LAST`, `FIRST INCREMENT LAST`
Missing P0: (none — basic positional args work)
Missing P1: `-s` (separator — default newline), `-w` (equal width), `-f` (format string)
Missing P2: (none)

### basename
Supported: PATH [SUFFIX]
Missing P0: (none — covers standard use)
Missing P1: `-a` (multiple arguments), `-s` (suffix)
Missing P2: `-z` (null delimiter)

### dirname
Supported: PATH
Missing P0: (none)
Missing P1: `-z` (null delimiter)
Missing P2: (none)

### expr
Supported: arithmetic (`+`, `-`, `*`, `/`, `%`), string comparison (`=`, `!=`)
Missing P0: (none for basic arithmetic)
Missing P1: `match` / `:` (regex match), `substr`, `index`, `length`, relational operators (`<`, `>`, `<=`, `>=`)
Missing P2: logical operators (`&`, `|`)

### xargs
Supported: default command (echo), basic stdin splitting
Missing P0: `-I {}` (replace string — extremely common), `-n N` (max args per command), `-0` / `--null` (null-delimited input)
Missing P1: `-P N` (parallel), `-L N` (max lines), `-d` (delimiter), `-p` (interactive)
Missing P2: `-t` (verbose), `--max-procs`

### yes
Supported: [STRING] (custom string)
Missing P0: (none — correct behavior)
Missing P1: (none)
Missing P2: (none)

### md5sum
Supported: FILE... (files), stdin
Missing P0: (none for basic use)
Missing P1: `-c` (check against file — commonly used in verification)
Missing P2: `--tag` (BSD-style output), `-b` (binary mode)

### sha256sum
Supported: FILE... (files), stdin
Missing P0: (none for basic use)
Missing P1: `-c` (check against file)
Missing P2: `--tag`, `-b`

### base64
Supported: `-d` / `--decode`, `-w` (wrap column)
Missing P0: (none — well covered)
Missing P1: `-i` (ignore garbage)
Missing P2: (none)

---

## System/Env Utilities

### env
Supported: (prints all exported vars, no flags parsed)
Missing P0: `-i` (start with empty environment), VAR=VALUE command (set variable and run command)
Missing P1: `-u` (unset variable), `-0` / `--null` (null-delimited output)
Missing P2: `-S` (split string into args)

### printenv
Supported: [NAME] (print specific variable)
Missing P0: (none — basic use covered)
Missing P1: `-0` (null delimiter)
Missing P2: (none)

### id
Supported: (fixed output, no flags parsed)
Missing P0: `-u` (user ID only), `-g` (group ID only), `-n` (name instead of number)
Missing P1: `-G` (all groups), `-r` (real ID)
Missing P2: (none)

### whoami
Supported: (no flags — correct)
Missing P0: (none)
Missing P1: (none)
Missing P2: (none)

### uname
Supported: `-s` (kernel name), `-a` (all), `-m` (machine), `-r` (release), `-n` (nodename)
Missing P0: (none — good coverage)
Missing P1: `-o` (operating system), `-p` (processor), `-v` (kernel version)
Missing P2: (none)

### hostname
Supported: (no flags — correct basic behavior)
Missing P0: (none)
Missing P1: `-f` (FQDN), `-i` (IP address), `-s` (short hostname)
Missing P2: (none)

### sleep
Supported: (no-op, returns immediately)
Missing P0: (none — cooperative yield is correct for WASM)
Missing P1: (none)
Missing P2: (none)

### date
Supported: `WASMSH_DATE` variable, fixed fallback
Missing P0: `+FORMAT` (format string — extremely common, e.g., `date +%Y-%m-%d`)
Missing P1: `-d` / `--date` (parse date string), `-u` (UTC), `-R` (RFC 2822)
Missing P2: `-I` (ISO 8601), `--iso-8601`

---

## Trivial Utilities

### which
Supported: `-a` (all matches), command names
Missing P0: (none — covers main use)
Missing P1: (none)
Missing P2: (none)

### rmdir
Supported: `-p` / `--parents` (remove parent directories)
Missing P0: (none — `-p` is the key flag)
Missing P1: `--ignore-fail-on-non-empty`
Missing P2: `-v` (verbose)

### tac
Supported: (no flags — reverses lines)
Missing P0: (none)
Missing P1: `-s` (separator)
Missing P2: `-b` (before), `-r` (regex separator)

### nl
Supported: `-b a` / `-ba` (number all lines)
Missing P0: (none for basic use)
Missing P1: `-b t` (number non-empty, default), `-n` (number format: ln/rn/rz), `-s` (separator), `-w` (number width)
Missing P2: `-i` (increment), `-v` (starting number)

### shuf
Supported: `-n N` (count limit)
Missing P0: (none for basic use)
Missing P1: `-e` (echo mode — treat args as input), `-i LO-HI` (range), `-o` (output file)
Missing P2: `-z` (null delimiter), `--random-source`

### cmp
Supported: `-s` / `--silent` / `--quiet`, `-l` (verbose byte-by-byte)
Missing P0: (none — well covered)
Missing P1: `-n N` (compare at most N bytes)
Missing P2: `-i` (skip bytes)

### comm
Supported: `-1`, `-2`, `-3` (suppress columns)
Missing P0: (none — correct coverage)
Missing P1: `--check-order`, `--nocheck-order`
Missing P2: `-z` (null delimiter)

### fold
Supported: `-w N` (width), `-s` (break at spaces)
Missing P0: (none — well covered)
Missing P1: `-b` (count bytes, not columns)
Missing P2: (none)

### nproc
Supported: (returns 1 — correct for WASM)
Missing P0: (none)
Missing P1: `--all` (all processors), `--ignore=N`
Missing P2: (none)

### expand
Supported: `-t N` (tab width)
Missing P0: (none)
Missing P1: `-i` (only initial tabs)
Missing P2: (none)

### unexpand
Supported: `-t N` (tab width), `-a` / `--all`, `--first-only`
Missing P0: (none — well covered)
Missing P1: (none)
Missing P2: (none)

### truncate
Supported: `-s SIZE` (absolute, +/- relative)
Missing P0: (none — covers main use)
Missing P1: `-r` (reference file), size suffixes (K, M, G)
Missing P2: (none)

### factor
Supported: NUMBER... (positional or stdin)
Missing P0: (none — correct behavior)
Missing P1: (none)
Missing P2: (none)

### cksum
Supported: FILE... (files), stdin
Missing P0: (none for basic use)
Missing P1: `--algorithm` (select CRC/SHA etc.)
Missing P2: (none)

### tsort
Supported: FILE or stdin
Missing P0: (none — correct behavior)
Missing P1: (none)
Missing P2: (none)

### install
Supported: `-d` (directory mode), `-m` (mode, skipped)
Missing P0: (none for basic use)
Missing P1: `-o` (owner), `-g` (group), `-v` (verbose), `-D` (create leading directories)
Missing P2: `-b` (backup), `-T` (no target directory)

### timeout
Supported: `--signal`, `-s`, `-k` / `--kill-after`, DURATION, COMMAND
Missing P0: (none — pass-through is acceptable)
Missing P1: `--foreground`, `--preserve-status`
Missing P2: (none)

### cal
Supported: [MONTH YEAR], [YEAR]
Missing P0: (none for basic use)
Missing P1: `-3` (show 3 months), `-y` (whole year), `-m` (Monday as first day)
Missing P2: `-j` (Julian day numbers)

---

## Diff/Patch

### diff
Supported: `-u` / `--unified`, `-c` (context format), `-q` / `--brief`, `-B` / `--ignore-blank-lines`, `-w` / `--ignore-all-space`, `-r` / `--recursive`, `-N` / `--new-file`, `-U N` (context lines)
Missing P0: (none — very good coverage)
Missing P1: `-y` / `--side-by-side`, `--color`, `-i` (ignore case), `--exclude`, `--strip-trailing-cr`
Missing P2: `-b` (ignore space changes), `--no-dereference`, `--tabsize`

### patch
Supported: unified diff parsing and application (stdin or file)
Missing P0: `-p N` (strip N leading path components — very common)
Missing P1: `-R` (reverse), `--dry-run`, `-d DIR` (change directory), `-i FILE` (input file)
Missing P2: `--fuzz`, `-b` (backup), `--verbose`

---

## Tree

### tree
Supported: `-L N` (max depth), `-d` (dirs only), `-a` (show hidden), `-I PATTERN` (exclude), `-f` (full path), `--noreport`, `-J` (JSON output)
Missing P0: (none — excellent coverage)
Missing P1: `-P PATTERN` (include pattern), `-s` (show size), `--du` (disk usage), `-p` (permissions)
Missing P2: `-H` (HTML output), `-C` (colorize), `--dirsfirst`

---

## Search Utilities

### rg
Supported: `-n` (line numbers), `-i` / `--ignore-case`, `-l` / `--files-with-matches`, `-c` / `--count`, `-w` / `--word-regexp`, `-v` / `--invert-match`, `-g` / `--glob`, `-t` / `--type`, `-A N` (after), `-B N` (before), `-C N` (context), `-r` (recursive), `-F` / `--fixed-strings`, `--no-heading`, `--hidden`, `-m` / `--max-count`
Missing P0: (none — excellent coverage)
Missing P1: `-o` / `--only-matching`, `-e PATTERN` (multiple patterns), `--replace`, `--files`, `--json`, `-S` / `--smart-case`, `-p` / `--pretty`
Missing P2: `--stats`, `-U` / `--multiline`, `--pcre2`, `-z` / `--search-zip`

### fd
Supported: `-t` / `--type` (f/d), `-e` / `--extension`, `-H` / `--hidden`, `-I` / `--no-ignore`, `-d` / `--max-depth`, `-x` / `--exec`, `-a` / `--absolute-path`, `-g` / `--glob`, `-1` (stop after first)
Missing P0: (none — excellent coverage)
Missing P1: `-E` / `--exclude`, `-s` / `--case-sensitive`, `-i` / `--ignore-case`, `-0` (null-separated), `--min-depth`, `-S` / `--size`
Missing P2: `--changed-within`, `--changed-before`, `-j` (threads), `--color`

---

## AWK

### awk
Supported: `-F` (field separator), `-v` (variable assignment), `-f` (program file)
Missing P0: (none — the three main flags are present)
Missing P1: `--` (end of options), `-b` (binary)
Missing P2: (none — awk is primarily a mini-language, not a flags-heavy tool)

---

## JSON/YAML Processors

### jq
Supported: `-r` / `--raw-output`, `-j` / `--join-output`, `-e` / `--exit-status`, `-c` / `--compact-output`, `-n` / `--null-input`, `-s` / `--slurp`, `--arg NAME VALUE`, `--argjson NAME VALUE`, `--`, file arguments
Missing P0: (none — excellent coverage)
Missing P1: `-R` / `--raw-input`, `--slurpfile`, `--jsonargs`, `--indent N`, `-f` / `--from-file`
Missing P2: `--tab`, `--sort-keys`, `--ascii-output`

### yq
Supported: `-r` / `--raw-output`, `-e` / `--exit-status`, `-c` / `--compact-output`, `-j` / `--json-output`
Missing P0: (none for basic use)
Missing P1: `-i` (in-place edit), `-y` (YAML output from JSON), `-s` / `--slurp`, `--arg`, `--argjson`
Missing P2: `--indent`, `-P` (pretty print), `--no-doc`

---

## Hash Utilities

### sha1sum
Supported: FILE... (files), stdin
Missing P0: (none for basic use)
Missing P1: `-c` (check against file)
Missing P2: `--tag`, `-b`

### sha512sum
Supported: FILE... (files), stdin
Missing P0: (none for basic use)
Missing P1: `-c` (check against file)
Missing P2: `--tag`, `-b`

---

## Binary Utilities

### xxd
Supported: `-r` (reverse), `-p` (plain hex), `-i` (C include), `-l N` (limit), `-s N` (skip), `-c N` (columns)
Missing P0: (none — excellent coverage)
Missing P1: `-b` (binary/bits mode), `-u` (uppercase hex)
Missing P2: `-g N` (grouping), `-e` (little-endian)

### dd
Supported: `if=`, `of=`, `bs=`, `count=`, `skip=`, `seek=`, `conv=ucase`, `conv=lcase`, `conv=notrunc`
Missing P0: (none for basic use)
Missing P1: `status=none` / `status=progress`, `ibs=` / `obs=` (separate block sizes), `conv=swab`, `iflag=` / `oflag=`
Missing P2: `conv=sync`, `conv=ascii`, `conv=ebcdic`

### strings
Supported: `-n N` / `--bytes N` (minimum string length)
Missing P0: (none for basic use)
Missing P1: `-t` (offset format), `-a` / `--all` (scan whole file)
Missing P2: `-e` (encoding), `-f` (print filename)

### split
Supported: `-l N` (lines), `-b SIZE` (bytes), `-n N` (chunks), `-d` (numeric suffix)
Missing P0: (none — well covered)
Missing P1: `-a N` (suffix length), `--additional-suffix`
Missing P2: `--filter`, `--verbose`

### file
Supported: `-b` / `--brief`, `-i` / `--mime-type`, magic byte detection, extension-based detection
Missing P0: (none for basic use)
Missing P1: `-k` (keep going, show all matches), `-L` (follow symlinks), `-z` (look inside compressed)
Missing P2: `-m` (custom magic file), `-f` (read filenames from file)

---

## Math

### bc
Supported: `-l` (math library — sin, cos, atan, log, exp, scale=20)
Missing P0: (none — `-l` is the primary flag)
Missing P1: `-q` (quiet, no welcome banner — likely already quiet), `-w` (warn)
Missing P2: `-i` (interactive), `-s` (POSIX strict)

---

## Archive/Compression

### tar
Supported: `-c` (create), `-x` (extract), `-t` (list), `-z` (gzip), `-v` (verbose), `-f FILE` (archive), `-C DIR` (change directory)
Missing P0: (none — all critical flags present)
Missing P1: `-j` (bzip2), `-J` (xz), `--exclude`, `--strip-components=N`, `-k` (keep existing)
Missing P2: `--wildcards`, `-p` (preserve permissions), `-m` (no-mtime), `--owner`, `--group`

### gzip
Supported: `-c` (stdout), `-d` (decompress), `-k` (keep original)
Missing P0: (none — well covered)
Missing P1: `-l` (list compression info), `-f` (force), `-r` (recursive), `-1`..`-9` (compression level — accepted for compat)
Missing P2: `-n` (no-name), `-N` (name), `-v` (verbose), `-t` (test)

### gunzip
Supported: (delegates to `gzip -d`, same flags as gzip)
Missing P0: (none)
Missing P1: `-k` (keep)
Missing P2: (none)

### zcat
Supported: (delegates to `gzip -dc`, same flags)
Missing P0: (none)
Missing P1: (none)
Missing P2: (none)

### unzip
Supported: `-l` (list), `-o` (overwrite), `-q` (quiet), `-d DIR` (destination directory)
Missing P0: (none — good coverage)
Missing P1: `-p` (extract to stdout), `-n` (never overwrite), `-j` (junk paths)
Missing P2: `-v` (verbose), `-t` (test)

---

## Disk Usage

### du
Supported: `-s` (summary), `-h` (human-readable), `-a` (all files), `-c` (grand total), `-d N` / `--max-depth=N`
Missing P0: (none — excellent coverage)
Missing P1: `-b` (bytes), `-k` (kilobytes), `--exclude`, `-L` (dereference)
Missing P2: `--apparent-size`, `--time`, `-0` (null delimiter)

### df
Supported: `-h` (human-readable)
Missing P0: (none for VFS)
Missing P1: `-T` (show filesystem type), `-i` (inode info)
Missing P2: `--total`, `-a` (all filesystems)

---

## bat (cat clone)

### bat
Supported: `-n` / `--number`, `-p` / `--plain`, `-A` / `--show-all`, `-r` / `--line-range`, `-l` / `--language` (ignored), `--paging` (ignored), `--style=plain|numbers|header`
Missing P0: (none — good coverage)
Missing P1: `--theme`, `--list-themes`, `--tabs`, `--wrap`, `-f` / `--force-colorization`
Missing P2: `--diff`, `--show-all`, `--map-syntax`

---

## Summary of Critical (P0) Gaps

The following utilities have P0 gaps that agents/developers will commonly encounter:

| Utility | Missing P0 Flag(s) | Impact |
|---------|-------------------|--------|
| **cat** | `-n` (number lines) | Very commonly used |
| **ls** | `-l`, `-a`, `-la`, `-1`, `-R` | All basic ls usage broken |
| **cp** | `-r` / `-R` (recursive) | Cannot copy directories |
| **ln** | `-s` (symbolic link) | Most `ln` usage is `ln -s` |
| **grep** | `-r`, `-l`, `-E`, `-w`, `-o`, `-q` | Major functionality gaps |
| **sed** | `-i` (in-place), `-n`, `-e` | Severely limits sed usage |
| **sort** | `-u` (unique), `-k` (key), `-t` (separator) | Very common flags |
| **find** | `-exec`, `-maxdepth` | Critical for scripting |
| **xargs** | `-I {}`, `-n`, `-0` | Most xargs usage needs `-I` |
| **stat** | `-c` / `--format` | Scripts depend on format strings |
| **date** | `+FORMAT` | Nearly all `date` usage needs this |
| **tr** | `-s` (squeeze), character classes | Very commonly used |
| **cut** | `-c`, field ranges | Common usage patterns |
| **env** | `-i`, `VAR=VALUE cmd` | Common scripting pattern |
| **id** | `-u`, `-g`, `-n` | Scripts depend on these |
| **head** | `-c N` (bytes) | Commonly used |

### Estimated effort
- **P0 fixes**: ~30 flags across ~16 utilities (highest impact)
- **P1 fixes**: ~100+ flags (meaningful but less urgent)
- **P2 fixes**: ~60+ flags (completeness)

### Recommendations
1. **Prioritize ls -la**: This is the single most common command agents use
2. **grep -r/-E**: Recursive grep is expected everywhere
3. **cp -r**: Without this, directory operations fail
4. **sed -i/-n/-e**: These are used in nearly all sed invocations
5. **sort -u/-k/-t**: Sort without key selection is very limited
6. **find -exec**: Without this, find is mostly useful for listing only
7. **cat -n**: Agents frequently use this for code review
8. **date +FORMAT**: Nearly every date usage in scripts uses format strings
