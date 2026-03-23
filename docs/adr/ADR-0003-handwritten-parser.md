# ADR-0003: Handgeschriebener Parser

## Status
Angenommen

## Kontext
Shell-Syntax ist kontextsensitiv und koppelt Tokenisierung, Quoting, Here-Docs und Expansion eng.

## Entscheidung
wasmsh verwendet:
- einen handgeschriebenen stateful lexer
- einen handgeschriebenen recursive-descent parser
- separate Subparser für `${}`, `$(( ))`, `[[ ]]` und Patterns

## Konsequenzen
- bessere Kontrolle über Shell-Eckenfälle
- höherer Initialaufwand
- dafür klarere Fehlerdiagnosen und bessere Browser-Tauglichkeit
