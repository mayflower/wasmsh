"""Dispatcher-backed remote sandbox.

`WasmshRemoteSandbox` implements the same `BaseSandbox` subset as the
in-process `WasmshSandbox` but routes every operation through the
wasmsh **dispatcher** HTTP service (see `docs/reference/dispatcher-api.md`
and `deploy/helm/wasmsh/`).  This is the backend to use when you want
Kubernetes-scale concurrency or want agent sessions to outlive the
client process.

The transport is plain JSON/HTTP to the dispatcher; all binary payloads
travel base64-encoded over the wire (the dispatcher's stable
contract).  Client-side file-operation semantics (error mapping, text
pagination, edit semantics) are identical to the in-process backend.
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
    FileData,
    FileDownloadResponse,
    FileUploadResponse,
    ReadResult,
)
from deepagents.backends.sandbox import BaseSandbox

from langchain_wasmsh._errors import extract_diagnostic, map_error
from langchain_wasmsh._text import (
    MAX_BINARY_PREVIEW_BYTES,
    decode_content,
    encode_content,
    paginate_text,
    to_initial_files,
)

if TYPE_CHECKING:
    from types import TracebackType

logger = logging.getLogger(__name__)

DEFAULT_WORKSPACE_DIR = "/workspace"
_DEFAULT_TIMEOUT_SECONDS = 30.0


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
                Intended as a future hook for auth (Bearer tokens, etc.)
                once the dispatcher grows an auth layer.
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

    def _post(self, path: str, payload: dict[str, Any]) -> dict[str, Any]:
        """POST `payload` as JSON to `path`; return parsed dispatcher response.

        Raises `RuntimeError` with the dispatcher-supplied error message on
        non-2xx; callers may inspect `args[0]` for classification.
        """
        response = self._client.post(self._url(path), json=payload)
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

        `timeout` is accepted for protocol compatibility but is enforced
        by the dispatcher / runner, not by the client.
        """
        if timeout is not None:
            logger.debug(
                "WasmshRemoteSandbox.execute timeout=%s is advisory; "
                "dispatcher/runner enforces session budget",
                timeout,
            )
        body = self._post(
            f"/sessions/{self._session_id}/run",
            {"command": f"cd {shlex.quote(self._working_directory)} && {command}"},
        )
        result = body.get("result") or {}
        return ExecuteResponse(
            output=str(result.get("output", "")),
            exit_code=result.get("exitCode"),
            truncated=False,
        )

    def read(
        self,
        file_path: str,
        offset: int = 0,
        limit: int = 2000,
    ) -> ReadResult:
        """Read a file, returning text with offset/limit or base64 binary.

        Mirrors `WasmshSandbox.read` byte-for-byte so both backends pass
        the `langchain-tests` sandbox standard suite.
        """
        responses = self.download_files([file_path])
        resp = responses[0]
        if resp.error or resp.content is None:
            detail = resp.error or "file not found"
            return ReadResult(error=f"File '{file_path}': {detail}")

        raw = resp.content
        if not raw:
            return ReadResult(
                file_data=FileData(
                    content="System reminder: File exists but has empty contents",
                    encoding="utf-8",
                ),
            )

        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError:
            if len(raw) > MAX_BINARY_PREVIEW_BYTES:
                return ReadResult(
                    error=(
                        f"File '{file_path}': Binary file exceeds maximum "
                        f"preview size of {MAX_BINARY_PREVIEW_BYTES} bytes"
                    ),
                )
            return ReadResult(
                file_data=FileData(
                    content=encode_content(raw),
                    encoding="base64",
                ),
            )

        page = paginate_text(text, offset=int(offset), limit=int(limit))
        return ReadResult(file_data=FileData(content=page, encoding="utf-8"))

    def edit(  # noqa: C901, PLR0911
        self,
        file_path: str,
        old_string: str,
        new_string: str,
        replace_all: bool = False,  # noqa: FBT001, FBT002
    ) -> EditResult:
        """Edit a file via download + string replace + upload.

        Mirrors `WasmshSandbox.edit` so error strings stay compatible with
        the standard suite.
        """
        responses = self.download_files([file_path])
        if responses[0].error or responses[0].content is None:
            detail = responses[0].error or "file_not_found"
            return EditResult(error=f"File '{file_path}': {detail}")

        text = responses[0].content.decode("utf-8", errors="replace")

        if not old_string:
            if text:
                return EditResult(
                    error="oldString must not be empty unless file is empty",
                )
            if not new_string:
                return EditResult(path=file_path, occurrences=0)
            data = new_string.encode("utf-8")
            upload = self.upload_files([(file_path, data)])
            if upload[0].error:
                return EditResult(
                    error=f"Failed to write '{file_path}': {upload[0].error}",
                )
            return EditResult(path=file_path, occurrences=1)

        idx = text.find(old_string)
        if idx == -1:
            return EditResult(error=f"String not found in file '{file_path}'")

        if old_string == new_string:
            return EditResult(path=file_path, occurrences=1)

        if replace_all:
            count = text.count(old_string)
            new_text = text.replace(old_string, new_string)
        else:
            second = text.find(old_string, idx + len(old_string))
            if second != -1:
                return EditResult(
                    error=(
                        f"Multiple occurrences found in '{file_path}'. "
                        "Use replace_all=True to replace all."
                    ),
                )
            count = 1
            new_text = text[:idx] + new_string + text[idx + len(old_string) :]

        data = new_text.encode("utf-8")
        upload = self.upload_files([(file_path, data)])
        if upload[0].error:
            return EditResult(
                error=f"Failed to write '{file_path}': {upload[0].error}",
            )
        return EditResult(path=file_path, occurrences=count)

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
