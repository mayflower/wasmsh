# ADR-0005: HIR/IR/VM statt direkter AST-Interpretation

## Status
Angenommen

## Kontext
AST-Direktausführung vermischt Syntax mit Laufzeitsemantik und erschwert Limits, Tracing und Optimierung.

## Entscheidung
Pipeline:
`AST -> HIR -> IR -> VM`

## Konsequenzen
- besseres Budgeting und Cancellation
- klarere Builtin-/Utility-Grenzen
- mehr Architekturaufwand, aber bessere Skalierbarkeit
