# ADR-0002: Browser-first auf wasm32-unknown-unknown

## Status
Angenommen

## Kontext
wasmsh muss im Browser laufen. `wasm32-unknown-unknown` ist das minimale Rust-WebAssembly-Ziel ohne standardisierte Host-Imports.

## Entscheidung
Der Primär-Target ist `wasm32-unknown-unknown`. Browserintegration erfolgt über `wasm-bindgen`/`web-sys` und einen Web Worker.

## Konsequenzen
- kein Vertrauen auf `std::fs`
- kein Prozessmodell des Hosts
- keine implizite Thread-Nutzung
- Plattformzugriffe ausschließlich über explizite Adapter
