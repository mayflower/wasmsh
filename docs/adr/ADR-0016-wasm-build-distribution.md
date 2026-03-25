# ADR-0016: WASM-Build und Distributionspipeline

## Status
Angenommen

## Kontext
wasmsh zielt auf Browser-Umgebungen via wasm32-unknown-unknown. Das kompilierte WASM muss aus JS-Bundlern (webpack, vite), ES-Modulen (Browser) und Node.js (CommonJS) nutzbar sein.

## Entscheidung
wasm-pack baut drei Targets aus dem wasmsh-browser-Crate:

- **bundler** (ES-Modul + .wasm) — webpack, vite, Rollup
- **web** (ES-Modul + .wasm) — Browser `<script type="module">`
- **nodejs** (CommonJS + .wasm) — Node.js, Testing, Python-Bridge

Crate-Type ist `["cdylib", "rlib"]`: `cdylib` erzeugt die .wasm-Binary via wasm-bindgen, `rlib` ermöglicht Rust-interne Abhängigkeit.

wasm-opt optimiert alle .wasm-Dateien mit `-Os` und aktiviert die von Rust 1.94+ benötigten Features (bulk-memory, mutable-globals, sign-ext, nontrapping-float-to-int).

### Distribution
- **GitHub Releases**: Tag-Push (`v*`) erzeugt Release mit bundler/web/nodejs-Tarballs
- **GitHub Actions Artifacts**: Jeder Push auf main lädt ephemere Artefakte hoch (90 Tage)
- **npm / crates.io**: Vorbereitet, noch nicht veröffentlicht

## Konsequenzen
- Drei Build-Targets decken das gesamte JS-Ökosystem ab
- wasm-bindgen generiert TypeScript-Typdefinitionen automatisch
- u64-Parameter werden zu BigInt in JavaScript (z.B. step_budget)
- Binary-Größe wird per CI-Budget (2 MB) überwacht
