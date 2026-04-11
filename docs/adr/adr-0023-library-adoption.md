# ADR-0023: Library Adoption for Text and Encoding Utilities

## Status

Accepted

## Context

ADR-0022 introduced the first external regex dependency (`posix-regex`)
and showed that a narrowly-scoped, zero-dep crate can replace
hand-written code in `wasmsh-utils` without disturbing the wasm build,
the license story, or the clean-room boundary. With that precedent set,
several other modules in `wasmsh-utils` still contain substantial
Not-Invented-Here code that duplicates functionality available in
well-tested, permissively licensed, no_std-friendly crates:

- `data_ops.rs` carried ~150 lines of hand-rolled **base64** encode and
  decode.
- `helpers.rs` carried a hand-rolled **hex** encoder.
- `awk_ops.rs` carried a ~324-line hand-rolled **regex** engine used
  exclusively by awk's pattern matching, while `sed` and `grep` had
  already moved to `posix-regex` in ADR-0022.

The policy lens has shifted from "wait for a bug, then touch" to
"prefer established libraries over reinvention, subject to the
clean-room and wasm constraints." This ADR documents that change of
posture and applies it to the three cases above.

## Decision

Adopt three additional workspace dependencies:

| Crate         | Version | License        | Purpose                           |
|---------------|---------|----------------|-----------------------------------|
| `base64`      | 0.22    | MIT / Apache-2 | base64 encode / decode (RFC 4648) |
| `hex`         | 0.4     | MIT / Apache-2 | lowercase hex encode              |
| `posix-regex` | 0.1     | MIT            | POSIX BRE/ERE (from ADR-0022)     |

All three are `no_std`-compatible with `alloc`, have zero transitive
runtime dependencies, and compile cleanly for `wasm32-unknown-unknown`
and `wasm32-unknown-emscripten`.

### Code deletions

- **base64**: removed the `b64_encode`, `b64_decode_char`,
  `decode_quartet` functions and the streaming quartet drain loop from
  `data_ops.rs`. The `util_base64` command now uses
  `base64::engine::general_purpose::STANDARD` directly. Error messages
  are translated from the crate's `DecodeError` variants into the
  diagnostic strings the historical implementation emitted, preserving
  agent-visible behaviour.
- **hex**: `helpers::hex_encode` is now a one-line wrapper over
  `hex::encode`. The wrapper is retained so future call sites have a
  single crate-local entry point that is stable across dependency
  upgrades.
- **awk regex**: deleted ~324 lines of hand-rolled regex engine from
  `awk_ops.rs` (types `RePiece`, `ReNode`, `RepeatKind`, `CcRange`,
  `CompiledPattern` and the matcher / compiler functions that went
  with them). The two entry points used by the awk interpreter
  (`regex_match`, `regex_find`) are now thin wrappers over
  `crate::regex_posix::Regex::compile_ere`. Malformed patterns fall
  through to literal substring matching, preserving the behaviour of
  the old engine when confronted with unknown metacharacters.

### awk now uses POSIX ERE

The previous hand-rolled awk engine supported only a subset of POSIX
ERE: `.`, `*`, `+`, `?`, `^`, `$`, `[...]`, `[^...]`, and `\d`/`\w`/
`\s` escapes. It did not support `{n,m}` bounded repetition, grouping
`(...)`, alternation `|`, word boundaries, or POSIX character classes
like `[[:digit:]]`. After the migration, all of these features work in
awk, matching GNU awk's behaviour for the common cases.

The migration is drop-in: all 149 pre-existing awk unit tests continue
to pass without modification, and the full TOML conformance suite
(540 cases) and workspace (1567 tests) remain green.

## Alternatives Considered

### Continue maintaining hand-rolled code

Rejected. The hand-rolled regex in `awk_ops` duplicated most of the
functionality now in `posix-regex`, contained its own bugs (notably the
leftmost-longest gap in ADR-0022), and forced three different engines
(`sed`/`grep` literal, `awk` handwritten, `rg`/`fd` another) to live
alongside each other. Three different regex dialects in one crate is a
maintenance tax with no corresponding benefit.

### Use `regex` / `regex-lite`

Same objections as ADR-0022: wrong dialect (PCRE-ish, not POSIX) and
wrong matching semantics (leftmost-first, not leftmost-longest). Also
larger wasm footprint and more transitive dependencies.

### Use `data-encoding` for base64 and hex in one crate

Possible, but `base64` and `hex` are two single-purpose crates with
zero dependencies each. Bundling them through `data-encoding` adds a
medium-sized crate for no material benefit.

## Consequences

- Three new workspace dependencies (`base64`, `hex`, `posix-regex`).
  All MIT or Apache-2, all already on the `deny.toml` allowlist.
- ~354 lines of hand-written code deleted from `wasmsh-utils`
  (approximately 150 for base64, 10 for hex, 294 for awk regex after
  wrapper).
- awk now supports the full POSIX ERE feature set via `posix-regex`:
  `{n,m}` bounded repetition, groups, alternation, POSIX character
  classes, word boundaries `\<`/`\>`, and backreferences `\1`..`\9`.
  Existing awk scripts are unaffected because the old feature set is a
  subset of the new one.
- `sed`, `grep`, and `awk` now all use the **same** regex engine, so
  the same pattern parses and matches consistently across the three
  utilities. Previously, `sed /\[foo\]/` worked after ADR-0022 but the
  awk equivalent would only work because the awk engine happened to
  accept that form as well.
- License compliance stays clean; `cargo deny check licenses` still
  passes with only the existing MIT / Apache-2 / BSD allowlist.
- wasm32-unknown-unknown and wasm32-unknown-emscripten both build
  cleanly with the new dependencies.
- Binary size impact: negligible. `base64` and `hex` are tiny;
  `posix-regex` was already pulled in by ADR-0022.

## Scope boundaries (what this ADR does NOT touch)

The original text of this ADR deferred hashes and sub-language
parsers out of scope by citing ADR-0015 and ADR-0003 respectively.
Both deferrals were wrong and have been revised:

- **Hashes**: moved in-scope under [ADR-0024](adr-0024-hashes-via-rustcrypto.md),
  which supersedes ADR-0015.  `md5sum`, `sha1sum`, `sha256sum`, and
  `sha512sum` now use the RustCrypto crates (`md-5`, `sha1`, `sha2`).
- **Sub-language parsers** (awk expressions, jq filters, yaml
  documents): *not* blocked by ADR-0003, which covers only the shell
  grammar (see ADR-0003's updated Scope section).  They remain
  hand-written in this migration pass but are explicit future
  candidates under their own ADRs (ADR-0025 for `jaq-core`,
  ADR-0026 for `saphyr` — not yet written).

### Actually out of scope for this ADR

These are deferred on their own merits, not inherited from other
ADRs:

- **Compression** (gzip / deflate / inflate in `archive_ops.rs`,
  used by `gzip`, `gunzip`, `zcat`, `tar -z`, `unzip`).  A follow-up
  ADR will consider `miniz_oxide`, which needs its own verification
  pass (wasm binary size, determinism of deflate output, CRC-32
  attribution).  Held separate to keep this ADR focused on text and
  encoding utilities.
- **jq filter language** (`jq_ops.rs`, ~8000 lines).  Migrating to
  `jaq-core` is larger and higher-risk than this ADR's scope; needs
  wasm32 dependency verification for `dyn-clone`, `once_cell`, and
  `typed-arena`; deserves its own ADR and its own risk budget.
- **YAML parsing** (`yaml_ops.rs`, ~2000 lines).  `saphyr` is a
  plausible target; held separate for the same reasons as jq.
- **CRC-32** (`helpers::crc32`).  It is a checksum, not a
  cryptographic hash, and the compile-time table is shared between
  `cksum` and the gzip output path.  It will likely be absorbed into
  the compression-library migration when that lands.
