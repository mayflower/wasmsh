# ADR-0025: DEFLATE/gzip via miniz_oxide

## Status

Accepted

## Context

Before this ADR, wasmsh's gzip and unzip utilities implemented DEFLATE
themselves in `archive_ops.rs`.  The implementation was technically
"correct" in the sense that every file it produced was a valid RFC
1952 gzip file and could be decompressed by GNU `gunzip` — but the
compression path only emitted **DEFLATE stored blocks** (block type 0),
and the decompression path only accepted them.  In other words:

- `wasmsh gzip` produced files the same size as the input plus 18
  bytes of header/trailer.  There was no actual compression happening.
- `wasmsh gunzip` / `zcat` / `tar -xzf` could only decode files that
  wasmsh itself had produced.  Any real-world `.gz` file (produced by
  upstream `gzip`, `curl | gzip -c`, `tar -czf`, a CI artifact, a
  package download, or anything else using fixed or dynamic Huffman
  encoding) failed with `unsupported DEFLATE block type 1` or `2`.
  This quietly broke the agent workflow `curl -O …tar.gz && tar -xzf`
  for virtually every real URL, despite the utilities advertising full
  gzip support.

Similarly, `wasmsh unzip` explicitly rejected any entry with
compression method 8 (DEFLATE), which is the default and most common
compression method used by real zip archives.  Only stored (method 0)
entries worked, which meant almost no `.zip` file in the wild could be
extracted inside the sandbox.

Reimplementing DEFLATE by hand is in the same category as
reimplementing hashes from ADR-0024: it's tractable but the spec is
long, the edge cases are many, and a well-tested crate (`miniz_oxide`)
already exists with exactly the right profile.

## Decision

Adopt `miniz_oxide` as the DEFLATE engine for all three call sites:

- `gzip_compress` — writes a RFC 1952 gzip header, then calls
  `miniz_oxide::deflate::compress_to_vec(data, 6)` for the raw DEFLATE
  stream, then appends the CRC-32 + ISIZE trailer.
- `gzip_decompress` — parses the RFC 1952 header, passes the DEFLATE
  body (everything before the 8-byte trailer) to
  `miniz_oxide::inflate::decompress_to_vec`, then verifies CRC-32 and
  ISIZE against the decoded bytes.
- `unzip_extract_deflated` — new function that handles ZIP entries
  with compression method 8 (DEFLATE).  Calls
  `miniz_oxide::inflate::decompress_to_vec` on the compressed data
  slice and writes the result to the VFS.

### Why miniz_oxide specifically

`miniz_oxide` is:

- A pure-Rust port of [miniz](https://github.com/richgel999/miniz),
  which is itself the classic single-file-C DEFLATE implementation.
- The default Rust-only backend of the `flate2` crate, which is the
  de facto gzip/deflate wrapper in the Rust ecosystem.
- Licensed MIT / Apache-2 / Zlib (triple-licensed for maximum
  compatibility) — already satisfies `deny.toml`.
- `no_std` with `alloc` — fits `wasm32-unknown-unknown` cleanly.
- Has one runtime dependency (`adler2` for Adler-32 checksumming of
  zlib streams; the gzip path does not use it).
- Uses `#![forbid(unsafe_code)]` in safe mode — no `unsafe` in the
  decompression or compression paths.

### Scope (what stays hand-rolled)

- **RFC 1952 gzip framing**.  The 10-byte header and 8-byte trailer
  are a fixed layout; wrapping them by hand is three lines of code
  and makes the integration with miniz_oxide explicit.  A full
  `flate2` dependency would hide this but pull in more transitive
  deps and make the wasm footprint larger.
- **CRC-32**.  Still computed by the compile-time table in
  `helpers::crc32` because that table is shared with `cksum` (see
  ADR-0024's deferral of CRC-32).  `miniz_oxide` exposes its own
  CRC-32 primitive but using it would force us to either drop
  `helpers::crc32` (and migrate `cksum` separately) or maintain two
  implementations.  One is cleaner.
- **ZIP local-file-header parsing**.  The handwritten ZIP parser in
  `archive_ops.rs` is small and specific; the zip crate is much
  larger and pulls in async/time dependencies.

## Alternatives Considered

### `flate2`

Rejected.  `flate2` is a higher-level crate that wraps a choice of
backends (miniz_oxide, zlib-ng, zlib-rs).  Using it directly would add
a layer of indirection and the additional dependencies that come with
its ReadBuf / ZipArchive / WriteBuf adapters.  Since wasmsh only
needs raw compress-to-vec and decompress-to-vec, depending on
`miniz_oxide` directly is simpler and smaller.

### `zlib-rs`

Rejected.  `zlib-rs` is a zlib-compatible implementation, but the
project targets zlib API compatibility rather than being a
lightweight codec.  Larger footprint and less widely deployed than
miniz_oxide.

### `deflate-rs` / `inflate-rs`

Rejected.  The `deflate` and `inflate` crates were the original
pre-`miniz_oxide` Rust implementations.  They're still maintained but
are smaller-scope and less well-tested in wasm targets.  `miniz_oxide`
is the ecosystem consensus.

### Keep the hand-rolled stored-blocks implementation

Rejected.  It was the reason real-world gzip files could not be read
inside the sandbox — a significant functional defect masquerading as
an implementation simplification.

## Consequences

- One new workspace dependency: `miniz_oxide = "0.8"`, with the
  default features replaced by `with-alloc`.  Transitive: `adler2`.
  Both MIT / Apache-2 / Zlib.
- ~40 lines of hand-rolled DEFLATE stored-block code deleted from
  `archive_ops.rs` (the `read_stored_blocks` function, the
  stored-block emission loop in `gzip_compress`).
- **Real compression**: files produced by `wasmsh gzip` are now
  materially smaller than the input (~6:1 on typical text).
- **Real interoperability**: files produced by upstream `gzip`,
  `curl`, `tar -czf`, GitHub release artifacts, Pyodide wheels, and
  any other real-world source now decompress correctly in the
  sandbox.
- **Real zip support**: `unzip` can now extract DEFLATE-compressed
  entries, which is virtually every real zip archive.  The
  regression test `unzip_deflated_entry` proves this works.
- New regression test `zcat_decompresses_real_deflate_gzip` builds a
  gzip file via `miniz_oxide` directly (the same codec path real
  upstream gzip uses) and verifies wasmsh's zcat can decode it.  This
  test would have failed with the previous implementation and locks
  in the capability going forward.
- wasm32-unknown-unknown and wasm32-unknown-emscripten both continue
  to build cleanly.  The standalone browser E2E suite (15/15) passes
  against a wasm build with `miniz_oxide` linked in.
- 1569 workspace tests pass, up from 1567.  No regressions in the
  TOML conformance suite, the unit tests, or the browser E2E.
- License compliance unchanged; MIT / Apache-2 / Zlib are all on the
  `deny.toml` allowlist.
