"""Wasmsh sandbox implementation.

`WasmshSandbox` is a plain :class:`deepagents.backends.sandbox.BaseSandbox`
subclass. Everything except `execute`, `upload_files`, `download_files`, and
the two documented transport overrides (`edit`, `grep` — see
:mod:`langchain_wasmsh._file_ops`) runs upstream Deep Agents code unchanged.
"""

from __future__ import annotations

import json
import logging
import shlex
import shutil
import subprocess
import threading
from pathlib import Path
from typing import TYPE_CHECKING, Any
from uuid import uuid4

if TYPE_CHECKING:
    from collections.abc import Callable

from deepagents.backends.protocol import (
    EditResult,
    ExecuteResponse,
    FileDownloadResponse,
    FileUploadResponse,
    GrepResult,
)
from deepagents.backends.sandbox import BaseSandbox
from wasmsh_pyodide_runtime import get_dist_dir, get_node_host_script

from langchain_wasmsh._errors import extract_diagnostic, map_error
from langchain_wasmsh._file_ops import (
    TIMEOUT_EXIT_CODE,
    aroute_edit_via_upload,
    build_grep_cmd,
    decode_content,
    encode_content,
    parse_grep_output,
    route_edit_via_upload,
    timeout_response_output,
    to_initial_files,
)

logger = logging.getLogger(__name__)

DEFAULT_WORKSPACE_DIR = "/workspace"


class WasmshSessionTerminatedError(RuntimeError):
    """Raised when a call reaches a session that was destroyed by a timeout.

    `execute(timeout=N)` cannot interrupt an in-flight Pyodide evaluation:
    the interpreter runs synchronously inside the WebAssembly module and the
    host has no safe cancellation point. Rather than keep talking to an
    interpreter that is still executing the abandoned command, the sandbox
    kills the host process and refuses every later call, so a caller cannot
    silently read half-written state.
    """


class WasmshSandbox(BaseSandbox):
    """Wasmsh sandbox using Deno (preferred) or Node.js as host runtime."""

    def __init__(  # noqa: PLR0913
        self,
        *,
        runtime: str | None = None,
        dist_dir: str | Path | None = None,
        working_directory: str = DEFAULT_WORKSPACE_DIR,
        step_budget: int = 0,
        initial_files: dict[str, str | bytes] | None = None,
        allowed_hosts: list[str] | None = None,
    ) -> None:
        """Create a wasmsh sandbox backed by a Deno or Node.js subprocess.

        Prefers Deno for its permission model (defense-in-depth: the subprocess
        is restricted to reading only the asset directory and accessing only the
        specified network hosts). Falls back to Node.js if Deno is not installed.

        Args:
            runtime: Explicit runtime path ("deno" or "node"). Auto-detected
                if not specified: prefers Deno, falls back to Node.js.
            dist_dir: Path to Pyodide distribution assets. Auto-resolved from
                the wasmsh-pyodide-runtime package if not specified.
            working_directory: Working directory for execute(). Defaults to
                "/workspace".
            step_budget: VM step budget per command. 0 means unlimited.
            initial_files: Files to seed at creation. Keys are absolute paths,
                values are str or bytes content.
            allowed_hosts: Hostnames allowed for network access. Under Deno
                this maps to --allow-net; under Node.js it is enforced at the
                wasmsh application level only.
        """
        resolved = self._resolve_runtime(runtime)
        self._runtime = resolved
        # Resolve symlinks: Deno's --allow-read prefix-matches the canonical
        # path the filesystem returns, not the symlinked path we hand it.
        # Without this, a venv installed via a symlinked checkout (or any
        # site-packages reached through a symlink) hits a permission denial
        # the first time Pyodide's loader reads pyodide.asm.js.
        raw_dist = Path(dist_dir) if dist_dir is not None else Path(get_dist_dir())
        self._dist_dir = raw_dist.resolve()
        self._working_directory = working_directory
        self._allowed_hosts = allowed_hosts or []
        self._id = f"wasmsh-python-{uuid4()}"
        self._lock = threading.Lock()

        cmd = self._build_cmd()
        self._process = subprocess.Popen(  # noqa: S603
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self._next_request_id = 0
        self._capabilities: dict[str, str] = {}
        self._lock_owner: int | None = None  # thread id while _request runs
        self._dispatching = False  # True while a PTC host_call is being served
        self._stderr_lines: list[str] = []
        self._stderr_bytes = 0
        self._terminated_reason: str | None = None
        self._stderr_thread = threading.Thread(target=self._drain_stderr, daemon=True)
        self._stderr_thread.start()
        try:
            self._request(
                "init",
                {
                    "stepBudget": step_budget,
                    "initialFiles": to_initial_files(initial_files),
                    "allowedHosts": self._allowed_hosts,
                },
            )
        except Exception:
            logger.exception("wasmsh host init failed; terminating subprocess")
            self._kill_process()
            raise

    def _kill_process(self) -> None:
        """Forcibly terminate the host subprocess."""
        if self._process.stdin:
            self._process.stdin.close()
        self._process.terminate()
        try:
            self._process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self._process.kill()
            try:
                self._process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                logger.exception(
                    "wasmsh host process %d did not terminate after SIGKILL",
                    self._process.pid,
                )

    @staticmethod
    def _resolve_runtime(runtime: str | None) -> str:
        """Find Deno or Node.js on PATH, preferring Deno.

        Deno is preferred for its permission model: the subprocess is
        restricted to ``--allow-read=<assets>`` and ``--allow-net=<hosts>``.
        Falls back to Node.js if Deno is not installed.
        """
        if runtime is not None:
            path = shutil.which(runtime)
            if path is None:
                msg = f"Runtime not found: {runtime}"
                raise FileNotFoundError(msg)
            return path
        for name in ("deno", "node"):
            path = shutil.which(name)
            if path is not None:
                return path
        msg = "Neither deno nor node found on PATH"
        raise FileNotFoundError(msg)

    def _build_cmd(self) -> list[str]:
        # Resolve the host script through the same realpath the asset dir
        # already went through. Deno's reads of node-host.mjs (and any
        # `lib/*.mjs` it imports) match against the canonical path.
        host_script = str(Path(get_node_host_script()).resolve())
        asset_dir = str(self._dist_dir)
        if self._use_deno:
            cmd = [
                self._runtime,
                "run",
                f"--allow-read={asset_dir}",
                "--allow-env",
            ]
            if self._allowed_hosts:
                hosts = ",".join(self._allowed_hosts)
                cmd.append(f"--allow-net={hosts}")
            cmd.extend([host_script, "--asset-dir", asset_dir])
        else:
            cmd = [self._runtime, host_script, "--asset-dir", asset_dir]
            if self._allowed_hosts:
                logger.warning(
                    "allowed_hosts has no OS-level enforcement under "
                    "Node.js; install Deno for defense-in-depth",
                )
        return cmd

    @property
    def _use_deno(self) -> bool:
        return Path(self._runtime).name.startswith("deno")

    @property
    def id(self) -> str:
        """Return the sandbox identifier."""
        return self._id

    _MAX_STDERR_BYTES = 64 * 1024

    def _drain_stderr(self) -> None:
        """Continuously drain stderr to prevent pipe buffer deadlock."""
        if self._process.stderr is None:
            return
        for line in self._process.stderr:
            if self._stderr_bytes >= self._MAX_STDERR_BYTES:
                continue
            self._stderr_lines.append(line)
            self._stderr_bytes += len(line)

    def _stderr_text(self) -> str:
        return "".join(self._stderr_lines).strip()

    _MAX_NON_JSON_LINES = 100
    # Defensive cap: protects against a host that emits valid-JSON but
    # wrong-id responses forever (e.g. a hung worker, or a misbehaving
    # test mock). 100 is well above any plausible legitimate out-of-band
    # burst from `ack` / late `host_call_result` events, so a real spec-
    # compliant host will never trip it.
    _MAX_STALE_RESPONSES = 100

    def _request(
        self,
        method: str,
        params: dict[str, Any],
        *,
        on_host_call: Callable[[dict[str, Any]], dict[str, Any]] | None = None,
    ) -> dict[str, Any]:
        """Send one JSON-RPC request and read its response.

        Out-of-band messages (capability ack from boot, PTC ``host_call``
        events) are filtered from the response stream. When ``on_host_call``
        is provided, the dispatcher is invoked synchronously per host call
        and a ``host_call_result`` message is sent back inline.
        """
        if self._terminated_reason is not None:
            raise WasmshSessionTerminatedError(self._terminated_reason)
        if not self._process.stdin or not self._process.stdout:
            msg = "wasmsh host is not available"
            raise RuntimeError(msg)

        # Reentry guard: a PTC tool that calls back into the same sandbox
        # while we're mid-request would deadlock on _lock and corrupt the
        # JSON-RPC stream. Surface a clean error instead.
        #
        # `_dispatching` is checked as well as the owning thread id because a
        # tool does not necessarily run on the thread that is waiting for the
        # response — an async tool runs on the agent's event loop, and a sync
        # tool reached through `arun` runs on an executor thread. A thread-id
        # check alone would miss both and hang on `_lock`. One sandbox is
        # driven by one `_ThreadREPL`, which already serialises its own calls,
        # so treating *any* call made during a dispatch as reentrant does not
        # reject legitimate concurrent use.
        current_thread = threading.get_ident()
        if self._dispatching or self._lock_owner == current_thread:
            msg = (
                f"reentrant wasmsh sandbox call: {method!r} invoked from a "
                "PTC tool dispatch. PTC tools must not call back into the "
                "sandbox; wrap their side effects in a separate sandbox."
            )
            raise RuntimeError(msg)

        self._lock.acquire()
        try:
            self._lock_owner = current_thread
            self._next_request_id += 1
            request_id = self._next_request_id
            payload = {"id": request_id, "method": method, "params": params}
            try:
                self._process.stdin.write(json.dumps(payload) + "\n")
                self._process.stdin.flush()
            except OSError as exc:
                stderr = self._stderr_text()
                msg = f"Failed to send '{method}' to wasmsh host: {exc}"
                if stderr:
                    msg += f"\nHost stderr: {stderr}"
                raise RuntimeError(msg) from exc

            return self._read_response(request_id, method, on_host_call=on_host_call)
        finally:
            self._lock_owner = None
            self._lock.release()

    def _read_response(  # noqa: C901 -- multi-branch reader by design
        self,
        request_id: int,
        method: str,
        *,
        on_host_call: Callable[[dict[str, Any]], dict[str, Any]] | None = None,
    ) -> dict[str, Any]:
        skipped_non_json = 0
        skipped_stale = 0
        while True:
            line = self._process.stdout.readline() if self._process.stdout else ""
            if not line:
                stderr = self._stderr_text()
                msg = stderr or "wasmsh host terminated unexpectedly"
                raise RuntimeError(msg)
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                skipped_non_json += 1
                logger.debug("Skipping non-JSON host output: %s", line.rstrip())
                if skipped_non_json >= self._MAX_NON_JSON_LINES:
                    msg = (
                        f"wasmsh host emitted {skipped_non_json} consecutive "
                        f"non-JSON lines without a response"
                    )
                    raise RuntimeError(msg) from None
                continue

            kind = message.get("type") if isinstance(message, dict) else None
            if kind == "ack":
                caps = message.get("capabilities", {})
                if isinstance(caps, dict):
                    self._capabilities = caps
                continue
            if kind == "host_call":
                self._handle_host_call(message, on_host_call, method)
                continue
            if kind == "host_call_result":
                # Sandbox shouldn't be sending these to us, but tolerate.
                continue

            if not isinstance(message, dict) or "id" not in message:
                logger.debug("Ignoring host message without id: %s", message)
                continue

            if message["id"] != request_id:
                skipped_stale += 1
                logger.debug(
                    "Ignoring response for stale id=%s while waiting for %s",
                    message.get("id"),
                    request_id,
                )
                if skipped_stale >= self._MAX_STALE_RESPONSES:
                    msg = (
                        f"wasmsh host emitted {skipped_stale} responses with "
                        f"mismatched ids while waiting for id={request_id}; "
                        "assuming the host process is stuck"
                    )
                    raise RuntimeError(msg)
                continue

            if not message.get("ok"):
                raise RuntimeError(
                    str(message.get("error", "unknown wasmsh host error")),
                )
            return message["result"]

    def _handle_host_call(
        self,
        message: dict[str, Any],
        on_host_call: Callable[[dict[str, Any]], dict[str, Any]] | None,
        method: str,
    ) -> None:
        call_id = message.get("id")
        if not isinstance(call_id, str):
            logger.warning("Dropping host_call with missing id: %s", message)
            return
        if on_host_call is None:
            self._send_host_call_result(
                {
                    "id": call_id,
                    "ok": False,
                    "error": "PTCNotEnabled",
                    "message": (
                        f"host emitted host_call during {method}(...) but "
                        "no dispatcher was registered"
                    ),
                },
            )
            return
        self._dispatching = True
        try:
            envelope = on_host_call(message)
        except Exception as exc:  # noqa: BLE001 -- isolate one tool failure
            envelope = {
                "id": call_id,
                "ok": False,
                "error": type(exc).__name__,
                "message": str(exc),
            }
        finally:
            self._dispatching = False
        # Ensure the dispatcher's envelope carries the correlation id.
        envelope.setdefault("id", call_id)
        self._send_host_call_result(envelope)

    def _send_host_call_result(self, envelope: dict[str, Any]) -> None:
        envelope = {"type": "host_call_result", **envelope}
        stdin = self._process.stdin
        if stdin is None:
            msg = "wasmsh host stdin is closed"
            raise RuntimeError(msg)
        try:
            stdin.write(json.dumps(envelope) + "\n")
            stdin.flush()
        except OSError as exc:
            msg = f"Failed to send host_call_result: {exc}"
            raise RuntimeError(msg) from exc

    def host_capabilities(self) -> dict[str, str]:
        """Return capabilities the running host advertised on boot."""
        return dict(self._capabilities)

    def execute(self, command: str, *, timeout: int | None = None) -> ExecuteResponse:
        """Execute a shell command inside the sandbox.

        `timeout` is a real wall-clock deadline in seconds, enforced by the
        client. `None` (the default) and `0` mean "no deadline", matching
        `SandboxBackendProtocol.execute`.

        Pyodide runs the shell synchronously inside the WebAssembly module,
        and the host offers no safe way to interrupt an evaluation mid-flight.
        So the deadline is enforced the only way that keeps the session
        honest: the host process is killed, this sandbox is marked terminated
        (every later call raises :class:`WasmshSessionTerminatedError`), and
        the call returns an exit code of 124 — GNU `timeout(1)`'s convention.
        Use `step_budget` when you want a bound that leaves the session alive.
        """
        payload = {
            "command": f"cd {shlex.quote(self._working_directory)} && {command}",
        }
        if timeout is None or timeout <= 0:
            result = self._request("run", payload)
            return self._to_execute_response(result)

        # The host has no cancellation point, so the deadline is armed as a
        # watchdog that kills the process. Killing it is also what unblocks
        # the reader: `readline()` returns "" once stdout closes, which
        # `_read_response` reports as an unexpected termination.
        watchdog = threading.Timer(
            timeout,
            self._on_execute_timeout,
            (command, timeout),
        )
        watchdog.daemon = True
        watchdog.start()
        try:
            result = self._request("run", payload)
        except RuntimeError:
            if self._terminated_reason is None:
                raise
            return ExecuteResponse(
                output=timeout_response_output(command, timeout),
                exit_code=TIMEOUT_EXIT_CODE,
                truncated=False,
            )
        finally:
            watchdog.cancel()
        return self._to_execute_response(result)

    @staticmethod
    def _to_execute_response(result: dict[str, Any]) -> ExecuteResponse:
        return ExecuteResponse(
            output=str(result["output"]),
            exit_code=result.get("exitCode"),
            truncated=False,
        )

    def _on_execute_timeout(self, command: str, timeout: int) -> None:
        """Destroy the host process after a missed `execute` deadline."""
        if self._terminated_reason is not None:
            return
        self._terminated_reason = (
            f"wasmsh session was destroyed after `execute(timeout={timeout})` "
            f"expired while running: {command}"
        )
        logger.warning(
            "wasmsh execute exceeded timeout=%ss; terminating session %s",
            timeout,
            self._id,
        )
        self._kill_process()

    def edit(
        self,
        file_path: str,
        old_string: str,
        new_string: str,
        replace_all: bool = False,  # noqa: FBT001, FBT002 -- mirrors BackendProtocol
    ) -> EditResult:
        """Edit a file through upstream's temp-file route.

        Upstream's default (inline) route feeds its payload to `python3 -c`
        over a heredoc, and wasmsh's in-process `python3` never sees the
        shell's stdin. Forcing the temp-file route keeps the replacement
        algorithm, CRLF handling, and error strings upstream's — see
        :mod:`langchain_wasmsh._file_ops`.
        """
        return route_edit_via_upload(
            self,
            file_path,
            old_string,
            new_string,
            replace_all,
        )

    async def aedit(
        self,
        file_path: str,
        old_string: str,
        new_string: str,
        replace_all: bool = False,  # noqa: FBT001, FBT002 -- mirrors BackendProtocol
    ) -> EditResult:
        """Async version of :meth:`edit`."""
        return await aroute_edit_via_upload(
            self,
            file_path,
            old_string,
            new_string,
            replace_all,
        )

    def grep(
        self,
        pattern: str,
        path: str | None = None,
        glob: str | None = None,
        *,
        max_count: int | None = None,
    ) -> GrepResult:
        """Search file contents for a literal string.

        wasmsh's `grep` silently ignores `-Z`, so upstream's NUL-delimited
        record parser cannot read its output. This runs an in-sandbox Python
        search that emits exactly the records upstream expects; parsing,
        `max_count`, and `truncated` semantics stay upstream's. See
        :mod:`langchain_wasmsh._file_ops`.
        """
        result = self.execute(build_grep_cmd(pattern, path, glob, max_count))
        return parse_grep_output(result, path, max_count)

    async def agrep(
        self,
        pattern: str,
        path: str | None = None,
        glob: str | None = None,
        *,
        max_count: int | None = None,
    ) -> GrepResult:
        """Async version of :meth:`grep`."""
        result = await self.aexecute(build_grep_cmd(pattern, path, glob, max_count))
        return parse_grep_output(result, path, max_count)

    def run_ptc(
        self,
        code: str,
        *,
        tools: list[str],
        on_host_call: Callable[[dict[str, Any]], dict[str, Any]],
    ) -> dict[str, Any]:
        """Run ``code`` in the Pyodide REPL with PTC bridging.

        ``tools`` is the list of snake-case names the in-sandbox ``tools``
        namespace will expose to user code. ``on_host_call`` is invoked
        synchronously for each ``host_call`` event from the sandbox and must
        return a JSON-serialisable envelope::

            {"ok": True, "value": <native value>}
            {"ok": False, "error": "ToolError", "message": "...", "stack": "..."}

        The ``id`` is injected automatically.

        Returns the launcher's JSON envelope (``{"ok": ..., "stdout": ...,
        "stderr": ..., "value": ..., "error": ..., "message": ..., ...}``).
        """
        caps = self._capabilities.get("host_call")
        if caps is None:
            msg = (
                "wasmsh host did not advertise host_call capability; "
                "PTC is not supported by this runtime build"
            )
            raise RuntimeError(msg)
        result = self._request(
            "runPtc",
            {"code": code, "tools": list(tools)},
            on_host_call=on_host_call,
        )
        envelope = result.get("envelope")
        if not isinstance(envelope, dict):
            msg = "runPtc returned no envelope; host adapter is out of sync"
            raise RuntimeError(msg)  # noqa: TRY004 -- protocol misuse, not a type error
        return envelope

    def download_files(self, paths: list[str]) -> list[FileDownloadResponse]:
        """Download files from the sandbox.

        Checks for directories and unreadable files before attempting
        download, since Emscripten's VFS does not enforce permissions
        and reads directories as empty bytes.
        """
        responses: list[FileDownloadResponse] = []
        for path in paths:
            if not path.startswith("/"):
                responses.append(
                    FileDownloadResponse(path=path, content=None, error="invalid_path")
                )
                continue

            # Pre-check: detect directories since Emscripten's VFS reads
            # them as empty bytes instead of returning an error.
            try:
                check = self.execute(f"test -d {shlex.quote(path)} && echo DIR || true")
                if check.output.strip() == "DIR":
                    responses.append(
                        FileDownloadResponse(
                            path=path, content=None, error="is_directory"
                        )
                    )
                    continue
            except RuntimeError:
                logger.debug(
                    "Directory pre-check failed for %s; proceeding with download",
                    path,
                    exc_info=True,
                )

            try:
                result = self._request("readFile", {"path": path})
            except RuntimeError as exc:
                responses.append(
                    FileDownloadResponse(
                        path=path,
                        content=None,
                        error=map_error(str(exc)),
                    )
                )
                continue
            diagnostic = extract_diagnostic(result.get("events"))
            if diagnostic:
                responses.append(
                    FileDownloadResponse(
                        path=path,
                        content=None,
                        error=map_error(diagnostic),
                    )
                )
                continue
            responses.append(
                FileDownloadResponse(
                    path=path,
                    content=decode_content(str(result["contentBase64"])),
                    error=None,
                )
            )
        return responses

    def upload_files(self, files: list[tuple[str, bytes]]) -> list[FileUploadResponse]:
        """Upload files into the sandbox."""
        responses: list[FileUploadResponse] = []
        for path, content in files:
            if not path.startswith("/"):
                responses.append(FileUploadResponse(path=path, error="invalid_path"))
                continue
            try:
                result = self._request(
                    "writeFile",
                    {
                        "path": path,
                        "contentBase64": encode_content(content),
                    },
                )
            except RuntimeError as exc:
                responses.append(
                    FileUploadResponse(path=path, error=map_error(str(exc)))
                )
                continue
            diagnostic = extract_diagnostic(result.get("events"))
            responses.append(
                FileUploadResponse(
                    path=path,
                    error=map_error(diagnostic) if diagnostic else None,
                )
            )
        return responses

    def close(self) -> None:
        """Stop the host subprocess."""
        if self._process.poll() is not None:
            return
        try:
            self._request("close", {})
        except RuntimeError:
            logger.debug(
                "close request to node host failed (process will be terminated)",
                exc_info=True,
            )
        finally:
            self._kill_process()

    def stop(self) -> None:
        """Alias for `close()`."""
        self.close()
