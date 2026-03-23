# ADR-0001: Clean-Room-Kompatibilität statt Codeübernahme

## Status
Angenommen

## Kontext
wasmsh will hohe Shell-Kompatibilität erreichen, darf dafür aber keinen BusyBox- oder Bash-Code in den Kern übernehmen.

## Entscheidung
Das Projekt verfolgt **Verhaltenskompatibilität** als Ziel, aber **keine Quellcode-, Hilfetext-, Tabellen- oder Testsuite-Übernahme** aus BusyBox/Bash.

## Konsequenzen
- wir implementieren Parser, Expansionen und Runtime selbst
- wir nutzen POSIX als normative Baseline
- Bash/BusyBox dienen höchstens als Black-Box-Referenz
- Provenance muss pro größerem PR dokumentiert werden
