# ADR-0008: Browserausführung in Web Worker

## Status
Angenommen

## Kontext
Parsing, Expansion und Execution dürfen die UI nicht blockieren.

## Entscheidung
wasmsh läuft im Browser immer in einem Worker und kommuniziert über ein versioniertes Nachrichtenprotokoll.

## Konsequenzen
- gute UI-Reaktionsfähigkeit
- saubere Entkopplung zwischen Shell-Kern und Frontend
- Message-Protokoll muss stabil gehalten werden
