# ADR-0015: Clean-Room Hash Implementations

## Status
**Superseded by [ADR-0024](adr-0024-hashes-via-rustcrypto.md).**

The reasoning in this ADR does not survive scrutiny — see ADR-0024's
Context section for the point-by-point rebuttal.  Hashes are now
implemented via the RustCrypto crates (`md-5`, `sha1`, `sha2`).  The
text below is preserved for historical record only.

## Context
Utilities like md5sum, sha1sum, sha256sum, sha512sum, and cksum/gzip require cryptographic hash algorithms. There is no libcrypto in browser WASM. External crates (sha2, md5) would increase the binary size.

## Decision
All hash algorithms are implemented by hand according to RFC/FIPS specifications -- no external crypto crate:

- MD5 (RFC 1321), SHA-1 (RFC 3174), SHA-256/SHA-512 (FIPS 180-4), CRC-32 (ISO 3309)
- CRC-32 table is generated at compile time (`const fn`) and shared by cksum and gzip
- Verified against NIST reference vectors (empty string, "abc")

## Consequences
- Zero external crypto dependencies for wasmsh-utils
- Clean-room provenance: written from specification, not derived
- Not constant-time (acceptable for sandbox, not for security-critical applications)
- Less WASM payload due to absent crate dependencies
