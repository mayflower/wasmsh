# Bash 4 Implementation Gaps for `wasmsh`

## Scope and method

Diese Liste vergleicht die Bash-4-Spezifikation aus
`/Users/johann/Downloads/bash4_spezifikation.md` mit dem aktuell im Repo
sichtbaren Stand von `wasmsh`:

- `SUPPORTED.md`
- `docs/reference/shell-syntax.md`
- `docs/reference/builtins.md`
- `crates/wasmsh-state/src/lib.rs`
- `crates/wasmsh-builtins/src/lib.rs`
- `crates/wasmsh-runtime/src/lib.rs`
- `tests/suite/**`

Scope ist nur die Bash-4-Shell selbst: Grammatik, Expansionen, Builtins,
Spezialparameter, Redirections, Optionen, Traps und Job-Control. Die vielen
zusätzlichen `wasmsh`-Utilities sind bewusst nicht Teil dieses Vergleichs.

## Status nach Umsetzung vom 2026-04-12

Die Arbeitsblöcke mit `Machbarkeit M0/M1` und `Priorität P0/P1/P2` aus der
vorigen Bewertung sind für den aktuellen Stand weitgehend umgesetzt:

- geschlossen: Spezialparameter `$$`, `$!`, `$-`, `$_`
- geschlossen: Redirection-Lücken `&>>`, `>|`, `[n]<&-`, `[n]>&-`
- geschlossen: `time`-Keyword und `times`-Builtin
- geschlossen: `select`-Loop-Semantik für wiederholte Eingaben bis `break`/EOF
- geschlossen: Builtins `exec`, `hash`, `times`, `dirs`, `pushd`, `popd`, `umask`, `wait`, `ulimit`
- geschlossen: `read`-/`mapfile`-Teilmenge für `-u`, `-i`, `-e`, `-d`, `-n`, `-O`, `-s`, `-C`, `-c`
- geschlossen: Flag-Lücken bei `declare`/`typeset`, `export`, `readonly`, `type`, `command`
- geschlossen: zusätzliche `test`-/`[[`-Operatoren wie `-L`, `-h`, `-p`, `-S`, `-t`, `-N`, `-O`, `-G`, `-ef`, `-nt`, `-ot`

Offen aus der priorisierten Liste bleiben damit vor allem die teureren Blöcke
`Job-Control`, `Trap-/Signalmodell`, `coproc` sowie die weiterhin breiteren
Lücken bei interaktiven Builtins und `set`-/`shopt`-Optionen.

## Priorisierungslogik

- `P0`: größter Bash-Kompatibilitätsbruch für nichttriviale Skripte
- `P1`: wichtige Bash-4-Features oder deutliche semantische Teilimplementierung
- `P2`: mittlere Abdeckungslücken bei Flags, Optionen oder Primaries
- `P3`: interaktive/TTY-zentrierte Features mit geringerem Nutzen im WASM-Sandbox-Modell

## Machbarkeitslogik

- `M0`: kleiner, lokaler Eingriff; gute Passung zur bestehenden Architektur
- `M1`: gut machbar, aber mit Änderungen in mehreren Komponenten oder Tests
- `M2`: größerer Querschnittseingriff mit spürbarer Runtime-/Parser-/State-Arbeit
- `M3`: architektonisch teuer oder nur eingeschränkt passend zum WASM-/Sandbox-Modell

## Logik der kombinierten Bewertung

- `K0`: bester nächster Arbeitsblock; hoher Nutzen bei guter Machbarkeit
- `K1`: sinnvoll danach; guter Nutzen, aber etwas breiter oder semantisch heikler
- `K2`: wichtig, aber eher strategisch/langfristig wegen Aufwand oder Modellkonflikt
- `K3`: derzeit niedriger ROI; entweder nischig, interaktiv oder architektonisch unpassend

## Priorisierte Gaps

### P0

#### 1. Echte Hintergrundausführung und Job-Control fehlen

- Bash-4-Spezifikation:
  Abschnitt 6, 14, 15, 16 und 21 decken `cmd &`, `$!`, `wait`, `jobs`,
  `fg`, `bg`, `disown`, `suspend`, `set -m/monitor` und Jobspecs (`%1`,
  `%+`, `%-`) ab.
- Aktueller `wasmsh`-Stand:
  `SUPPORTED.md` sagt explizit, dass `cmd &` zwar geparst wird, aber
  synchron läuft. In `docs/reference/sandbox-and-capabilities.md` werden
  `wait`, `jobs` und echte Signal-/Prozesssemantik ebenfalls als nicht
  vorhanden beschrieben.
- Warum `P0`:
  Das ist der größte einzelne Semantikbruch gegenüber Bash. Viele Shellskripte
  nutzen `&` nicht für Komfort, sondern für tatsächliche Parallelität,
  `wait`-Synchronisation und PID-basierte Steuerung.

#### 2. Trap- und Signalmodell ist nur als Minimalvariante vorhanden

- Bash-4-Spezifikation:
  Abschnitt 20 verlangt neben `EXIT` und `ERR` auch `DEBUG`, `RETURN`,
  `trap -l`, `trap -p`, Reset/Ignore-Semantik und reguläre Signalnamen.
- Aktueller `wasmsh`-Stand:
  `trap` ist implementiert, aber laut `SUPPORTED.md` und
  `crates/wasmsh-builtins/src/lib.rs` nur für `EXIT` und `ERR`; andere
  Signale sind Warnung oder No-op.
- Warum `P0`:
  Cleanup-, Debug- und Fehlerbehandlungslogik in Bash hängt stark an `trap`.
  Ohne konsistente Trap-Semantik bleiben auch robustere Skripte unportabel.

#### 3. Wichtige Spezialparameter fehlen oder sind nur dokumentiert, nicht implementiert

- Bash-4-Spezifikation:
  Abschnitt 14 verlangt mindestens `$-`, `$$`, `$!`, `$_` zusätzlich zu
  `$?`, `$#`, `$@`, `$*`, `$0`.
- Aktueller `wasmsh`-Stand:
  `crates/wasmsh-state/src/lib.rs` liefert sichtbar nur `?`, `#`, `0`,
  `@`, `*`, `RANDOM`, `LINENO`, `SECONDS`, `FUNCNAME`, `BASH_SOURCE`.
  `$$`, `$!`, `$-` und `$_` werden dort nicht aufgelöst.
- Warum `P0`:
  Diese Parameter sind klein, aber in realen Skripten häufig. Außerdem
  erzeugt die Diskrepanz zwischen `SUPPORTED.md` und Runtime zusätzlichen
  Integrationsschaden.

### P1

#### 4. `coproc` fehlt komplett

- Bash-4-Spezifikation:
  Abschnitt 8 beschreibt `coproc [NAME] command`.
- Aktueller `wasmsh`-Stand:
  `SUPPORTED.md` und `docs/reference/shell-syntax.md` nennen `coproc` als
  nicht implementiert.
- Warum `P1`:
  `coproc` ist klar Bash-4-spezifisch und kein Randdetail. Es ist seltener
  als Job-Control, aber ein echter Sprachbaustein, nicht nur ein Flag.

#### 5. Redirection-Oberfläche ist nicht vollständig auf Bash-4-Niveau

- Bash-4-Spezifikation:
  Abschnitt 11 verlangt u. a. `&>>`, `>|`, `[n]<&-`, `[n]>&-` und die
  üblichen FD-Duplikationsfälle.
- Aktueller `wasmsh`-Stand:
  Öffentlich dokumentiert sind `<`, `>`, `>>`, `<>`, `<<`, `<<-`, `<<<`,
  `2>`, `2>>`, `2>&1` und `&>`.
  Im Lexer/AST sind keine offensichtlichen Tokens für `>|` oder `&>>`
  sichtbar; `SUPPORTED.md` listet beide ebenfalls nicht.
- Warum `P1`:
  Das sind keine exotischen Features. `>|` ist für `noclobber` relevant,
  `&>>` ist explizit eine Bash-4-Erweiterung.

#### 6. `select` ist nur teilweise bash-kompatibel

- Bash-4-Spezifikation:
  `select` ist ein Schleifenkonstrukt, das bis `break` oder EOF weiterläuft.
- Aktueller `wasmsh`-Stand:
  `SUPPORTED.md` beschreibt `select` als "single iteration in sandbox".
- Warum `P1`:
  Die Syntax existiert, aber die Laufzeitsemantik ist eingeschränkt. Das
  ist gefährlicher als ein klar fehlendes Feature, weil Skripte scheinbar
  laufen und dann semantisch anders reagieren.

#### 7. Mehrere wichtige nicht-interaktive Builtins aus der Spezifikation fehlen

- Bash-4-Spezifikation:
  Abschnitt 15 listet unter anderem `exec`, `hash`, `times`, `umask`,
  `ulimit`, `wait`, `dirs`, `pushd`, `popd`.
- Aktueller `wasmsh`-Stand:
  Diese Builtins tauchen weder in `docs/reference/builtins.md` noch in der
  Builtin-Registry in `crates/wasmsh-builtins/src/lib.rs` als vollwertig
  implementiert auf.
- Warum `P1`:
  Das sind klassische Skript-Bausteine. Ein Teil davon ist im Browser-Modell
  schwierig (`ulimit`), anderes wäre trotzdem nützlich (`exec` für FD-/scope-
  Semantik, `wait` mit künftiger Job-Control, `pushd/popd` für Skripte).

### P2

#### 8. `read` und `mapfile` decken nur einen Teil der Bash-4-Flags ab

- Bash-4-Spezifikation:
  `read` umfasst u. a. `-u fd`, `-i text`, `-e`; `mapfile` umfasst u. a.
  `-d`, `-n`, `-O`, `-s`, `-u`, `-C`, `-c`.
- Aktueller `wasmsh`-Stand:
  `docs/reference/builtins.md` dokumentiert bei `read` nur `-r`, `-p`,
  `-d`, `-n`, `-N`, `-a`, `-t`, `-s`; bei `mapfile` nur `-t`.
- Warum `P2`:
  Für viele einfache Skripte reicht die vorhandene Teilmenge, aber der
  Abstand zur Bash-4-Spezifikation bleibt deutlich.

#### 9. `declare`/`typeset`, `export`, `readonly`, `type` und `command` sind flag-seitig unvollständig

- Bash-4-Spezifikation:
  Abschnitt 12 und 15 verlangt weitere Modi wie `declare -f/-F/-t`,
  `export -f/-n/-p`, `readonly -a/-A/-f/-p`, `type -afptP`,
  `command -pVv`.
- Aktueller `wasmsh`-Stand:
  Die Referenzdoku deckt jeweils nur eine sinnvolle Teilmenge ab.
- Warum `P2`:
  Diese Lücken brechen weniger häufig Standardskripte als Job-Control oder
  Traps, aber sie begrenzen echte Bash-Kompatibilität.

#### 10. Die Test-/`[[`-Operatoren decken nicht die volle Bash-4-Menge ab

- Bash-4-Spezifikation:
  Abschnitt 19 listet viele zusätzliche Dateioperatoren wie `-L`, `-h`,
  `-p`, `-S`, `-t`, `-N`, `-O`, `-G`, `-ef`, `-nt`, `-ot`.
- Aktueller `wasmsh`-Stand:
  `docs/reference/builtins.md` dokumentiert nur `-f`, `-d`, `-e`, `-s`,
  `-r`, `-w`, `-x`, `-n`, `-z` sowie die arithmetischen Vergleichsoperatoren.
- Warum `P2`:
  Das ist eine klare Coverage-Lücke, auch wenn einige Tests im Sandbox-VFS
  nur begrenzt sinnvoll sind.

#### 11. `set`- und `shopt`-Optionen sind nur als sinnvolle Teilmenge vorhanden

- Bash-4-Spezifikation:
  Abschnitt 16 nennt deutlich mehr Optionen als aktuell dokumentiert,
  unter anderem `-b`, `-h`, `-k`, `-n`, `-p`, `-t`, `-v`, `-B`, `-E`,
  `-H`, `-P`, `-T`, `posix`, `vi`, `emacs` sowie viele weitere `shopt`-Flags.
- Aktueller `wasmsh`-Stand:
  Dokumentiert und getestet sind vor allem `errexit`, `nounset`, `xtrace`,
  `noglob`, `allexport`, `noclobber`, `pipefail` und einige Glob-Optionen.
- Warum `P2`:
  Für agentische/nicht-interaktive Nutzung reicht die aktuelle Auswahl oft,
  für Bash-4-Kompatibilität aber nicht.

#### 12. `time`-Keyword und `times`-Builtin fehlen

- Bash-4-Spezifikation:
  `time` gehört zur Shell-Grammatik, `times` zu den POSIX-Builtins.
- Aktueller `wasmsh`-Stand:
  `docs/reference/shell-syntax.md` nennt das `time`-Keyword als nicht
  implementiert; `times` ist nirgends als Builtin dokumentiert.
- Warum `P2`:
  Nicht zentral für die meisten Skripte, aber klarer Spezifikationsabstand.

### P3

#### 13. Interaktive, History- und Completion-Builtins fehlen weitgehend

- Bash-4-Spezifikation:
  Abschnitt 15.2 nennt `bind`, `caller`, `compgen`, `complete`, `compopt`,
  `fc`, `history`, `help`, `logout`.
- Aktueller `wasmsh`-Stand:
  Diese Builtins sind in der öffentlichen Referenz nicht vorhanden.
- Warum `P3`:
  Sie gehören zu Bash 4, haben aber im browser-/worker-zentrierten
  `wasmsh`-Modell deutlich weniger praktischen Wert.

#### 14. Interaktive `shopt`-Features fehlen weitgehend

- Bash-4-Spezifikation:
  Optionen wie `autocd`, `cdspell`, `checkjobs`, `cmdhist`, `direxpand`,
  `dirspell`, `histappend`, `lithist`, `xpg_echo`.
- Aktueller `wasmsh`-Stand:
  Die dokumentierte `shopt`-Menge fokussiert auf Pattern Matching und
  nicht-interaktive Shelllogik.
- Warum `P3`:
  Diese Features sind Bash-4-konform, aber für die Kernmission von
  `wasmsh` nur nachrangig.

## Dinge, die ich bewusst **nicht** als aktuelle Implementation Gaps zähle

### Prozesssubstitution

- `SUPPORTED.md` und `docs/reference/shell-syntax.md` nennen `<(cmd)` und
  `>(cmd)` noch als nicht implementiert.
- Der Runtime-Code und mehrere Tests zeigen aber, dass Prozesssubstitution
  inzwischen real vorhanden ist, inklusive `cat <(…)`, `diff <(…)` und
  `>(cat)`-Fällen.
- Das ist daher eher ein Doku-Gap als ein Runtime-Gap.

### Große Teile der Bash-4-Kernsprache sind bereits abgedeckt

Nicht mehr in der Gap-Liste, weil im Repo klar vorhanden:

- `[[ ... ]]` inklusive Regex und `BASH_REMATCH`
- `(( ... ))` und C-style `for (( ... ))`
- `case` mit `;&` und `;;&`
- Indexed und associative arrays
- viele Parameter-Expansionen inklusive indirekter und transformierender Formen
- `extglob`, `globstar`, `pipefail`, `nounset`, `xtrace`
- `select` als Basiskonstrukt, wenn auch noch nicht mit voller Semantik

## Machbarkeitsbewertung und kombinierte Priorisierung

| Nr. | Gap | Kompatibilität | Machbarkeit | Kombiniert | Kurzbegründung |
| --- | --- | --- | --- | --- | --- |
| 1 | Hintergrundausführung und Job-Control | `P0` | `M3` | `K2` | Sehr hoher Bash-Wert, aber im aktuellen Prozess-/Sandbox-Modell der teuerste Eingriff. |
| 2 | Trap- und Signalmodell | `P0` | `M2` | `K1` | Wichtig für robuste Skripte; machbar, aber mit Runtime- und Fehlerpfad-Folgen. |
| 3 | Spezialparameter (`$$`, `$!`, `$-`, `$_`) | `P0` | `M0` | `K0` | Sehr hoher Nutzen bei vermutlich lokalem State-/Expansion-Fix. |
| 4 | `coproc` | `P1` | `M3` | `K3` | Echte Bash-4-Funktion, aber stark gekoppelt an parallele Prozess- und FD-Semantik. |
| 5 | Redirection-Lücken (`&>>`, `>|`, FD-close) | `P1` | `M1` | `K0` | Gute Chance auf klare Parser-/Executor-Arbeit mit hohem Skript-Nutzen. |
| 6 | `select` nur teilimplementiert | `P1` | `M1` | `K1` | Semantisch wichtig, aber vermutlich ohne grundlegenden Architekturumbau zu schließen. |
| 7 | Fehlende nicht-interaktive Builtins | `P1` | `M1` | `K1` | Ein Teil ist direkt ergänzbar; einzelne Builtins wie `wait` bleiben an Job-Control gekoppelt. |
| 8 | `read`-/`mapfile`-Flags unvollständig | `P2` | `M1` | `K1` | Gute inkrementelle Kompatibilitätsarbeit mit überschaubarem Risiko. |
| 9 | Flag-Lücken bei `declare`/`export`/`readonly`/`type`/`command` | `P2` | `M0` | `K1` | Eher lokale Builtin-Erweiterungen, aber mit geringerem Hebel als Spezialparameter/Redirections. |
| 10 | Lücken bei Test-/`[[`-Operatoren | `P2` | `M1` | `K2` | Meist gut implementierbar, aber im Sandbox-VFS nicht jeder Operator gleich wertvoll. |
| 11 | `set`-/`shopt`-Optionen unvollständig | `P2` | `M2` | `K2` | Viele kleine Schalter, aber semantisch breit über Parser, State und Executor verteilt. |
| 12 | `time` und `times` fehlen | `P2` | `M1` | `K2` | Klar definierbar, aber für reale Portabilität unterhalb anderer Lücken. |
| 13 | History-/Completion-/interaktive Builtins | `P3` | `M3` | `K3` | Geringer Fit zum Kernmodell von `wasmsh`. |
| 14 | Interaktive `shopt`-Features | `P3` | `M2` | `K3` | Ebenfalls niedriger ROI für die primären nicht-interaktiven Use Cases. |

## Kombinierte Bewertung nach Arbeitsblöcken

### K0

- `3`: Spezialparameter (`$$`, `$!`, `$-`, `$_`)
- `5`: Redirection-Lücken (`&>>`, `>|`, FD-close)

### K1

- `2`: Trap- und Signalmodell
- `6`: `select`-Semantik vervollständigen
- `7`: fehlende nicht-interaktive Builtins, zuerst die nicht an Job-Control hängenden
- `8`: `read`-/`mapfile`-Flags ergänzen
- `9`: Flag-Lücken bei deklarativen Builtins schließen

### K2

- `1`: Job-Control und echte Hintergrundausführung
- `10`: zusätzliche Test-/`[[`-Operatoren
- `11`: weitere `set`-/`shopt`-Optionen
- `12`: `time` und `times`

### K3

- `4`: `coproc`
- `13`: History-/Completion-Builtins
- `14`: interaktive `shopt`-Features

## Empfohlene Arbeitsreihenfolge auf Basis der kombinierten Bewertung

1. Spezialparameter bereinigen und Dokumentation mit Runtime synchronisieren
2. Redirection-Lücken (`&>>`, `>|`, FD-close) schließen
3. Trap-/Signalmodell ausbauen
4. fehlende nicht-interaktive Builtins ergänzen, zuerst ohne Job-Control-Abhängigkeit
5. deklarative Builtin-Flags sowie `read`/`mapfile` inkrementell komplettieren
6. `select` von der Teilsemantik zur echten Schleifensemantik bringen
7. erst danach Job-/Prozessmodell für `&`, `wait`, Jobspecs und `$!` angehen
8. `coproc` und interaktive Bash-Features zuletzt behandeln
