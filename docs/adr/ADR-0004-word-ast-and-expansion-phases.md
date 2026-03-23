# ADR-0004: Strukturierte Word-ASTs und späte Expansion

## Status
Angenommen

## Kontext
Shell-Wörter dürfen nicht zu früh in Strings kollabieren, sonst gehen Quoting- und Expansionseigenschaften verloren.

## Entscheidung
Wörter werden als strukturierte `WordPart`-Bäume modelliert. Expansion erfolgt in klar getrennten Phasen erst nach dem Parsing.

## Konsequenzen
- korrektes Verhalten für Quotes, Field Splitting und Globs
- mehr interne Typen
- bessere Testbarkeit
