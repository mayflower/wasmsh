# ADR-0015: Clean-Room-Hash-Implementierungen

## Status
Angenommen

## Kontext
Utilities wie md5sum, sha1sum, sha256sum, sha512sum und cksum/gzip benötigen kryptografische Hash-Algorithmen. Im Browser-WASM gibt es kein libcrypto. Externe Crates (sha2, md5) würden die Binary vergrößern.

## Entscheidung
Alle Hash-Algorithmen werden von Hand nach RFC/FIPS-Spezifikation implementiert — kein externes Crypto-Crate:

- MD5 (RFC 1321), SHA-1 (RFC 3174), SHA-256/SHA-512 (FIPS 180-4), CRC-32 (ISO 3309)
- CRC-32-Tabelle wird zur Compile-Zeit generiert (`const fn`) und von cksum und gzip geteilt
- Verifiziert gegen NIST-Referenzvektoren (Leerstring, „abc")

## Konsequenzen
- Null externe Crypto-Dependencies für wasmsh-utils
- Clean-Room-Provenance: geschrieben nach Spezifikation, nicht abgeleitet
- Nicht konstant-zeitig (für Sandbox akzeptabel, nicht für sicherheitskritische Anwendung)
- Weniger WASM-Payload durch fehlende Crate-Dependencies
