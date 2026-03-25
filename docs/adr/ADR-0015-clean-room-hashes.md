# ADR-0015: Clean-Room Hash and Checksum Implementations

## Status
Accepted

## Context
Standard shell utilities (md5sum, sha1sum, sha256sum, sha512sum, cksum, gzip) require cryptographic hash and checksum algorithms. In a browser WASM sandbox, we cannot link to system libcrypto or OpenSSL. External Rust crate dependencies (sha2, md5) would add to binary size and are not strictly necessary for correctness.

## Decision
All hash algorithms are implemented from scratch following their respective specifications:

| Algorithm | Spec | Output | Implementation |
|-----------|------|--------|----------------|
| MD5 | RFC 1321 | 128-bit | `data_ops.rs` |
| SHA-1 | RFC 3174 | 160-bit | `hash_ops.rs` |
| SHA-256 | FIPS 180-4 | 256-bit | `data_ops.rs` |
| SHA-512 | FIPS 180-4 | 512-bit | `hash_ops.rs` |
| CRC-32 | ISO 3309 | 32-bit | `helpers.rs` (shared) |

CRC-32 uses a compile-time lookup table (`const fn build_crc32_table`) shared between cksum and gzip/gunzip.

## Consequences
- **Zero external crypto dependencies** for the utils crate
- **Verified against known test vectors**: NIST reference values for empty string and "abc"
- **Clean-room provenance**: Written from RFC/FIPS specifications, not derived from existing implementations
- **Not constant-time**: These implementations are not side-channel resistant (acceptable for a sandbox utility, not for security-critical use)
- **Binary size benefit**: No additional WASM payload from crypto crate dependencies
