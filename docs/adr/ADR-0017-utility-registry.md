# ADR-0017: Utility Registry and Command Organization

## Status
Accepted

## Context
wasmsh bundles 86 utilities that must be resolvable by name at runtime. The command resolution order (builtin → shell function → utility → virtual external) is defined in ADR-0007. This ADR documents how utilities are organized, registered, and invoked.

## Decision

### Registry
All utilities are registered in a single `IndexMap<&'static str, UtilFn>` at startup via `UtilRegistry::new()`. Lookup is O(1). The function signature for all utilities is:

```rust
type UtilFn = fn(&mut UtilContext<'_>, &[&str]) -> i32;
```

### Context
Each utility receives a `UtilContext` providing:
- `fs: &mut MemoryFs` — virtual filesystem access
- `output: &mut dyn UtilOutput` — stdout/stderr streams
- `cwd: &str` — current working directory
- `stdin: Option<&[u8]>` — pipe/here-doc input
- `state: Option<&ShellState>` — environment variables

### Module organization (16 modules, 86 utilities)

| Module | Utilities | Focus |
|--------|-----------|-------|
| `file_ops` | cat, ls, mkdir, rm, touch, mv, cp, ln, readlink, realpath, stat, find, chmod, mktemp | File operations |
| `text_ops` | head, tail, wc, grep, sed, sort, uniq, cut, tr, tee, paste, rev, column, bat | Text processing |
| `data_ops` | seq, basename, dirname, expr, xargs, yes, md5sum, sha256sum, base64 | Data/string |
| `system_ops` | env, printenv, id, whoami, uname, hostname, sleep, date | System info |
| `trivial_ops` | which, rmdir, tac, nl, shuf, cmp, comm, fold, nproc, expand, unexpand, truncate, factor, cksum, tsort, install, timeout, cal | Simple utilities |
| `diff_ops` | diff, patch | File comparison |
| `tree_ops` | tree | Directory visualization |
| `search_ops` | rg, fd | Code search |
| `awk_ops` | awk | Text processing language |
| `jq_ops` | jq | JSON processor |
| `yaml_ops` | yq | YAML processor |
| `hash_ops` | sha1sum, sha512sum | Hash utilities |
| `binary_ops` | xxd, dd, strings, split, file | Binary operations |
| `math_ops` | bc | Calculator |
| `archive_ops` | tar, gzip, gunzip, zcat, unzip | Archive/compression |
| `disk_ops` | du, df | Disk usage |

### Error handling pattern
All utilities follow a consistent error handling pattern:
- `emit_error(output, cmd_name, path, &error)` for filesystem errors
- Exit code 0 for success, 1 for errors, 2 for usage errors (grep, diff)
- File handles always closed on both success and error paths
- No `unwrap()` on user input; no `unwrap_or_default()` on `read_file`

## Consequences
- Adding a new utility requires: implement function, add to registry, add feature flag
- All utilities are stateless (no persistent state between invocations)
- Utilities cannot spawn subprocesses (VFS-only execution model)
- The registry is loaded eagerly at startup (no lazy initialization)
