# ADR-0014: Eingebettete Interpreter für komplexe Utilities

## Status
Angenommen

## Kontext
Wichtige Agent-Utilities (jq, awk, yq, bc) benötigen eigene Ausdruckssprachen mit Parser und Evaluator. Alternativen wären externe Crate-Dependencies oder Delegation an den Host.

## Entscheidung
Jedes komplexe Utility wird als eigenständiger Interpreter innerhalb `wasmsh-utils` implementiert: Tokenizer → Parser → AST → Evaluator. Kein externer Interpreter, keine GPL-Abhängigkeit.

- **jq**: JSON-Parser, Filter-Sprache, 90+ Built-in-Funktionen
- **awk**: Lexer, rekursiver Abstieg, assoziative Arrays, User-Funktionen, eigene Regex-Engine
- **yq**: YAML-Parser mit Einrückungsverfolgung, jq-kompatibler Filter-Subset
- **bc**: Ausdrucksparser mit Variablen, Kontrollfluss, User-Funktionen

## Konsequenzen
- Clean-Room-Garantie bleibt erhalten (keine externen Interpreter-Deps)
- Sicherheit durch Iterationslimits (awk: 1M, jq: Tiefe 1000) und Allokationsschutz
- ~12.000 Zeilen Interpreter-Code als Wartungslast
- Eigene Regex-Engines in awk und rg (kein regex-Crate)
