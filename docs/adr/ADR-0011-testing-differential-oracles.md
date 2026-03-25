# ADR-0011: Originäre Tests plus Differential Oracles

## Status
Angenommen (aktualisiert 2026-03)

## Kontext
Kompatibilität muss messbar sein, ohne GPL-Testkorpora ins Repo zu ziehen.

## Entscheidung
Das Repo enthält nur originäre Tests. Zusätzlich kann lokal/CI ein optionaler Differential-Harness gegen installierte Referenzshells laufen.

## Aktueller Stand

960 Tests insgesamt: 506 Unit-Tests + 454 TOML-Integrationstests.

### Unit-Tests (506)
- **wasmsh-utils**: 400 Unit-Tests decken alle 86 Utilities ab
- **wasmsh-parse**: Parser-Tests inkl. Property-based Fuzzing (proptest)
- **wasmsh-lex**, **wasmsh-expand**, **wasmsh-vm**, etc.: Crate-spezifische Tests

### TOML-Integrationstests (454)
- Deklarative `[[test]]`-Tabellen in TOML-Dateien
- Shell-Semantik, Utility-Verhalten, Redirections, Expansions
- **60 Real-World-Integrationstests** (rw01–rw60): Multi-Tool-Pipelines aus der Praxis (CI/CD, Log-Analyse, ETL-Pipelines, Deployment-Automatisierung, Schema-Validierung, Crontab-Management, etc.)

### Property-based Fuzzing
- `proptest` in `wasmsh-parse/tests/property_tests.rs`
- Lexer und Parser paniken nie bei beliebiger Eingabe
- Fuzz-Generatoren erzeugen syntaktisch strukturierte und rein zufällige Eingaben

### Benchmarks
- **Criterion-Benchmarks** für Parser (`wasmsh-parse/benches/parse_bench.rs`), Expansion (`wasmsh-expand/benches/expand_bench.rs`) und Pipeline-Ausführung (`wasmsh-browser/benches/pipeline_bench.rs`)
- Regressionserkennung über CI (optional)

## Konsequenzen
- Saubere Provenance — kein Test ist aus GPL-Projekten kopiert
- Mehr Aufwand für Testdesign
- Trotzdem hohe Praxisnähe durch Real-World-Szenarien
- Property-Tests sichern Robustheit gegen beliebige Eingaben
- Benchmarks ermöglichen Performance-Tracking über Releases
