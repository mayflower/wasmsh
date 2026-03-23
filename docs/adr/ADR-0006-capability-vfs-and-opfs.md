# ADR-0006: Capability-basiertes VFS mit OPFS-Backend

## Status
Angenommen

## Kontext
Im Browser gibt es kein normales POSIX-Dateisystem. Gleichzeitig braucht wasmsh ein virtuelles, sandbox-fähiges FS.

## Entscheidung
wasmsh verwendet ein eigenes VFS-Interface mit `MemoryFs`, `OverlayFs` und optional `OpfsFs`.

## Konsequenzen
- deterministisches FS-Verhalten
- isolierbare Sessions
- gute Grundlage für Sandboxing und Snapshots
