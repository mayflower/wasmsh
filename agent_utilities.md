# Agent Utility Prioritization for wasmsh

Research-based prioritization of utilities to make wasmsh a productive bash sandbox for software agents (Claude Code, SWE-agent, OpenHands, Codex CLI, etc.).

## Key Insight

Vercel's experiment (2026): removing 80% of specialized agent tools and giving Claude just a bash shell + filesystem improved accuracy from 80% → 100% while cutting cost 75%. Modern agents are highly proficient with standard Unix tools. **A complete shell with good utility coverage is the optimal agent sandbox.**

## Current State

wasmsh currently supports **72 commands** (31 builtins + 41 utilities). Coverage is strong for basic file ops and text processing. Major gaps are in structured data processing, file comparison, archival, and developer tooling.

## Agent Tool Usage Frequency (from research)

| Rank | Operation | Tools Used | wasmsh Status |
|------|-----------|------------|---------------|
| 1 | Code search | grep, rg | ✅ grep |
| 2 | File reading | cat, head, tail | ✅ all |
| 3 | File discovery | find, fd, ls, tree | ⚠️ no tree |
| 4 | File editing | sed, patch, diff | ⚠️ no diff/patch |
| 5 | Build/test/lint | make, npm, cargo, pytest | ❌ out of scope |
| 6 | Git operations | git (status, diff, log, add, commit) | ❌ not implemented |
| 7 | Text transform | sort, uniq, wc, cut, tr, awk, jq | ⚠️ no awk, jq |
| 8 | File management | mkdir, cp, mv, rm, touch, chmod | ✅ all |
| 9 | Pipelines | xargs, tee | ✅ all |
| 10 | HTTP requests | curl, wget | ❌ needs network |

---

## P0 — Critical for Agent Workflows

These are the highest-impact gaps. Without them, agents hit walls constantly.

### 1. `jq` — JSON processor
- **Why**: Agents work with JSON constantly (package.json, tsconfig.json, API responses, config files, tool output). Every major sandbox base image includes jq. SWE-agent, OpenHands, and Claude Code all rely on JSON parsing in pipelines.
- **Complexity**: High (recursive descent parser, filter language, path expressions)
- **Minimum viable**: `.key`, `.[0]`, `.[]`, `|` pipe, `-r` raw output, `-e` exit status, `select()`, `keys`, `length`, `type`, `map()`, `to_entries`, `@csv`/`@tsv`, string interpolation
- **BusyBox**: Not included (separate project), but universally installed in agent sandboxes

### 2. `diff` — file comparison
- **Why**: Core agent operation. Codex CLI's primary edit mechanism is unified diff. Agents generate and review diffs constantly (`git diff` output format). SWE-agent and OpenHands compare files before/after edits.
- **Complexity**: Medium (longest common subsequence algorithm)
- **Minimum viable**: unified format (`-u`), context format (`-c`), normal format, `-r` recursive, `--color`, `-q` brief, `-B` ignore blank lines, `-w` ignore whitespace
- **BusyBox**: ✅ included

### 3. `patch` — apply diffs
- **Why**: Complementary to diff. Codex CLI uses unified diffs as its file edit mechanism. Agents frequently need to apply patches. Critical for agentic code modification workflows.
- **Complexity**: Medium (parse unified/context format, apply hunks with fuzz)
- **Minimum viable**: unified format (`-u`), `-p` strip prefix, `--dry-run`, `-R` reverse, fuzz factor
- **BusyBox**: ✅ included

### 4. `awk` — text processing
- **Why**: Agents use awk for column extraction, field processing, and data transformation. More powerful than `cut` for structured text. Common in build scripts and CI pipelines that agents need to understand and modify.
- **Complexity**: High (own language: patterns, actions, fields, variables, functions, printf)
- **Minimum viable**: Field splitting (`$1`, `$2`, `NF`, `NR`), pattern matching, `BEGIN`/`END`, `-F` field separator, `print`/`printf`, variables, basic arithmetic, string functions (`length`, `substr`, `index`, `split`, `gsub`, `sub`, `match`, `sprintf`, `tolower`, `toupper`)
- **BusyBox**: ✅ included (mawk-compatible)

### 5. `tree` — directory visualization
- **Why**: Agents use tree to quickly understand project structure. Claude Code, Cursor, and SWE-agent all benefit from tree output for spatial reasoning about codebases. Included in yolobox and most sandbox images.
- **Complexity**: Low (recursive VFS walk with formatting)
- **Minimum viable**: depth limit (`-L`), `-d` directories only, `-I` exclude pattern, `--gitignore`, `-a` show hidden, `-f` full path, JSON output (`-J`)
- **BusyBox**: ✅ included (in miscutils)

### 6. `which` — locate commands
- **Why**: Agents need to check command availability before using them. Common in setup scripts and CI. The `type` builtin partially covers this but `which` has different semantics (PATH-only lookup).
- **Complexity**: Very low (iterate PATH, check command registry)
- **Minimum viable**: basic lookup, `-a` show all matches
- **BusyBox**: ✅ included

### 7. `timeout` — run with time limit
- **Why**: Agents need to prevent runaway commands. Critical for sandbox safety. Agents wrap potentially slow commands (builds, tests) with timeout to maintain responsiveness.
- **Complexity**: Low (wrap command execution with deadline)
- **Minimum viable**: `timeout DURATION COMMAND`, `--signal`, `--kill-after`
- **BusyBox**: ✅ included

### 8. `rmdir` — remove empty directories
- **Why**: Basic POSIX utility. Scripts expect it to exist. Agents use it for cleanup.
- **Complexity**: Very low
- **Minimum viable**: basic, `-p` parents
- **BusyBox**: ✅ included

---

## P1 — High Value for Agent Workflows

These significantly expand what agents can do in the sandbox.

### 9. `tar` + `gzip`/`gunzip`/`zcat` — archive handling
- **Why**: Agents need to extract downloaded packages, create backups, bundle outputs. Common in CI scripts. Tar+gzip is the universal exchange format.
- **Complexity**: Medium-high (tar header parsing, DEFLATE for gzip)
- **Minimum viable**: `tar -czf` create, `tar -xzf` extract, `tar -tzf` list, `gzip`/`gunzip` standalone
- **BusyBox**: ✅ included
- **Note**: Could use a Rust DEFLATE implementation (flate2 or miniz_oxide, both MIT)

### 10. `sha1sum` / `sha512sum` — additional checksums
- **Why**: Agents verify file integrity. sha1sum still common in legacy systems, sha512sum for security-sensitive contexts. Package managers use these.
- **Complexity**: Low (clean-room hash implementations like existing md5sum/sha256sum)
- **BusyBox**: ✅ included

### 11. `tac` — reverse file
- **Why**: View logs in reverse chronological order. Agents reading log files often want newest entries first.
- **Complexity**: Very low
- **BusyBox**: ✅ included

### 12. `nl` — number lines
- **Why**: Agents need line numbers for code references. `cat -n` partially covers this but `nl` has more formatting options.
- **Complexity**: Very low
- **BusyBox**: ✅ included

### 13. `od` / `hexdump` / `xxd` — binary inspection
- **Why**: Agents debugging binary formats, inspecting file encodings, examining wasm binaries. `xxd` is particularly popular for hex dumps.
- **Complexity**: Low (format bytes as hex/octal/decimal with various layouts)
- **Minimum viable**: `xxd` (hex dump + reverse), `od -A x -t x1z` (classic), `hexdump -C`
- **BusyBox**: ✅ included

### 14. `shuf` — random shuffle/selection
- **Why**: Agents sampling data, randomizing test order, selecting random entries. Useful for data processing pipelines.
- **Complexity**: Very low (Fisher-Yates shuffle)
- **BusyBox**: ✅ included

### 15. `bc` — calculator
- **Why**: Arbitrary precision arithmetic beyond what `$((...))` provides. Common in shell scripts for floating-point math. Agents performing calculations in scripts.
- **Complexity**: Medium (expression parser, arbitrary precision, math functions)
- **Minimum viable**: basic arithmetic (+, -, *, /, %, ^), scale, `sqrt()`, comparison operators, variables, `ibase`/`obase`
- **BusyBox**: ✅ included

### 16. `dd` — data copy/convert
- **Why**: Creating files of specific sizes, data format conversion, block-level operations. Used in test setup and data generation.
- **Complexity**: Medium (block-based I/O with conversion)
- **Minimum viable**: `if=`, `of=`, `bs=`, `count=`, `skip=`, `seek=`, `conv=` (basic: ucase, lcase, notrunc), `/dev/zero` and `/dev/urandom` support
- **BusyBox**: ✅ included

### 17. `cmp` — byte comparison
- **Why**: Quick binary comparison of files. More efficient than diff for checking equality. Used in build systems and test scripts.
- **Complexity**: Very low
- **BusyBox**: ✅ included

### 18. `comm` — compare sorted files
- **Why**: Set operations on sorted line-based data (intersection, difference, unique). Useful for comparing file lists, finding common entries.
- **Complexity**: Very low
- **BusyBox**: ✅ included

### 19. `fold` — wrap lines
- **Why**: Text formatting for fixed-width displays. Agents formatting output for readability.
- **Complexity**: Very low
- **BusyBox**: ✅ included

### 20. `df` / `du` — disk usage
- **Why**: Agents checking available space, measuring directory sizes. Useful for VFS quota awareness.
- **Complexity**: Low (walk VFS, sum sizes)
- **Minimum viable**: `du -sh`, `du -h`, `df -h` (report VFS usage/limits)
- **BusyBox**: ✅ included

---

## P2 — Nice to Have

These round out the utility set for broader compatibility.

### 21. `expand` / `unexpand` — tab↔space conversion
- **Complexity**: Very low
- **BusyBox**: ✅

### 22. `split` — split files into pieces
- **Complexity**: Low
- **BusyBox**: ✅

### 23. `truncate` — set file size
- **Complexity**: Very low
- **BusyBox**: ✅

### 24. `factor` — prime factorization
- **Complexity**: Low (trial division)
- **BusyBox**: ✅

### 25. `nproc` — processor count
- **Complexity**: Trivial (always returns 1 in WASM)
- **BusyBox**: ✅

### 26. `strings` — extract printable strings from binary
- **Complexity**: Very low
- **BusyBox**: ✅ (in miscutils)

### 27. `install` — copy with permissions
- **Complexity**: Low (cp + chmod + mkdir)
- **BusyBox**: ✅

### 28. `cksum` / `sum` — CRC checksums
- **Complexity**: Low
- **BusyBox**: ✅

### 29. `tsort` — topological sort
- **Complexity**: Low (DAG sort)
- **BusyBox**: ✅

### 30. `mkfifo` — create named pipe
- **Complexity**: Low (VFS node type)
- **BusyBox**: ✅

### 31. `uniq -c` enhancements (already have uniq)
- Count prefix, skip fields/chars

### 32. `logger` — write log messages
- **Complexity**: Very low (write to /var/log or event stream)

### 33. `cal` — calendar
- **Complexity**: Low

---

## P3 — Future / Complex Features

These are high-value but very complex to implement correctly.

### 34. `git` (subset) — version control
- **Why**: THE most important tool for agents. Every single agent framework uses git extensively. Claude Code averages dozens of git calls per session.
- **Complexity**: Extremely high (object store, pack files, index, refs, merge, diff, etc.)
- **Approach**: Either implement a minimal subset (init, add, commit, diff, log, status, show) operating on VFS, or provide a `git` shim that translates to protocol messages for the host to handle.
- **Minimum viable subset**: `git init`, `git add`, `git status`, `git diff`, `git commit`, `git log`, `git show`, `git branch`, `git checkout`
- **Alternative**: Host-delegated git via protocol extension (most practical)

### 35. `curl` / `wget` (subset) — HTTP client
- **Why**: Agents test APIs, download files, check endpoints.
- **Complexity**: Depends on network capability exposure
- **Approach**: Protocol message to host for actual HTTP, or sandboxed mock responses
- **Minimum viable**: `curl -s URL`, `curl -X POST -d DATA URL`, `-H` headers, `-o` output file

### 36. `yq` — YAML processor
- **Why**: Agents work with Kubernetes manifests, GitHub Actions workflows, docker-compose files.
- **Complexity**: Medium (YAML parser + jq-like filter language)

### 37. `make` (subset) — build orchestration
- **Why**: Agents use `make` as a standardized entrypoint for build/test/lint.
- **Approach**: Parse Makefile, resolve dependencies, execute recipes as shell commands
- **Minimum viable**: simple targets, variables, dependencies, `.PHONY`, pattern rules

### 38. Virtual devices: `/dev/null`, `/dev/zero`, `/dev/urandom`, `/dev/stdin`, `/dev/stdout`, `/dev/stderr`
- **Why**: Standard Unix patterns like `> /dev/null`, `dd if=/dev/zero`, reading from `/dev/urandom`
- **Complexity**: Low (VFS special nodes)

---

## Not Applicable in Browser Sandbox

These BusyBox utilities have no meaning in a WASM sandbox:
- Process management: `ps`, `kill`, `top`, `free`, `pgrep`, `pidof`
- Networking daemons: `httpd`, `ftpd`, `telnetd`, `sshd`
- User management: `adduser`, `passwd`, `su`, `login`
- Hardware: `mount`, `fdisk`, `mkfs`, `dmesg`, `hdparm`
- Init/services: `init`, `rc`, `runsv`, `syslogd`
- Terminals: `getty`, `stty`, `openvt`, `clear` (no TTY)

---

## Implementation Roadmap

### Phase 1: Agent Essentials (P0)
~8 utilities, estimated effort: medium-high

| Utility | Complexity | Lines est. | Key challenge |
|---------|-----------|------------|---------------|
| `which` | trivial | 30 | — |
| `rmdir` | trivial | 40 | parent flag |
| `timeout` | low | 80 | integrate with step budget |
| `tree` | low | 150 | formatting, gitignore |
| `diff` | medium | 600 | LCS algorithm, unified format |
| `patch` | medium | 500 | hunk parsing, fuzz matching |
| `awk` | high | 1500+ | own parser, field splitting, functions |
| `jq` | high | 2000+ | JSON parser, filter language, pipes |

### Phase 2: Expanded Coverage (P1)
~12 utilities, estimated effort: medium

| Utility | Complexity | Lines est. |
|---------|-----------|------------|
| `tac` | trivial | 30 |
| `nl` | trivial | 50 |
| `shuf` | trivial | 60 |
| `cmp` | trivial | 50 |
| `comm` | low | 60 |
| `fold` | low | 50 |
| `sha1sum` | low | 200 |
| `sha512sum` | low | 200 |
| `xxd` | low | 150 |
| `dd` | medium | 300 |
| `bc` | medium | 500 |
| `du`/`df` | low | 200 |
| `tar`+`gzip` | high | 1500+ |

### Phase 3: Developer Experience (P2+P3)
Virtual devices, remaining P2 utilities, and complex features like git shim, curl proxy, etc.

---

## Sources

- BusyBox 1.37.x applet list (busybox.net, GitHub mirror)
- Vercel "We removed 80% of our agent's tools" (2026)
- SWE-agent NeurIPS 2024 paper (ACI design)
- OpenHands ICLR 2025 paper (CodeActAgent)
- Anthropic 2026 Agentic Coding Trends Report
- Claude Code tools documentation (Bash, Read, Grep, Glob, Edit)
- Docker AI Sandbox base images
- Yolobox sandbox toolkit
- Mini-SWE-Agent (bash-only approach)
- Claude Code GitHub issues (#19649, #21696, #21697 — agent tool usage patterns)
