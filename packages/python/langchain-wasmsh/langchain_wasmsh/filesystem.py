"""``WasmshFilesystemBackend`` — DeepAgents memory backend backed by a wasmsh VFS.

A thin shim that adapts a ``WasmshSandbox`` (or ``WasmshRemoteSandbox``) to
the DeepAgents :class:`~deepagents.backends.protocol.BackendProtocol`. Unlike
using the sandbox directly, this backend:

- never exposes ``execute()`` — it is a memory store, not a code-runner;
- supports a ``namespace`` prefix so several memory routes can share one
  sandbox without colliding (``/memories``, ``/skills``, …);
- is composable as a sub-backend in
  :class:`~deepagents.backends.composite.CompositeBackend`.

Use it on its own for laptop-scale persistent memory, or wire it behind a
``CompositeBackend`` route for production setups where memory lives in a
long-running dedicated wasmsh session.

Path traversal containment
~~~~~~~~~~~~~~~~~~~~~~~~~~

The ``namespace`` is a security boundary. The wasmsh sandbox VFS resolves
``..`` segments at the POSIX layer, so a naive ``f"{namespace}{path}"``
join would let any caller — including an LLM-driven agent that controls
``file_path`` on the standard ``read_file`` / ``write_file`` / ``edit_file``
tools — escape the namespace with payloads like ``../../skills/secret.py``.

``_scope`` resolves the joined path with :func:`posixpath.normpath` and
rejects any input that, after normalisation, leaves the namespace root.
``_unscope`` applies the matching containment check on inbound result
paths so an upstream bug elsewhere can't leak non-namespaced paths.
"""

from __future__ import annotations

import posixpath
from typing import TYPE_CHECKING

from deepagents.backends.protocol import (
    BackendProtocol,
    EditResult,
    FileDownloadResponse,
    FileUploadResponse,
    GlobResult,
    GrepResult,
    LsResult,
    ReadResult,
    WriteResult,
)


class WasmshNamespaceEscapeError(PermissionError):
    """Raised when a caller-supplied path would escape the configured namespace.

    Subclasses ``PermissionError`` so existing error-handlers that map OS
    permission errors to ``"permission_denied"`` continue to do the right
    thing without any additional catch.
    """

    def __init__(self, attempted_path: str, namespace: str) -> None:
        super().__init__(
            f"path {attempted_path!r} escapes namespace {namespace!r}",
        )

if TYPE_CHECKING:
    from deepagents.backends.sandbox import BaseSandbox

    # Both WasmshSandbox and WasmshRemoteSandbox inherit from BaseSandbox,
    # so the alias is just BaseSandbox — kept named for call-site clarity.
    SandboxLike = BaseSandbox


class WasmshFilesystemBackend(BackendProtocol):
    """Memory backend that routes file ops to a wasmsh VFS.

    Args:
        sandbox: A live ``WasmshSandbox`` / ``WasmshRemoteSandbox`` instance,
            or any object implementing the deepagents ``BaseSandbox`` file
            surface. The backend does **not** take ownership: callers are
            responsible for closing the sandbox.
        namespace: Optional absolute-path prefix (e.g. ``"/memories"``) that
            is silently prepended to every path the agent uses. Lets one
            sandbox host multiple memory routes without collisions.

    Example:
        ```python
        from deepagents.backends import CompositeBackend, StateBackend
        from langchain_wasmsh import WasmshFilesystemBackend, WasmshSandbox

        sandbox = WasmshSandbox()
        backend = CompositeBackend(
            default=StateBackend(),
            routes={
                "/memories/": WasmshFilesystemBackend(sandbox, namespace="/memories"),
            },
        )
        ```
    """

    def __init__(
        self,
        sandbox: SandboxLike,
        *,
        namespace: str = "",
    ) -> None:
        """Wrap ``sandbox`` as a memory backend; see class docstring for args."""
        self._sandbox = sandbox
        self._namespace = self._normalise_namespace(namespace)

    # ---- namespace mapping ----------------------------------------------

    @staticmethod
    def _normalise_namespace(namespace: str) -> str:
        if not namespace:
            return ""
        if not namespace.startswith("/"):
            namespace = "/" + namespace
        return namespace.rstrip("/")

    def _scope(self, path: str | None) -> str | None:
        if path is None:
            return None
        if not self._namespace:
            return path
        if not path.startswith("/"):
            path = "/" + path
        if path == "/":
            return self._namespace or "/"
        # ``posixpath.normpath`` collapses ``.`` and ``..`` segments. The
        # subsequent containment check rejects any payload that, after
        # normalisation, leaves the namespace root — including spellings
        # that bypass a naive ``"../" in path`` substring guard.
        joined = f"{self._namespace}{path}"
        resolved = posixpath.normpath(joined)
        if not self._is_contained(resolved):
            raise WasmshNamespaceEscapeError(path, self._namespace)
        return resolved

    def _unscope(self, path: str) -> str:
        if not self._namespace:
            return path
        # Containment check on the way back: an upstream bug (or a
        # misbehaving sandbox) should never leak paths from outside the
        # namespace into the caller's view.
        if not self._is_contained(path):
            raise WasmshNamespaceEscapeError(path, self._namespace)
        stripped = path[len(self._namespace) :]
        return stripped or "/"

    def _is_contained(self, resolved: str) -> bool:
        """``True`` iff ``resolved`` sits at the namespace root or below.

        A plain ``startswith(self._namespace)`` would accept a sibling
        directory whose name shares the prefix (e.g. ``/memstore`` vs.
        ``/mem``); we anchor with the trailing separator explicitly.
        """
        if resolved == self._namespace:
            return True
        return resolved.startswith(self._namespace + "/")

    # ---- BackendProtocol surface ----------------------------------------

    def ls(self, path: str) -> LsResult:
        """Delegate ``ls`` to the wrapped sandbox, unscoping result paths."""
        result = self._sandbox.ls(self._scope(path) or "/")
        if result.error or not result.entries:
            return result
        return LsResult(
            entries=[
                {**entry, "path": self._unscope(entry["path"])}
                for entry in result.entries
            ],
        )

    def read(
        self,
        file_path: str,
        offset: int = 0,
        limit: int = 2000,
    ) -> ReadResult:
        """Read a file inside the scoped namespace."""
        scoped = self._scope(file_path) or file_path
        return self._sandbox.read(scoped, offset=offset, limit=limit)

    def grep(
        self,
        pattern: str,
        path: str | None = None,
        glob: str | None = None,
    ) -> GrepResult:
        """Grep within the scoped namespace, unscoping result paths."""
        result = self._sandbox.grep(pattern, self._scope(path), glob)
        if result.error or not result.matches:
            return result
        return GrepResult(
            matches=[
                {**m, "path": self._unscope(m["path"])} for m in result.matches
            ],
        )

    def glob(self, pattern: str, path: str = "/") -> GlobResult:
        """Glob within the scoped namespace, unscoping result paths."""
        scoped = self._scope(path) or "/"
        result = self._sandbox.glob(pattern, scoped)
        if result.error or not result.matches:
            return result
        return GlobResult(
            matches=[
                {**m, "path": self._unscope(m["path"])} for m in result.matches
            ],
        )

    def write(self, file_path: str, content: str) -> WriteResult:
        """Write a new file inside the scoped namespace."""
        scoped = self._scope(file_path) or file_path
        return self._sandbox.write(scoped, content)

    def edit(
        self,
        file_path: str,
        old_string: str,
        new_string: str,
        replace_all: bool = False,  # noqa: FBT001, FBT002 -- mirrors BackendProtocol
    ) -> EditResult:
        """Edit a file inside the scoped namespace."""
        scoped = self._scope(file_path) or file_path
        return self._sandbox.edit(scoped, old_string, new_string, replace_all)

    def upload_files(
        self,
        files: list[tuple[str, bytes]],
    ) -> list[FileUploadResponse]:
        """Upload many files at once into the scoped namespace."""
        scoped = [(self._scope(p) or p, content) for p, content in files]
        responses = self._sandbox.upload_files(scoped)
        return [self._unscope_upload(resp) for resp in responses]

    def download_files(self, paths: list[str]) -> list[FileDownloadResponse]:
        """Download many files at once from the scoped namespace."""
        scoped = [self._scope(p) or p for p in paths]
        responses = self._sandbox.download_files(scoped)
        return [self._unscope_download(resp) for resp in responses]

    # ---- helpers --------------------------------------------------------

    def _unscope_upload(self, response: FileUploadResponse) -> FileUploadResponse:
        if not self._namespace:
            return response
        return FileUploadResponse(
            path=self._unscope(response.path),
            error=response.error,
        )

    def _unscope_download(
        self,
        response: FileDownloadResponse,
    ) -> FileDownloadResponse:
        if not self._namespace:
            return response
        return FileDownloadResponse(
            path=self._unscope(response.path),
            content=response.content,
            error=response.error,
        )


__all__ = ["WasmshFilesystemBackend"]
