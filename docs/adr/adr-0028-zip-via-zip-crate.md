# ADR-0028: unzip via the `zip` crate

## Status

Accepted

## Context

Before this ADR, wasmsh's `unzip` utility was a handwritten reader
for the ZIP local-file-header format, ~300 lines in `archive_ops.rs`:

- `find_pk_signature` / `unzip_advance_to_header` — linearly scanned
  the raw archive bytes for `PK\x03\x04` signatures.
- `parse_zip_local_header` — read the 30-byte local header (compression
  method, compressed/uncompressed size, filename length, name) by
  byte offset.
- `unzip_extract_stored` / `unzip_extract_deflated` — copied stored
  bytes or delegated to `miniz_oxide::inflate::decompress_to_vec`.
- `sanitize_zip_name` / `unzip_check_traversal` — zip-slip defence.

This worked for the archives wasmsh had historically been tested
with (pip wheels, simple test fixtures, well-formed zips produced
by `zip -r`), but it had real functional gaps that showed up as
soon as you fed it anything unusual:

- **No central directory handling.**  Real ZIP archives have an
  "end of central directory record" at the very end that lists
  every entry with its location in the file.  wasmsh's reader
  ignored it and walked forward from the start of the file.  This
  breaks on:
  - **Self-extracting archives** where an executable stub precedes
    the first local-file header.  The scanner would get fooled by
    arbitrary `PK\x03\x04` byte sequences inside the stub.
  - **Archives with interleaved data descriptors** (`PK\x07\x08`)
    where local-file-header sizes are zero and the real sizes are
    written *after* the compressed data.  wasmsh assumed sizes in
    the local header were authoritative.
  - **ZIP64** archives (> 4 GB or > 65k entries).  The local-file
    header uses 32-bit fields; ZIP64 moves the real sizes to an
    extra field.
- **No Unicode filename flag handling** (flag bit 11).  wasmsh
  assumed all names were UTF-8, which mostly worked but silently
  mangled CP437-encoded names from older zips.
- **No CRC verification.**  Data integrity was never checked.
- **Limited compression support.**  Only method 0 (stored) and
  method 8 (deflate) were handled.  BZIP2, LZMA, deflate64, and
  zstd were all rejected with "unsupported compression method N".

All of these gaps could be fixed with more handwritten code, but
writing a ZIP reader from scratch is the canonical "just use the
library" case: the format has a quirky history, the corner cases
are numerous, and an active, widely-used, permissively-licensed
crate already exists.

## Decision

Replace the handwritten zip reader with the `zip` crate (version 2,
MIT).  The wasmsh CLI surface (`-l`, `-o`, `-q`, `-d`) stays
identical, along with the list header/footer, zip-slip
sanitisation, and VFS integration — those are wasmsh-specific.
The underlying parsing, decompression, central directory handling,
CRC verification, and ZIP64 support are delegated to the crate.

### Crate configuration

```toml
zip = { version = "2", default-features = false, features = ["deflate"] }
```

- `default-features = false` drops the optional backends we don't
  need: `aes-crypto`, `bzip2`, `deflate64`, `lzma`, `xz`, `zstd`,
  `time`, `chrono`.  These would add significant transitive
  dependencies (`aes`, `bzip2`, `lzma-rs`, `xz2`, `zstd`,
  `chrono`) that we don't want in the wasm build.
- `features = ["deflate"]` enables DEFLATE read/write via
  `flate2`'s pure-Rust backend (which in turn uses `miniz_oxide`
  — the same crate ADR-0025 already added for gzip).  A side-effect
  is that the `zopfli` write-time optimiser gets pulled in; we
  don't call it but it costs ~30 KB of wasm.  An alternative
  would be to disable `deflate-zopfli` specifically, but zip 2's
  feature toggles don't expose the cleaner split we'd need.

### New transitive dependencies

- `zip` itself
- `flate2` (already implicitly present via miniz_oxide; now an
  explicit dep)
- `zopfli` (write-time DEFLATE; unused at runtime but linked in)
- `aho-corasick`, `memchr`, `indexmap`, `hashbrown`, `equivalent`,
  `simd-adler32`, `arbitrary`, `derive_arbitrary`

All MIT / Apache-2.  `cargo deny check licenses` still passes.

### Architecture

```
                      archive_data: Vec<u8>
                            │
                            ▼
               zip::ZipArchive::new(Cursor)
                            │
                            ▼
        for i in 0..archive.len():
            archive.by_index(i) ──► zip::read::ZipFile<'_>
                                      │      │
                                      │      ▼
                                      │  .name(), .size(), .is_dir()
                                      ▼
                             entry.read_to_end(&mut buf)
                                 (DEFLATE via flate2 +
                                  miniz_oxide, CRC32 verified,
                                  central directory authoritative)
                                      │
                                      ▼
                            sanitize_zip_name
                                      │
                                      ▼
                            traversal check
                                      │
                                      ▼
                            ctx.fs.open + write
```

### Wasmsh keeps ownership of

- **Flag parser** (`parse_unzip_args`) — wasmsh's subset of
  unzip's CLI is small and doesn't need a dedicated option crate.
- **`sanitize_zip_name` / `unzip_check_traversal`** — zip-slip
  prevention on top of the raw entry names.  The `zip` crate has
  its own `enclosed_name` helper but it uses `std::path::PathBuf`,
  which fights wasmsh's VFS string-based paths.  Our version is
  simpler and integrates cleanly.
- **List header/footer output** — wasmsh's formatting matches the
  hand-rolled baseline so the TOML conformance tests pass
  unchanged.
- **VFS integration** — `ctx.fs.stat` / `ctx.fs.create_dir` /
  `write_file` — all wasmsh-specific.

## Alternatives Considered

### Keep the handwritten reader

Rejected.  The central-directory gap is a real functional defect,
not just a stylistic one.  Agents that feed `curl`-downloaded
`.zip` files into the sandbox silently hit the failure modes
described in the Context section.  Fixing them one by one would
end up reimplementing half the `zip` crate anyway.

### Use `rc-zip` / `rc-zip-sync`

Considered.  `rc-zip` is a more modern pure-Rust zip reader with
async support.  Its sync flavour `rc-zip-sync` is smaller than
`zip` and has a cleaner API.  But it's less widely used, less
well-tested, and its wasm32 story is less proven.  `zip` is the
de facto standard.

### Use `async_zip`

Rejected.  wasmsh utilities are synchronous by design; bringing
in an async zip reader would require wrapping everything in an
executor just to call it.

### Keep the hand-roll but extend it to parse the central directory

Considered.  It's a finite amount of code.  But the benefit is
exactly "behave like the `zip` crate", and at that point we're
rewriting the crate in-house for no reason.

## Consequences

- **~150 lines of hand-rolled code deleted** from `archive_ops.rs`:
  the entire zip local-header parser, the signature scanner, the
  separate stored/deflated extractors, and the `u16_le`/`u32_le`
  helpers.
- **Full ZIP spec support**:
  - Central directory parsing → correct handling of
    self-extracting archives with prefixed stubs.
  - Data descriptors → correct reading of zips written by tools
    that stream entries without knowing sizes in advance.
  - ZIP64 → archives larger than 4 GB or with more than 65k
    entries work.
  - CRC-32 verification → corrupted entries are now detected.
  - Unicode filename flag (bit 11) → proper UTF-8 handling for
    modern zips.
- **Writer support for free.**  The crate's `ZipWriter` is now
  available to the test suite.  The two test fixtures
  (`make_stored_zip`, `make_deflated_zip`) are now thin wrappers
  over `ZipWriter` instead of handwritten byte layouts — which
  is appropriate because the old fixtures were emitting *invalid
  zips* that happened to work with the old scanner.
- **Two new regression tests** lock in the new capabilities:
  - `unzip_with_prefixed_garbage_uses_central_directory` — builds
    a fake self-extracting archive (stub + fake PK signature +
    real zip) and verifies the central-directory reader finds
    the real payload.
  - `unzip_multiple_entries_in_one_archive` — builds a zip with
    three entries in nested directories and verifies all three
    extract correctly.
- **New workspace dep**: `zip = "2"` (with `default-features =
  false, features = ["deflate"]`).  MIT-licensed, already on the
  allowlist.  The transitive additions (`flate2`, `zopfli`,
  `aho-corasick`, etc.) are all permissive.
- **Wasm build unchanged**.  `cargo check --target
  wasm32-unknown-unknown -p wasmsh-utils` and `-p wasmsh-browser`
  both continue to pass.  Binary size impact: ~30–50 KB from
  zopfli + flate2 glue, which is small relative to the ~600 KB
  wasm baseline.
- **Tests**: 13 unzip tests pass (was 11 before this ADR — 2
  new regressions added).  All 1234 workspace tests and 550 TOML
  conformance cases green.

## Scope boundaries

- **`tar`** remains hand-rolled.  See ADR-0028-scope (this section)
  for why: the handwritten tar reader is ~390 lines, the format
  is frozen, the library alternative (`tar` crate) uses
  `std::path::PathBuf` extensively and doesn't fit wasmsh's VFS
  model cleanly.  The cost/benefit is different from zip.
- **CRC-32** remains hand-rolled.  See the discussion in ADR-0025
  — it's a 35-line const-table implementation shared with `cksum`
  and the gzip trailer, smaller than the library adapter would be.
- **Writer side of `util_unzip`** is a no-op: wasmsh only has
  `unzip` (read), not `zip` (write).  The `ZipWriter` we pulled
  in for test fixtures is not exposed as a utility.  If a future
  feature needs a `zip` utility, adding it is now trivial.
- **Optional compression methods** (BZIP2, LZMA, zstd, xz) are
  still unsupported by wasmsh, because the corresponding `zip`
  crate feature toggles pull in large C-tainted backends.  If an
  agent actually needs one, adding the feature + backend is a
  one-line Cargo.toml change.
