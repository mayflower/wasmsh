# ADR-0013: Eigener Diagnose- und Fehlermodell-Stack

## Status
Angenommen

## Kontext
Shells scheitern oft an schwer lesbaren Parser- und Runtime-Fehlern.

## Entscheidung
Spans, Diagnosecodes, strukturierte Fehler und benutzerfreundliche Meldungen sind first-class.

## Konsequenzen
- bessere DX und UI-Integration
- höherer Initialaufwand
- später bessere Editor- und Agentenintegration
