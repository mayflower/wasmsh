# ADR-0024: Hashes via RustCrypto

## Status

Accepted.  **Supersedes ADR-0015.**

## Context

ADR-0015 mandated that all hash algorithms (MD5, SHA-1, SHA-256,
SHA-512, CRC-32) be implemented by hand from their specifications,
with no external crypto crate dependencies.  Its stated rationale:

1. "There is no libcrypto in browser WASM."
2. "External crates (`sha2`, `md5`) would increase the binary size."
3. "Clean-room provenance: written from specification, not derived."
4. "Not constant-time (acceptable for sandbox, not for security-critical
   applications)."

Revisiting each of these claims against current facts:

1. **Wrong.**  The RustCrypto crates (`md-5`, `sha1`, `sha2`) all
   compile cleanly for `wasm32-unknown-unknown` and
   `wasm32-unknown-emscripten`.  There is no libcrypto dependency —
   they are pure Rust.  This has been the case since 2018.

2. **Weak.**  The three crates together pull in `digest`,
   `block-buffer`, `crypto-common`, `generic-array`, `typenum`,
   `cpufeatures`, and `libc`/`version_check` as transitive deps.
   All are well under 100 KiB compiled for wasm, and wasmsh's wasm
   binary already ships at ~600 KiB.  The binary-size argument was
   never quantified in ADR-0015; in practice the overhead is
   dominated by the `digest` trait machinery, which the compiler
   strips via dead-code elimination if only a subset of the algorithms
   is used.

3. **Confused.**  ADR-0001 defines "clean-room" as a *license
   compliance* requirement: no code transfer from GPL-licensed
   sources, and tests written independently rather than copied from
   GPL projects.  RustCrypto crates are MIT or Apache-2 licensed, so
   they are automatically clean-room-compatible under ADR-0001.
   ADR-0015 reinterpreted "clean-room" as "written by us from scratch,
   not derived from any upstream source" — an implementation-origin
   argument, not a license argument.  That reading is not what the
   policy says and not what the policy needs.

4. **Backwards.**  The hand-rolled implementations in `hash_ops.rs`
   and `data_ops.rs` have no constant-time protections: they use
   `match` on loop indices, conditional loads, and non-constant-time
   integer operations.  RustCrypto's SHA-1/SHA-256/SHA-512 use
   constant-time round functions and (where the target supports
   them) hardware-accelerated SHA extensions.  For a sandbox
   reading secrets from agent inputs, constant-time is strictly
   better than not-constant-time.

The deeper problem is that cryptographic primitives are the worst
category to reinvent.  A subtle bug in a padding step, a bit-rotate
direction, or a big-endian conversion produces digests that match
NIST test vectors on the common inputs ("abc", empty string) while
silently corrupting on adversarial or rare inputs.  "We tested
against NIST vectors" is a floor for spec compliance, not a ceiling.

## Decision

Replace the hand-rolled MD5, SHA-1, SHA-256, and SHA-512
implementations in `wasmsh-utils` with the corresponding RustCrypto
crates:

| Utility      | Crate     | Version | License        |
|--------------|-----------|---------|----------------|
| `md5sum`     | `md-5`    | 0.10    | MIT / Apache-2 |
| `sha1sum`    | `sha1`    | 0.10    | MIT / Apache-2 |
| `sha256sum`  | `sha2`    | 0.10    | MIT / Apache-2 |
| `sha512sum`  | `sha2`    | 0.10    | MIT / Apache-2 |

CRC-32 (used by `cksum` and by gzip) remains hand-written for now —
it is a checksum, not a cryptographic hash, and its scope is smaller.
A later ADR will revisit CRC-32 if a gzip-backend migration (e.g.
`miniz_oxide`) makes the question moot.

### Clean-room policy unchanged

ADR-0001's clean-room boundary is unchanged.  "Clean-room" means:
- No code transfer from GPL-licensed projects.
- Tests written from specifications, not copied from upstream
  projects with incompatible licenses.

It does **not** mean "written by us from scratch for every
functionality".  Depending on permissively-licensed crates
(MIT, Apache-2, BSD, ISC) is and remains fine.

### What ADR-0015 got right (preserved here)

- **Deterministic output.**  The RustCrypto crates produce byte-for-byte
  identical digests to the hand-rolled versions on the NIST vectors
  (empty input, "abc", common file contents).  All pre-existing hash
  tests pass without modification, including the embedded NIST test
  vectors in `hash_ops::tests`.

- **`no_std` compatible.**  `md-5`, `sha1`, and `sha2` are all
  `no_std`-friendly with `alloc`, so the wasm build story is
  unchanged.

## Alternatives Considered

### Keep hand-rolled hashes

Rejected.  The arguments in ADR-0015 do not survive scrutiny (see
Context above).  Hand-rolled cryptographic primitives are a
security liability and a maintenance burden, and the cited
rationale (no libcrypto, binary size, clean-room) either doesn't
apply or is backwards.

### Use `ring` or another C-backed crypto library

Rejected.  `ring` depends on ring assembly / C code that doesn't
cleanly target `wasm32-unknown-unknown` or
`wasm32-unknown-emscripten`.  Pure Rust (RustCrypto) is a better
fit for wasmsh's dual-target requirement.

### Write a single wrapper module and let it dispatch

Possible, but unnecessary.  Each of the four call sites is already a
single-function digest: `md5_digest`, `sha1_digest`, `sha256_digest`,
`sha512_digest`.  After the migration each is a three-line wrapper
around `Digest::new`, `update`, `finalize`.  A shared dispatcher
would add indirection without saving code.

## Consequences

- Four new workspace dependencies: `md-5`, `sha1`, `sha2`, `digest`.
  All MIT / Apache-2.  `cargo deny check licenses` continues to pass.
- ~510 lines of hand-written, security-sensitive code deleted from
  `wasmsh-utils` (tables of MD5/SHA-256/SHA-512 round constants, the
  four digest compression functions, the padding loops).
- Hash primitives are now audited, fuzzed, constant-time, and
  SIMD-accelerated where the target supports it.
- `md5sum`, `sha1sum`, `sha256sum`, `sha512sum` remain
  byte-for-byte identical in output; all existing unit tests and
  TOML conformance tests pass without modification (1567 workspace
  tests green, 15/15 standalone browser E2E green).
- wasm32-unknown-unknown and wasm32-unknown-emscripten both continue
  to build cleanly.  Binary size delta is small relative to the
  existing baseline (the RustCrypto stack is well-tree-shaken).
- ADR-0015 is marked **Superseded**.  Its text is left in place for
  historical record but should not be relied on by new work.

## What remains hand-written (with explicit rationale)

- **CRC-32** (`helpers::crc32`).  It's a checksum, not a cryptographic
  hash; the compile-time table is shared with the gzip path; and a
  future compression-library ADR may absorb it.  No audit concern.
- **Shell parser/lexer/AST/HIR/IR/VM**.  ADR-0003 applies — see the
  scope clarification added to that ADR in this same migration pass.
- **Sub-language expression evaluators** for awk, jq, yaml.  These
  remain hand-written for now but are *not* out of scope; future
  ADRs (likely ADR-0025 for `jaq-core`, ADR-0026 for `saphyr`) will
  consider migrating them on their own merits.
