# ADR-0011: Originäre Tests plus Differential Oracles

## Status
Angenommen

## Kontext
Kompatibilität muss messbar sein, ohne GPL-Testkorpora ins Repo zu ziehen.

## Entscheidung
Das Repo enthält nur originäre Tests. Zusätzlich kann lokal/CI ein optionaler Differential-Harness gegen installierte Referenzshells laufen.

## Konsequenzen
- saubere Provenance
- mehr Aufwand für Testdesign
- trotzdem hohe Praxisnähe
