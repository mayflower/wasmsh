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
`Job-Control`, `coproc` sowie die weiterhin breiteren Lücken bei interaktiven
Builtins und `set`-/`shopt`-Optionen.

Zusätzlich reduziert am 2026-04-12:

- `set`: `-E`, `-T`, `-n`, `-p`, `-v` sowie `set -o` / `set +o`
- `shopt`: `sourcepath`
- `source`: `PATH`-Suche jetzt dokumentiert und per `shopt sourcepath` schaltbar
- `trap`/Signalmodell: hostgetriebene Signalzustellung, `ERR`-/`DEBUG`-/`RETURN`-
  Vererbung gemäß `set -E`/`set -T`, sowie Ablehnung von `KILL`/`STOP` als
  nicht trappbar

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

#### 2. [erledigt] Trap-Randfälle und das sessionbasierte POSIX-Signalmodell

- Bash-4-Spezifikation:
  Abschnitt 20 verlangt neben `EXIT` und `ERR` auch `DEBUG`, `RETURN`,
  `trap -l`, `trap -p`, Reset/Ignore-Semantik und reguläre Signalnamen.
- Aktueller `wasmsh`-Stand:
  `trap` deckt inzwischen `EXIT`, `ERR`, `DEBUG`, `RETURN`, `trap -l`,
  `trap -p`, Reset/Ignore-Semantik und reguläre Signalnamen ab. Die
  Runtime modelliert jetzt auch hostgetriebene Signalzustellung für die
  Shell-Session, inklusive Default-Aktionen, Trap-Ausführung während
  laufender `StartRun`/`PollRun`-Ausführungen, `ERR`-Vererbung via
  `set -E` und `DEBUG`/`RETURN`-Vererbung via `set -T`. `KILL` und
  `STOP` werden jetzt korrekt als nicht trappbar abgelehnt.
- Restgrenze:
  Was offen bleibt, ist nicht mehr der Trap-Block selbst, sondern die
  an echtes Job-Control gebundene Stop/Continue-Semantik für reale
  Hintergrundjobs. Dieser Rest ist im Job-Control-Block aufgehoben.

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
  Dokumentiert und getestet sind jetzt zusätzlich `errtrace`, `functrace`,
  `noexec`, `privileged`, `verbose`, `set -o` / `set +o` sowie
  `shopt sourcepath`. Offen bleiben vor allem weitere `set`-Flags wie
  `-b`, `-h`, `-k`, `-t`, `-B`, `-H`, `-P`, Editor-/POSIX-Modi und die
  deutlich breitere `shopt`-Restmenge.
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
- `select` mit wiederholter Menüschleife bis `break` oder EOF

## Machbarkeitsbewertung und kombinierte Priorisierung

Historische Einträge, die seit der ersten Bewertung geschlossen wurden,
bleiben hier zur Nachvollziehbarkeit stehen und sind als `erledigt`
markiert.

| Nr. | Gap | Kompatibilität | Machbarkeit | Kombiniert | Kurzbegründung |
| --- | --- | --- | --- | --- | --- |
| 1 | Hintergrundausführung und Job-Control | `P0` | `M3` | `K2` | Sehr hoher Bash-Wert, aber im aktuellen Prozess-/Sandbox-Modell der teuerste Eingriff. |
| 2 | `[erledigt]` Trap-Randfälle und sessionbasiertes POSIX-Signalmodell | `erledigt` | `erledigt` | `-` | Am 2026-04-12 umgesetzt; verbleibende Stop/Continue-Reste hängen jetzt am Job-Control-Modell. |
| 3 | `[erledigt]` Spezialparameter (`$$`, `$!`, `$-`, `$_`) | `erledigt` | `erledigt` | `-` | Am 2026-04-12 umgesetzt. |
| 4 | `coproc` | `P1` | `M3` | `K3` | Echte Bash-4-Funktion, aber stark gekoppelt an parallele Prozess- und FD-Semantik. |
| 5 | `[erledigt]` Redirection-Lücken (`&>>`, `>|`, FD-close) | `erledigt` | `erledigt` | `-` | Am 2026-04-12 umgesetzt. |
| 6 | `[erledigt]` `select`-Semantik | `erledigt` | `erledigt` | `-` | Am 2026-04-12 umgesetzt. |
| 7 | `[erledigt]` fehlende nicht-interaktive Builtins | `erledigt` | `erledigt` | `-` | Der M0/M1-Block ohne echtes Job-Control wurde umgesetzt. |
| 8 | `[erledigt]` `read`-/`mapfile`-Flags | `erledigt` | `erledigt` | `-` | Am 2026-04-12 umgesetzt. |
| 9 | `[erledigt]` Flag-Lücken bei `declare`/`export`/`readonly`/`type`/`command` | `erledigt` | `erledigt` | `-` | Am 2026-04-12 umgesetzt. |
| 10 | `[erledigt]` zusätzliche Test-/`[[`-Operatoren | `erledigt` | `erledigt` | `-` | Am 2026-04-12 umgesetzt. |
| 11 | `set`-/`shopt`-Optionen unvollständig | `P2` | `M1` | `K2` | Der günstige nicht-interaktive Block ist umgesetzt; offen bleibt vor allem der breitere Rest an Modi und Editor-/Interactive-Optionen. |
| 12 | `[erledigt]` `time` und `times` | `erledigt` | `erledigt` | `-` | Am 2026-04-12 umgesetzt. |
| 13 | History-/Completion-/interaktive Builtins | `P3` | `M3` | `K3` | Geringer Fit zum Kernmodell von `wasmsh`. |
| 14 | Interaktive `shopt`-Features | `P3` | `M2` | `K3` | Ebenfalls niedriger ROI für die primären nicht-interaktiven Use Cases. |

## Kombinierte Bewertung nach Arbeitsblöcken

### K0

- kein verbleibender `K0`-Block; die früheren K0-Arbeiten sind umgesetzt

### K1

- kein verbleibender `K1`-Block; die früheren K1-Arbeiten sind umgesetzt oder in einen teureren Restblock übergegangen

### K2

- `1`: Job-Control und echte Hintergrundausführung
- `11`: verbleibende `set`-/`shopt`-Restmenge

### K3

- `4`: `coproc`
- `13`: History-/Completion-Builtins
- `14`: interaktive `shopt`-Features

## Empfohlene Arbeitsreihenfolge auf Basis der kombinierten Bewertung

1. Job-/Prozessmodell für `&`, `wait`, Jobspecs und die noch daran hängende Stop/Continue-Semantik angehen
2. die verbleibende `set`-/`shopt`-Restmenge opportunistisch als kleinere Kompatibilitätsarbeit schneiden
3. `coproc` erst auf Basis eines tragfähigen Prozess-/FD-Modells angehen
4. interaktive Bash-Features zuletzt behandeln
