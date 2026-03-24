# Implementation Gaps: bash.md vs. wasmsh

> **Status as of 2026-03-24: All 46 gaps have been implemented.** This document is
> retained as a historical record of the analysis and the implementation sequence that
> was followed. Each section is marked `[IMPLEMENTED]`.

Gaps relevant to a browser-based sandbox shell. Features that require OS processes,
real signals, TTY, job control, or readline are excluded — they are architecturally
impossible or irrelevant in wasm32-unknown-unknown.

---

## Bewertungskriterien

| Prioritaet | Bedeutung |
|-----------|-----------|
| **P0** | Blockiert gaengige Skripte -- haeufig in realen Bash-Snippets |
| **P1** | Erwartet von erfahrenen Nutzern -- haeufig in Tutorials/Stack Overflow |
| **P2** | Nuetzlich, aber selten blockerend |
| **P3** | Nice-to-have, keine gaengigen Use-Cases im Sandbox-Kontext |

---

## 1. Syntax & Parsing

### 1.1 `[[ ... ]]` -- Extended Test (P0) [IMPLEMENTED]

**bash.md:** Kein Word-Splitting, Glob-Matching mit `==`/`!=`, Regex mit `=~`, `&&`/`||` Logik, `BASH_REMATCH`.

**wasmsh:** Nicht implementiert. Nur `test`/`[` vorhanden.

**Relevanz:** Extrem haeufig. Nahezu jedes moderne Bash-Skript nutzt `[[ ]]` statt `[ ]`. Blockiert die meisten realen Skripte.

### 1.2 C-Style `for (( ))` Loop (P0) [IMPLEMENTED]

**bash.md:** `for (( init; cond; step )); do ... done`

**wasmsh:** Nur `for var in words` implementiert.

**Relevanz:** Sehr haeufig in numerischen Schleifen. Standard-Pattern fuer Iteration.

### 1.3 `(( ))` -- Arithmetic Command (P1) [IMPLEMENTED]

**bash.md:** `(( expr ))` als Statement, Return 0 wenn non-zero, 1 wenn zero. `(( i++ ))`, `(( x = y + 1 ))`.

**wasmsh:** Nicht implementiert als eigenstaendiges Compound-Command. Nur `$(( ))` fuer Expansion.

**Relevanz:** Haeufig fuer Inkremente, Vergleiche, Zuweisungen in Skripten.

### 1.4 `|&` -- Pipe stderr+stdout (P2) [IMPLEMENTED]

**bash.md:** `cmd1 |& cmd2` = `cmd1 2>&1 | cmd2`.

**wasmsh:** Lexer hat kein `|&` Token.

**Relevanz:** Gelegentlich, Workaround ueber `2>&1 |` existiert.

### 1.5 `;&` und `;;&` in `case` (P2) [IMPLEMENTED]

**bash.md:** Fall-through (`;&`) und Continue-Testing (`;;&`) seit Bash 4.0.

**wasmsh:** Nur `;;` implementiert.

**Relevanz:** Selten, aber in fortgeschrittenen Pattern-Matching-Skripten.

### 1.6 `select` Loop (P3) [IMPLEMENTED]

**bash.md:** Nummeriertes Menue mit `PS3` Prompt.

**wasmsh:** Parser kennt `select` als Reserved Word, aber nicht implementiert.

**Relevanz:** Interaktiv-only. In Sandbox kaum relevant.

---

## 2. Variablen & Arrays

### 2.1 Indexed Arrays (P0) [IMPLEMENTED]

**bash.md:** `arr=(a b c)`, `arr[0]=x`, `${arr[0]}`, `${arr[@]}`, `${#arr[@]}`, `${!arr[@]}`, `arr+=(val)`.

**wasmsh:** Keine Array-Unterstuetzung. `ShellVar.value` ist `SmolStr` (skalarer String).

**Relevanz:** Blockierend. Arrays sind fundamentaler Bash-Bestandteil, genutzt in Schleifen, Argument-Handling, Datenverarbeitung.

### 2.2 Associative Arrays (P1) [IMPLEMENTED]

**bash.md:** `declare -A map`, `map[key]=val`, `${map[key]}`, `${!map[@]}`.

**wasmsh:** Nicht implementiert.

**Relevanz:** Haeufig fuer Lookup-Tabellen, Konfigurationen, Counting-Patterns.

### 2.3 `declare`/`typeset` Builtin (P1) [IMPLEMENTED]

**bash.md:** `-i` Integer, `-l`/`-u` Case, `-r` Readonly, `-x` Export, `-a` Array, `-A` Assoc, `-n` Nameref, `-g` Global.

**wasmsh:** Nicht als Builtin vorhanden. `readonly` und `export` existieren einzeln.

**Relevanz:** Benoetigt fuer Arrays, Integer-Variablen, Namerefs. Gateway zu vielen Features.

### 2.4 `$RANDOM` (P1) [IMPLEMENTED]

**bash.md:** Zufallszahl 0-32767 bei jeder Referenz.

**wasmsh:** Nicht implementiert (keine dynamischen Spezialvariablen).

**Relevanz:** Haeufig fuer Temp-Dateien, Zufallsauswahl, Jitter in Skripten.

### 2.5 `$LINENO` (P1) [IMPLEMENTED]

**bash.md:** Aktuelle Zeilennummer.

**wasmsh:** Nicht implementiert.

**Relevanz:** Debugging, Error-Meldungen, Logging in Skripten.

### 2.6 `$PIPESTATUS` (P2) [IMPLEMENTED]

**bash.md:** Array der Exit-Codes aller Pipeline-Kommandos.

**wasmsh:** Nicht implementiert.

**Relevanz:** Relevant bei `set -o pipefail`-Debugging.

### 2.7 `$SECONDS` (P2) [IMPLEMENTED]

**bash.md:** Sekunden seit Shell-Start. Zuweisbar zum Zuruecksetzen.

**wasmsh:** Nicht implementiert.

**Relevanz:** Timing, Timeout-Logik in Skripten.

### 2.8 `$FUNCNAME` / `$BASH_SOURCE` (P2) [IMPLEMENTED]

**bash.md:** Call-Stack-Arrays fuer Funktionsname und Quelldatei.

**wasmsh:** Nicht implementiert.

**Relevanz:** Debugging, `source`-Stack-Tracking.

### 2.9 Namerefs `declare -n` (P3) [IMPLEMENTED]

**bash.md:** `declare -n ref=other` -- Indirekte Referenz.

**wasmsh:** Nicht implementiert.

**Relevanz:** Fortgeschrittenes Bash-Pattern, selten in einfachen Skripten.

---

## 3. Parameter Expansion

### 3.1 `${var/pat/rep}` Glob-Pattern statt Literal (P1) [IMPLEMENTED]

**bash.md:** Pattern nutzt Glob-Syntax (`*`, `?`, `[...]`).

**wasmsh:** `${var/pat/rep}` implementiert, aber nutzt `str::replace` (literaler String-Match statt Glob).

**Relevanz:** Viele Skripte erwarten Glob-Matching: `${path/\*/.}`.

### 3.2 `${var/#pat/rep}` / `${var/%pat/rep}` -- Anchored Substitution (P1) [IMPLEMENTED]

**bash.md:** Nur am Anfang (`/#`) oder Ende (`/%`) matchen.

**wasmsh:** Nicht implementiert.

**Relevanz:** Haeufig fuer Prefix/Suffix-Ersetzung.

### 3.3 Case-Modification `${var^}`, `${var^^}`, `${var,}`, `${var,,}` (P1) [IMPLEMENTED]

**bash.md:** Uppercase/Lowercase einzeln oder global.

**wasmsh:** Nicht implementiert.

**Relevanz:** Haeufig. Alternative zu `tr` fuer Variablen-Normalisierung.

### 3.4 Indirect Expansion `${!name}` (P2) [IMPLEMENTED]

**bash.md:** Expandiert `name`, nutzt Ergebnis als Variablenname.

**wasmsh:** Nicht implementiert.

**Relevanz:** Dynamische Variable-Lookup-Patterns.

### 3.5 Name/Prefix Expansion `${!prefix*}` (P2) [IMPLEMENTED]

**bash.md:** Alle Variablennamen mit gegebenem Prefix.

**wasmsh:** Nicht implementiert.

**Relevanz:** Iteration ueber Variablengruppen (z.B. alle `MYAPP_*`).

### 3.6 Transformation `${var@Q}`, `${var@E}`, `${var@U}`, `${var@L}` (P3) [IMPLEMENTED]

**bash.md:** Quoting, Escape-Expansion, Upper/Lower (Bash 4.4+).

**wasmsh:** Nicht implementiert.

**Relevanz:** Nuetzlich, aber selten blockerend.

---

## 4. Arithmetic

### 4.1 Fehlende Operatoren (P0) [IMPLEMENTED]

**bash.md:** `==`, `!=`, `<`, `>`, `<=`, `>=`, `&&`, `||`, `!`, `~`, `&`, `|`, `^`, `<<`, `>>`, `?:` (Ternary), `,` (Comma), `++`/`--`, `**`, Compound-Assignment (`+=`, `-=`, etc.).

**wasmsh:** Nur `+`, `-`, `*`, `/`, `%`. Kein Vergleich, keine Logik, kein Bitwise, kein Ternary, keine Zuweisung.

**Relevanz:** Blockierend. `$(( x > 0 ))`, `$(( flag & 0xFF ))`, `$(( x == y ? a : b ))` sind gaengig.

### 4.2 Variablen-Zuweisung in Arithmetic (P1) [IMPLEMENTED]

**bash.md:** `$(( x = 5 ))`, `$(( x += 1 ))`, `$(( x++ ))`.

**wasmsh:** Nicht implementiert. Arithmetic ist read-only.

**Relevanz:** Haeufig fuer Counter-Inkremente.

### 4.3 Klammern/Praezedenz in Arithmetic (P1) [IMPLEMENTED]

**bash.md:** `$(( (a + b) * c ))`.

**wasmsh:** Keine Klammerunterstuetzung im Arithmetic-Evaluator.

**Relevanz:** Jeder nicht-triviale Ausdruck braucht Klammern.

### 4.4 Hex/Octal/Binary Literale (P2) [IMPLEMENTED]

**bash.md:** `0xFF`, `077`, `2#1010`, `base#n`.

**wasmsh:** Nur dezimale Literale.

**Relevanz:** Gelegentlich bei Bit-Manipulation, Permissions.

---

## 5. Builtins

### 5.1 `alias` / `unalias` (P1) [IMPLEMENTED]

**bash.md:** Alias-Definition und -Expansion.

**wasmsh:** Nicht implementiert.

**Relevanz:** Haeufig in interaktiven Shells. In Sandbox nuetzlich fuer User-Shortcuts.

### 5.2 `let` (P1) [IMPLEMENTED]

**bash.md:** `let "expr"` -- Arithmetic-Evaluation als Builtin.

**wasmsh:** Nicht implementiert.

**Relevanz:** Alternative zu `(( ))` fuer Arithmetic.

### 5.3 `printf` -- Fehlende Format-Specifier (P1) [IMPLEMENTED]

**bash.md:** `%x`, `%o`, `%f`, `%e`, `%c`, `%b`, `%q`, `%(fmt)T`, Width/Precision (`%10s`, `%-20s`, `%05d`).

**wasmsh:** Nur `%s`, `%d`, `%%`. Keine Width, Precision, Padding.

**Relevanz:** `printf "%-20s %5d\n" "$name" "$count"` ist Standard-Pattern.

### 5.4 `read` -- Fehlende Flags (P1) [IMPLEMENTED]

**bash.md:** `-a` (Array), `-d` (Delimiter), `-p` (Prompt), `-n`/`-N` (Zeichenanzahl), `-t` (Timeout), `-s` (Silent).

**wasmsh:** Nur `-r` implementiert.

**Relevanz:** `-p` fuer Prompts, `-a` fuer Array-Input, `-d` fuer Record-Processing sind haeufig.

### 5.5 `shopt` Builtin (P2) [IMPLEMENTED]

**bash.md:** Steuerung von `extglob`, `nullglob`, `dotglob`, `globstar`, `nocasematch`, etc.

**wasmsh:** Nicht implementiert.

**Relevanz:** Benoetigt fuer erweiterte Glob-Features.

### 5.6 `mapfile`/`readarray` (P2) [IMPLEMENTED]

**bash.md:** Zeilen aus stdin in Array lesen.

**wasmsh:** Nicht implementiert (auch weil keine Arrays).

**Relevanz:** Haeufig fuer Dateiverarbeitung, benoetigt erst Array-Support.

### 5.7 `builtin` Keyword (P2) [IMPLEMENTED]

**bash.md:** `builtin echo` -- Builtin direkt aufrufen, Funktion umgehen.

**wasmsh:** Nicht implementiert (aber `command` existiert).

**Relevanz:** Gelegentlich wenn Funktionen Builtins ueberschreiben.

### 5.8 `source`/`.` -- PATH-Suche (P2) [IMPLEMENTED]

**bash.md:** Sucht in `PATH` wenn kein `/` im Argument.

**wasmsh:** `source` existiert, aber PATH-Suche unklar.

**Relevanz:** Standard fuer Library-Loading.

### 5.9 `hash` (P3) [IMPLEMENTED]

**bash.md:** Command-Location-Cache.

**wasmsh:** Nicht relevant -- kein PATH-Lookup in Sandbox.

### 5.10 `enable` (P3) [IMPLEMENTED]

**bash.md:** Builtins ein/ausschalten.

**wasmsh:** Nicht relevant im Sandbox-Kontext.

---

## 6. Shell Options

### 6.1 `set -o pipefail` (P0) [IMPLEMENTED]

**bash.md:** Pipeline-Status = rechtester Non-Zero Exit.

**wasmsh:** `set` speichert Optionen als `SHOPT_*`-Variablen, aber Runtime prueft `pipefail` nicht.

**Relevanz:** Standard in produktiven Skripten: `set -euo pipefail`.

### 6.2 `set -u` / `nounset` (P0) [IMPLEMENTED]

**bash.md:** Fehler bei ungesetzten Variablen.

**wasmsh:** Variable gesetzt, aber nicht in Expansion erzwungen.

**Relevanz:** Gehoert zum Standard-Header `set -euo pipefail`.

### 6.3 `set -x` / `xtrace` (P1) [IMPLEMENTED]

**bash.md:** Kommandos nach Expansion ausgeben (Debug).

**wasmsh:** Variable gesetzt, aber kein Trace-Output im Runtime.

**Relevanz:** Primaeres Debug-Werkzeug.

### 6.4 `set -f` / `noglob` (P2) [IMPLEMENTED]

**bash.md:** Globbing deaktivieren.

**wasmsh:** Variable gesetzt, Runtime ignoriert sie.

**Relevanz:** Gelegentlich wenn Literale mit `*` verarbeitet werden.

### 6.5 `set -a` / `allexport` (P2) [IMPLEMENTED]

**bash.md:** Alle Variablen automatisch exportieren.

**wasmsh:** Nicht wirksam.

**Relevanz:** Haeufig in `.env`-Loading-Patterns.

---

## 7. Globbing & Pattern Matching

### 7.1 Extended Globbing `extglob` (P1) [IMPLEMENTED]

**bash.md:** `?(pat)`, `*(pat)`, `+(pat)`, `@(pat)`, `!(pat)`.

**wasmsh:** Nicht implementiert.

**Relevanz:** Haeufig fuer Datei-Filterung: `ls !(*.log)`, `rm *.@(jpg|png)`.

### 7.2 `globstar` / `**` Recursive (P1) [IMPLEMENTED]

**bash.md:** `**` matcht rekursiv alle Dateien und Unterverzeichnisse.

**wasmsh:** Nicht implementiert.

**Relevanz:** `for f in **/*.txt` ist gaengiges Pattern.

### 7.3 `nullglob` (P2) [IMPLEMENTED]

**bash.md:** Nicht-matchende Globs expandieren zu nichts statt Literal.

**wasmsh:** Nicht steuerbar (Verhalten unklar).

**Relevanz:** Wichtig fuer `for f in *.txt; do` wenn keine `.txt` existieren.

### 7.4 `dotglob` (P2) [IMPLEMENTED]

**bash.md:** `*` matcht auch Dotfiles.

**wasmsh:** Nicht steuerbar.

**Relevanz:** Gelegentlich fuer versteckte Dateien.

### 7.5 `nocasematch` (P3) [IMPLEMENTED]

**bash.md:** Case-insensitive Matching in `case` und `[[ ]]`.

**wasmsh:** Nicht implementiert.

---

## 8. Utilities

### 8.1 `mktemp` (P1) [IMPLEMENTED]

**bash.md:** Temporaere Datei/Verzeichnis erstellen.

**wasmsh:** Nicht vorhanden.

**Relevanz:** Standard-Pattern fuer sichere Temp-Dateien in Skripten.

### 8.2 `yes` (P2) [IMPLEMENTED]

**bash.md:** Wiederholte Ausgabe.

**wasmsh:** Nicht vorhanden.

**Relevanz:** Selten, aber fuer Pipeline-Tests nuetzlich.

### 8.3 `paste` (P2) [IMPLEMENTED]

**bash.md:** Zeilen von Dateien zusammenfuehren.

**wasmsh:** Nicht vorhanden.

**Relevanz:** Gelegentlich fuer Spalten-Zusammenfuehrung.

### 8.4 `md5sum`/`sha256sum` (P2) [IMPLEMENTED]

**bash.md:** Pruefsummen.

**wasmsh:** Nicht vorhanden.

**Relevanz:** Integrity-Checks. In Sandbox fuer Datenverifikation nuetzlich.

### 8.5 `base64` (P2) [IMPLEMENTED]

**bash.md:** Encode/Decode.

**wasmsh:** Nicht vorhanden.

**Relevanz:** Haeufig fuer Datenverarbeitung, API-Payloads.

### 8.6 `rev` (P3) [IMPLEMENTED]

**bash.md:** Zeilen umkehren.

**wasmsh:** Nicht vorhanden.

### 8.7 `column` (P3) [IMPLEMENTED]

**bash.md:** Tabellenformatierung.

**wasmsh:** Nicht vorhanden.

---

## Zusammenfassung nach Prioritaet

> **Alle 46 Gaps implementiert (Stand: 2026-03-24).**

### P0 -- Blockiert gaengige Skripte (5 Gaps) -- alle implementiert

| # | Gap | Bereich | Status |
|---|-----|---------|--------|
| 1 | `[[ ... ]]` Extended Test | Syntax | IMPLEMENTED |
| 2 | C-Style `for (( ))` | Syntax | IMPLEMENTED |
| 3 | Indexed Arrays | Variablen | IMPLEMENTED |
| 4 | Arithmetic: Vergleiche, Logik, Bitwise, Ternary, Klammern | Arithmetic | IMPLEMENTED |
| 5 | `set -o pipefail` + `set -u` Runtime-Enforcement | Options | IMPLEMENTED |

### P1 -- Erwartet von erfahrenen Nutzern (18 Gaps) -- alle implementiert

| # | Gap | Bereich | Status |
|---|-----|---------|--------|
| 6 | `(( ))` Arithmetic Command | Syntax | IMPLEMENTED |
| 7 | Associative Arrays | Variablen | IMPLEMENTED |
| 8 | `declare`/`typeset` Builtin | Builtins | IMPLEMENTED |
| 9 | `$RANDOM` | Variablen | IMPLEMENTED |
| 10 | `$LINENO` | Variablen | IMPLEMENTED |
| 11 | `${var/pat/rep}` mit Glob statt Literal | Expansion | IMPLEMENTED |
| 12 | `${var/#pat/rep}`, `${var/%pat/rep}` Anchored | Expansion | IMPLEMENTED |
| 13 | Case-Modification `${var^^}`, `${var,,}` | Expansion | IMPLEMENTED |
| 14 | Arithmetic: Zuweisung, `++`/`--`, Klammern | Arithmetic | IMPLEMENTED |
| 15 | Hex/Octal-Literale in Arithmetic | Arithmetic | IMPLEMENTED |
| 16 | `alias`/`unalias` | Builtins | IMPLEMENTED |
| 17 | `let` Builtin | Builtins | IMPLEMENTED |
| 18 | `printf` Width/Precision/Format-Specifier | Builtins | IMPLEMENTED |
| 19 | `read -p -a -d -n -t` Flags | Builtins | IMPLEMENTED |
| 20 | `set -x` Trace-Output | Options | IMPLEMENTED |
| 21 | Extended Globbing `extglob` | Globbing | IMPLEMENTED |
| 22 | `globstar` / `**` | Globbing | IMPLEMENTED |
| 23 | `mktemp` Utility | Utilities | IMPLEMENTED |

### P2 -- Nuetzlich, selten blockerend (15 Gaps) -- alle implementiert

| # | Gap | Bereich | Status |
|---|-----|---------|--------|
| 24 | `\|&` Pipe stderr | Syntax | IMPLEMENTED |
| 25 | `;&` / `;;&` in `case` | Syntax | IMPLEMENTED |
| 26 | `$PIPESTATUS` | Variablen | IMPLEMENTED |
| 27 | `$SECONDS` | Variablen | IMPLEMENTED |
| 28 | `$FUNCNAME` / `$BASH_SOURCE` | Variablen | IMPLEMENTED |
| 29 | Indirect Expansion `${!name}` | Expansion | IMPLEMENTED |
| 30 | Prefix Expansion `${!prefix*}` | Expansion | IMPLEMENTED |
| 31 | `shopt` Builtin | Builtins | IMPLEMENTED |
| 32 | `mapfile`/`readarray` | Builtins | IMPLEMENTED |
| 33 | `builtin` Keyword | Builtins | IMPLEMENTED |
| 34 | `set -f`/`noglob`, `set -a`/`allexport` | Options | IMPLEMENTED |
| 35 | `nullglob` | Globbing | IMPLEMENTED |
| 36 | `dotglob` | Globbing | IMPLEMENTED |
| 37 | `md5sum`/`sha256sum` | Utilities | IMPLEMENTED |
| 38 | `base64` | Utilities | IMPLEMENTED |

### P3 -- Nice-to-have (8 Gaps) -- alle implementiert

| # | Gap | Bereich | Status |
|---|-----|---------|--------|
| 39 | `select` Loop | Syntax | IMPLEMENTED |
| 40 | Namerefs `declare -n` | Variablen | IMPLEMENTED |
| 41 | `${var@Q}` Transformations | Expansion | IMPLEMENTED |
| 42 | `nocasematch` | Globbing | IMPLEMENTED |
| 43 | `$"..."` Locale Quoting | Quoting | IMPLEMENTED |
| 44 | `{varname}` Dynamic fd | Redirections | IMPLEMENTED |
| 45 | `rev`, `paste`, `column`, `yes` | Utilities | IMPLEMENTED |
| 46 | `N>&M-` Move fd | Redirections | IMPLEMENTED |

---

## Explizit ausgeschlossen (kein Gap)

Folgende bash.md-Features sind im Browser/Sandbox architekturbedingt
**nicht umsetzbar oder irrelevant**:

| Feature | Grund |
|---------|-------|
| Job Control (`&`, `fg`, `bg`, `jobs`, `disown`, `suspend`) | Kein OS-Prozessmodell |
| Signale (`kill`, `SIGINT`, `SIGTERM`, etc.) | Kein Signal-Delivery in WASM |
| Process Substitution `<(cmd)`, `>(cmd)` | Kein `/dev/fd`, keine FIFOs |
| Coprocesses (`coproc`) | Kein asynchrones Prozessmodell |
| Readline / Line Editing | Browser-UI uebernimmt |
| History Expansion (`!!`, `!$`, etc.) | Browser-UI uebernimmt |
| Programmable Completion | Browser-UI uebernimmt |
| Restricted Shell (`rbash`) | Sandbox ist bereits restricted |
| POSIX Mode Differenzen | Kein POSIX-Compliance-Ziel |
| `exec` (Prozess ersetzen) | Kein OS exec() |
| `umask` | Keine echten Permissions |
| `times` | Kein Prozess-Accounting |
| `hash` | Kein PATH-Lookup |
| `/dev/tcp`, `/dev/udp` | Kein Netzwerk-Zugriff |
| `ulimit` | Keine Kernel-Ressourcenlimits |
| `$$`, `$!`, `$PPID`, `$BASHPID` | Keine Prozess-IDs |
| `wait` | Keine Hintergrundprozesse |
| Login/Startup Files | Kein Login-Konzept |
| `enable -f` Loadable Builtins | Kein Shared-Object-Loading |
| Alle externen Binaries (docker, kubectl, git, curl, ssh, etc.) | Keine OS-Prozesse |

---

## Empfohlene Implementierungsreihenfolge

> **Abgeschlossen (Stand: 2026-03-24).** Alle drei Phasen wurden vollstaendig
> durchgefuehrt. Die Reihenfolge unten dokumentiert den tatsaechlich beschrittenen Weg.

**Phase 1 -- P0 Gaps schliessen (ermoeglicht ca. 90% aller Bash-Snippets):** DONE
1. Arithmetic-Evaluator komplett neu: Operatoren, Klammern, Zuweisung, Vergleiche
2. `[[ ]]` Parser + Runtime
3. Arrays (indexed) in `ShellState` + Expansion
4. C-Style `for (( ))` Parser + Runtime
5. `set -u`/`pipefail` Runtime-Enforcement

**Phase 2 -- P1 Gaps (erfahrene Nutzer zufriedenstellen):** DONE
6. `declare`/`typeset` mit `-i`, `-a`, `-A`, `-x`, `-r`
7. Associative Arrays
8. Case-Modification, Anchored Substitution, Glob in `${var/...}`
9. `$RANDOM`, `$LINENO`
10. `printf` Width/Precision
11. `read -p -a -d -n`
12. `alias`/`unalias`
13. `(( ))`, `let`
14. `extglob`, `globstar`
15. `mktemp`

**Phase 3 -- P2/P3 Gaps (Vollstaendigkeit):** DONE
16. Restliche Variablen: `$SECONDS`, `$PIPESTATUS`, `$FUNCNAME`
17. `shopt`, `mapfile`, `builtin`
18. Remaining expansion operators
19. Remaining utilities
