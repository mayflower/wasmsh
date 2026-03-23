# ADR-0009: Kooperative Budgets und Cancellation

## Status
Angenommen

## Kontext
Ein agentischer Sandbox-Service braucht Laufzeitgrenzen und Unterbrechbarkeit.

## Entscheidung
Die VM erhält:
- step budget
- memory budget hooks
- cancellation token
- event sink für progress/trace

## Konsequenzen
- kontrollierbare Laufzeit
- sichere Einbettung in Services
- etwas mehr VM-Komplexität
