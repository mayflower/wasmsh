# ADR-0007: Command-Modell = Builtin / Function / Utility / Virtual External

## Status
Angenommen

## Kontext
Im Browser können Shell-Kommandos nicht auf ein klassisches fork/exec-Modell bauen.

## Entscheidung
Jedes Kommando wird beim Resolve in eine von vier Klassen einsortiert:
- Builtin
- Shell Function
- Bundled Utility
- Virtual External (nur Host-Profil)

## Konsequenzen
- Browserprofil bleibt realistisch
- Shell-State bleibt korrekt mutierbar
- Utilities können separat entwickelt werden
