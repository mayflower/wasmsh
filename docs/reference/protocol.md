# Worker Protocol Reference

Communication protocol between the host application and the wasmsh Web Worker runtime.

## Protocol Version

Current: `0.1.0`

## Host → Worker Commands

### `Init`

Initialize the shell runtime. Must be sent before any other command.

| Field | Type | Description |
|-------|------|-------------|
| `step_budget` | `u64` | Maximum VM steps per execution (0 = unlimited) |

**Response**: `Version("0.1.0")`

### `Run`

Execute a shell command string.

| Field | Type | Description |
|-------|------|-------------|
| `input` | `String` | Shell source code to execute |

**Response**: Zero or more `Stdout`/`Stderr` events, followed by `Exit(code)`.

### `Cancel`

Abort the currently running execution.

**Response**: `Diagnostic(Info, "cancel received")`

### `ReadFile`

Read a file from the virtual filesystem.

| Field | Type | Description |
|-------|------|-------------|
| `path` | `String` | Absolute VFS path |

**Response**: `Stdout(data)` with file contents, or `Diagnostic(Error, ...)`.

### `WriteFile`

Write data to a file in the virtual filesystem.

| Field | Type | Description |
|-------|------|-------------|
| `path` | `String` | Absolute VFS path |
| `data` | `Vec<u8>` | File contents |

**Response**: `FsChanged(path)` on success.

### `ListDir`

List directory contents.

| Field | Type | Description |
|-------|------|-------------|
| `path` | `String` | Absolute VFS directory path |

**Response**: `Stdout(data)` with newline-separated filenames.

## Worker → Host Events

| Event | Fields | Description |
|-------|--------|-------------|
| `Stdout(Vec<u8>)` | bytes | Shell command stdout output |
| `Stderr(Vec<u8>)` | bytes | Shell command stderr output |
| `Exit(i32)` | code | Command execution finished |
| `Diagnostic(level, msg)` | level + message | Runtime diagnostic |
| `FsChanged(String)` | path | A VFS file was modified |
| `Version(String)` | version | Protocol version (sent on Init) |

### Diagnostic Levels

- `Trace` — detailed execution tracing
- `Info` — informational messages
- `Warning` — non-fatal issues (e.g., output limit approaching)
- `Error` — execution errors
