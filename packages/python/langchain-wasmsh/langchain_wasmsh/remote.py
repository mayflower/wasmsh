"""Dispatcher-backed remote sandbox.

`WasmshRemoteSandbox` implements the same `BaseSandbox` subset as the
in-process `WasmshSandbox` but routes every operation through the
wasmsh **dispatcher** HTTP service (see `docs/reference/dispatcher-api.md`
and `deploy/helm/wasmsh/`).  This is the backend to use when you want
Kubernetes-scale concurrency or want agent sessions to outlive the
client process.

The transport is plain JSON/HTTP to the dispatcher; all binary payloads
travel base64-encoded over the wire (the dispatcher's stable contract).
File-operation semantics come from `BaseSandbox` itself, so they are
identical to the in-process backend by construction — including the two
transport overrides (`edit`, `grep`) both share via
:mod:`langchain_wasmsh._file_ops`.
"""

from __future__ import annotations

import logging
import shlex
from typing import TYPE_CHECKING, Any, Self
from uuid import uuid4

import httpx
from deepagents.backends.protocol import (
    EditResult,
    ExecuteResponse,
    FileDownloadResponse,
    FileUploadResponse,
    GrepResult,
)
from deepagents.backends.sandbox import BaseSandbox

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

if TYPE_CHECKING:
    from types import TracebackType

logger = logging.getLogger(__name__)

DEFAULT_WORKSPACE_DIR = "/workspace"
_DEFAULT_TIMEOUT_SECONDS = 30.0

_EXECUTE_TIMEOUT_GRACE_SECONDS = 10.0
"""Extra socket budget on top of a per-command `execute(timeout=N)` deadline.

The runner enforces the deadline itself and answers with a timeout result;
the client socket must outlive that exchange, otherwise it would abort the
request before the authoritative answer arrives and the caller could not
tell a timed-out command from a dead dispatcher.
"""


class WasmshRemoteSandbox(BaseSandbox):
    """Wasmsh sandbox backed by a remote dispatcher + runner pool.

    Use this backend in production / Kubernetes deployments.  For local
    single-process usage prefer `WasmshSandbox`, which boots Pyodide
    in-process via a Deno or Node.js subprocess.

    The dispatcher HTTP API is documented in
    `docs/reference/dispatcher-api.md`; the Helm chart in
    `deploy/helm/wasmsh/` provisions the control plane.
    """

    def __init__(  # noqa: PLR0913
        self,
        dispatcher_url: str,
        *,
        session_id: str | None = None,
        allowed_hosts: list[str] | None = None,
        step_budget: int = 0,
        initial_files: dict[str, str | bytes] | None = None,
        working_directory: str = DEFAULT_WORKSPACE_DIR,
        timeout: float = _DEFAULT_TIMEOUT_SECONDS,
        headers: dict[str, str] | None = None,
        http_client: httpx.Client | None = None,
    ) -> None:
        """Create a remote sandbox bound to a dispatcher session.

        Args:
            dispatcher_url: Base URL of the wasmsh dispatcher
                (e.g. ``http://wasmsh-dispatcher.wasmsh.svc.cluster.local:8080``).
            session_id: Reuse an existing dispatcher session instead of
                creating a new one. When `None`, a fresh client-generated id
                is sent so callers can correlate logs across client + server.
            allowed_hosts: Hostnames the sandbox may reach via `curl`/`wget`.
                Forwarded to the runner's capability model.
            step_budget: Per-execution VM step budget. 0 means unlimited.
            initial_files: Files to seed at session creation. Keys are
                absolute paths; values are str (utf-8) or raw bytes.
            working_directory: Working directory prepended to every
                `execute()` command. Defaults to ``/workspace``.
            timeout: Per-request HTTP timeout in seconds. Tune upwards for
                long-running commands.
            headers: Extra HTTP headers forwarded with every request.
                When the dispatcher is configured with
                ``WASMSH_AUTH_TOKEN``, pass
                ``headers={"Authorization": f"Bearer {token}"}`` here.
            http_client: Inject a pre-configured `httpx.Client` for tests
                or custom transports. When omitted the sandbox owns a
                client it will close on `close()`.
        """
        self._base_url = dispatcher_url.rstrip("/")
        self._working_directory = working_directory
        self._session_id = session_id or f"wasmsh-python-{uuid4()}"
        self._owns_client = http_client is None
        self._client = http_client or httpx.Client(
            timeout=timeout,
            headers=headers,
        )
        self._closed = False

        payload = {
            "session_id": self._session_id,
            "allowed_hosts": allowed_hosts or [],
            "step_budget": step_budget,
            "initial_files": to_initial_files(initial_files),
        }
        try:
            response = self._post("/sessions", payload)
        except Exception:
            # Session creation failed — tear down the client we own so the
            # socket doesn't leak.  If the caller supplied a client, leave
            # it alone; they own its lifetime.
            if self._owns_client:
                self._client.close()
            raise

        # The runner echoes the authoritative session id in its response
        # envelope; prefer it over our local guess in case the dispatcher
        # chose to mint its own.
        session = response.get("session") or {}
        reported_id = session.get("sessionId")
        if isinstance(reported_id, str) and reported_id:
            self._session_id = reported_id

    @property
    def id(self) -> str:
        """Return the dispatcher session id."""
        return self._session_id

    # ── HTTP plumbing ──────────────────────────────────────────────────

    def _url(self, path: str) -> str:
        return f"{self._base_url}{path}"

    def _post(
        self,
        path: str,
        payload: dict[str, Any],
        *,
        request_timeout: float | None = None,
    ) -> dict[str, Any]:
        """POST `payload` as JSON to `path`; return parsed dispatcher response.

        `request_timeout` overrides the client-wide socket timeout for this
        one call, which `execute()` needs so a long command deadline is not
        clipped by the default. Raises `RuntimeError` with the
        dispatcher-supplied error message on non-2xx; callers may inspect
        `args[0]` for classification.
        """
        kwargs: dict[str, Any] = {"json": payload}
        if request_timeout is not None:
            kwargs["timeout"] = request_timeout
        response = self._client.post(self._url(path), **kwargs)
        return self._parse_response(response, path)

    def _delete(self, path: str) -> dict[str, Any]:
        response = self._client.delete(self._url(path))
        return self._parse_response(response, path)

    def _parse_response(
        self,
        response: httpx.Response,
        path: str,
    ) -> dict[str, Any]:
        try:
            body = response.json()
        except ValueError as exc:
            msg = (
                f"dispatcher {path} returned non-JSON body "
                f"(status {response.status_code}): {response.text[:200]}"
            )
            raise RuntimeError(msg) from exc
        if response.is_success and body.get("ok", True):
            return body
        fallback = f"dispatcher error (status {response.status_code})"
        error = str(body.get("error", fallback))
        msg = f"dispatcher {path}: {error}"
        raise RuntimeError(msg)

    # ── BaseSandbox overrides ──────────────────────────────────────────

    def execute(self, command: str, *, timeout: int | None = None) -> ExecuteResponse:
        """Execute a shell command inside the remote sandbox.

        `timeout` is a real wall-clock deadline in seconds. It is sent to the
        dispatcher as `timeout_ms` and enforced by the *runner*, which owns
        the Pyodide worker and can terminate it; the client additionally
        widens its own socket deadline past the command deadline so the
        runner's authoritative timeout answer is not cut off in transit.
        `None` and `0` mean "no deadline", matching
        `SandboxBackendProtocol.execute`.

        On expiry the runner destroys the worker for that session and reports
        exit code 124 (GNU `timeout(1)`'s convention). The session is then
        unusable — an in-flight Pyodide evaluation cannot be interrupted
        safely, so it is not resumed.
        """
        payload: dict[str, Any] = {
            "command": f"cd {shlex.quote(self._working_directory)} && {command}",
        }
        request_timeout: float | None = None
        if timeout is not None and timeout > 0:
            payload["timeout_ms"] = int(timeout) * 1000
            request_timeout = float(timeout) + _EXECUTE_TIMEOUT_GRACE_SECONDS
        try:
            body = self._post(
                f"/sessions/{self._session_id}/run",
                payload,
                request_timeout=request_timeout,
            )
        except httpx.TimeoutException:
            if timeout is None:
                raise
            logger.warning(
                "dispatcher did not answer within %ss for a command with "
                "timeout=%ss; reporting a client-side timeout",
                request_timeout,
                timeout,
            )
            return ExecuteResponse(
                output=timeout_response_output(command, timeout),
                exit_code=TIMEOUT_EXIT_CODE,
                truncated=False,
            )
        result = body.get("result") or {}
        if result.get("timedOut"):
            runner_output = result.get("output") or timeout_response_output(
                command,
                int(timeout or 0),
            )
            return ExecuteResponse(
                output=str(runner_output),
                exit_code=result.get("exitCode", TIMEOUT_EXIT_CODE),
                truncated=False,
            )
        return ExecuteResponse(
            output=str(result.get("output", "")),
            exit_code=result.get("exitCode"),
            truncated=False,
        )

    def edit(
        self,
        file_path: str,
        old_string: str,
        new_string: str,
        replace_all: bool = False,  # noqa: FBT001, FBT002 -- mirrors BackendProtocol
    ) -> EditResult:
        """Edit a file through upstream's temp-file route.

        Identical to `WasmshSandbox.edit`: the runner executes the same
        wasmsh shell, so upstream's heredoc-fed inline route fails the same
        way. See :mod:`langchain_wasmsh._file_ops`.
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

        Same transport override as `WasmshSandbox.grep`; see
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
        on_host_call: Any,  # noqa: ARG002 -- Phase 2 implementation
    ) -> dict[str, Any]:
        """Run ``code`` with PTC bridging via the dispatcher.

        Not yet implemented for the remote path — Phase 2 of the
        ``ptc_suspend_resume.md`` spec adds an SSE response stream and a
        ``POST /sessions/<id>/host_result`` companion endpoint. Today the
        dispatcher only supports the single-shot ``run`` round-trip.
        """
        del code, tools
        msg = (
            "WasmshRemoteSandbox.run_ptc is not yet implemented; "
            "the dispatcher SSE channel (Phase 2 of ptc_suspend_resume.md) "
            "must land first"
        )
        raise NotImplementedError(msg)

    def download_files(self, paths: list[str]) -> list[FileDownloadResponse]:
        """Download files from the remote sandbox.

        Performs an `execute("test -d …")` pre-check for each path because
        the underlying Emscripten VFS reads directories as empty bytes
        instead of returning an error — identical behavior to the
        in-process backend.
        """
        responses: list[FileDownloadResponse] = []
        for path in paths:
            if not path.startswith("/"):
                responses.append(
                    FileDownloadResponse(path=path, content=None, error="invalid_path"),
                )
                continue

            try:
                check = self.execute(f"test -d {shlex.quote(path)} && echo DIR || true")
                if check.output.strip() == "DIR":
                    responses.append(
                        FileDownloadResponse(
                            path=path,
                            content=None,
                            error="is_directory",
                        ),
                    )
                    continue
            except RuntimeError:
                logger.debug(
                    "Directory pre-check failed for %s; proceeding with download",
                    path,
                    exc_info=True,
                )

            try:
                body = self._post(
                    f"/sessions/{self._session_id}/read-file",
                    {"path": path},
                )
            except RuntimeError as exc:
                responses.append(
                    FileDownloadResponse(
                        path=path,
                        content=None,
                        error=map_error(str(exc)),
                    ),
                )
                continue

            result = body.get("result") or {}
            diagnostic = extract_diagnostic(result.get("events"))
            if diagnostic:
                responses.append(
                    FileDownloadResponse(
                        path=path,
                        content=None,
                        error=map_error(diagnostic),
                    ),
                )
                continue
            content_b64 = result.get("contentBase64")
            if not isinstance(content_b64, str):
                responses.append(
                    FileDownloadResponse(
                        path=path,
                        content=None,
                        error="invalid_path",
                    ),
                )
                continue
            responses.append(
                FileDownloadResponse(
                    path=path,
                    content=decode_content(content_b64),
                    error=None,
                ),
            )
        return responses

    def upload_files(
        self,
        files: list[tuple[str, bytes]],
    ) -> list[FileUploadResponse]:
        """Upload files into the remote sandbox."""
        responses: list[FileUploadResponse] = []
        for path, content in files:
            if not path.startswith("/"):
                responses.append(FileUploadResponse(path=path, error="invalid_path"))
                continue
            try:
                body = self._post(
                    f"/sessions/{self._session_id}/write-file",
                    {"path": path, "contentBase64": encode_content(content)},
                )
            except RuntimeError as exc:
                responses.append(
                    FileUploadResponse(path=path, error=map_error(str(exc))),
                )
                continue
            result = body.get("result") or {}
            diagnostic = extract_diagnostic(result.get("events"))
            responses.append(
                FileUploadResponse(
                    path=path,
                    error=map_error(diagnostic) if diagnostic else None,
                ),
            )
        return responses

    # ── Lifecycle ──────────────────────────────────────────────────────

    def close(self) -> None:
        """Close the dispatcher session and release runner affinity.

        Best-effort: network errors during shutdown are logged but do not
        propagate, because callers (test fixtures, agent lifecycles) rely
        on `close()` to be safe even when the dispatcher is unreachable.
        """
        if self._closed:
            return
        self._closed = True
        for path in (
            f"/sessions/{self._session_id}/close",
            f"/sessions/{self._session_id}",
        ):
            try:
                if path.endswith(self._session_id):
                    self._delete(path)
                else:
                    self._post(path, {})
            except (httpx.HTTPError, RuntimeError):
                logger.debug(
                    "dispatcher %s failed during close (ignored)",
                    path,
                    exc_info=True,
                )
        if self._owns_client:
            self._client.close()

    def stop(self) -> None:
        """Alias for `close()`."""
        self.close()

    def __enter__(self) -> Self:
        """Return self so the sandbox can be used as a context manager."""
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc_value: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        """Close the dispatcher session on context-manager exit."""
        self.close()
