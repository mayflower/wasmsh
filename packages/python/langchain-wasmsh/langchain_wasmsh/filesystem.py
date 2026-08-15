"""``WasmshFilesystemBackend`` — a namespace/path adapter over a wasmsh VFS.

Adapts a ``WasmshSandbox`` (or ``WasmshRemoteSandbox``) to the Deep Agents
:class:`~deepagents.backends.protocol.BackendProtocol` so one sandbox can
serve several routes of a
:class:`~deepagents.backends.composite.CompositeBackend` without collisions.
It is deliberately thin: every operation is forwarded to the sandbox, and
the only work done here is rewriting paths in and out of the namespace.

What this backend is not
~~~~~~~~~~~~~~~~~~~~~~~~

**Not durable memory.** A local wasmsh VFS lives inside the host subprocess
and disappears when that process exits; a remote session's files last only
as long as the dispatcher keeps that session. Neither is a cross-process
store. For durable user, agent, and organization memory — profiles, skills,
policies — route those prefixes to
:class:`~deepagents.backends.store.StoreBackend` over a real LangGraph
``BaseStore`` instead, and keep the wasmsh backend for the executable
workspace and transient artifacts.

**Not a tenant-isolation boundary.** ``namespace`` is enforced lexically:
:func:`posixpath.normpath` collapses ``.``/``..`` and the result must still
sit under the namespace root. That stops an agent-controlled ``file_path``
like ``../../skills/secret.py`` on the ordinary ``read_file`` /
``write_file`` / ``edit_file`` tools, which is what it exists for. It does
**not** survive symlinks: the sandbox resolves them at the POSIX layer, so
anything with shell access (``execute``, the Python interpreter, a custom
tool) can link out of the namespace and read through it. When principals do
not trust each other, give each one its own sandbox session, or put the
sensitive data in a non-executable store namespace.

Example:
    ```python
    from deepagents.backends import CompositeBackend, StateBackend
    from langchain_wasmsh import WasmshFilesystemBackend, WasmshSandbox

    sandbox = WasmshSandbox()
    backend = CompositeBackend(
        default=StateBackend(),
        routes={
            "/scratch/": WasmshFilesystemBackend(sandbox, namespace="/scratch"),
        },
    )
    ```
"""

from __future__ import annotations

import posixpath
from typing import TYPE_CHECKING

from deepagents.backends.protocol import (
    BackendProtocol,
    DeleteResult,
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
        """Record the rejected path and the namespace it tried to leave."""
        super().__init__(
            f"path {attempted_path!r} escapes namespace {namespace!r}",
        )


if TYPE_CHECKING:
    from deepagents.backends.sandbox import BaseSandbox

    # Both WasmshSandbox and WasmshRemoteSandbox inherit from BaseSandbox,
    # so the alias is just BaseSandbox — kept named for call-site clarity.
    SandboxLike = BaseSandbox


class WasmshFilesystemBackend(BackendProtocol):
    """Route Deep Agents file operations into a namespaced wasmsh VFS path.

    Args:
        sandbox: A live ``WasmshSandbox`` / ``WasmshRemoteSandbox`` instance,
            or any object implementing the deepagents ``BaseSandbox`` file
            surface. The backend does **not** take ownership: callers are
            responsible for closing the sandbox.
        namespace: Optional absolute-path prefix (e.g. ``"/scratch"``) that
            is silently prepended to every path the agent uses. Read the
            module docstring before treating it as an isolation boundary —
            it is a routing prefix, not a sandbox.
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

    def _unscope_optional(self, path: str | None) -> str | None:
        """Unscope a result path that a failed operation may leave unset."""
        return None if path is None else self._unscope(path)

    def _is_contained(self, resolved: str) -> bool:
        """``True`` iff ``resolved`` sits at the namespace root or below.

        A plain ``startswith(self._namespace)`` would accept a sibling
        directory whose name shares the prefix (e.g. ``/memstore`` vs.
        ``/mem``); we anchor with the trailing separator explicitly.
        """
        if resolved == self._namespace:
            return True
        return resolved.startswith(self._namespace + "/")

    # ---- result mapping --------------------------------------------------
    #
    # Errors are forwarded verbatim. Their text embeds the scoped path, which
    # is the path the sandbox actually operated on, and rewriting it risks
    # changing what the message means (an error can name a path other than
    # the one the caller passed). Only the `path` field of a *successful*
    # result is translated back into the caller's namespace-relative view.

    def _map_ls(self, result: LsResult) -> LsResult:
        if result.error or not result.entries:
            return result
        return LsResult(
            entries=[
                {**entry, "path": self._unscope(entry["path"])}
                for entry in result.entries
            ],
        )

    def _map_grep(self, result: GrepResult) -> GrepResult:
        if result.error or not result.matches:
            return result
        return GrepResult(
            matches=[{**m, "path": self._unscope(m["path"])} for m in result.matches],
            truncated=result.truncated,
        )

    def _map_glob(self, result: GlobResult) -> GlobResult:
        if result.error or not result.matches:
            return result
        return GlobResult(
            matches=[{**m, "path": self._unscope(m["path"])} for m in result.matches],
            truncated=result.truncated,
        )

    def _map_write(self, result: WriteResult) -> WriteResult:
        if result.error:
            return result
        return WriteResult(path=self._unscope_optional(result.path))

    def _map_edit(self, result: EditResult) -> EditResult:
        if result.error:
            return result
        return EditResult(
            path=self._unscope_optional(result.path),
            occurrences=result.occurrences,
        )

    def _map_delete(self, result: DeleteResult) -> DeleteResult:
        if result.error:
            return result
        return DeleteResult(path=self._unscope_optional(result.path))

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

    # ---- BackendProtocol surface ----------------------------------------

    def ls(self, path: str) -> LsResult:
        """Delegate ``ls`` to the wrapped sandbox, unscoping result paths."""
        return self._map_ls(self._sandbox.ls(self._scope(path) or "/"))

    async def als(self, path: str) -> LsResult:
        """Async version of :meth:`ls`."""
        return self._map_ls(await self._sandbox.als(self._scope(path) or "/"))

    def read(
        self,
        file_path: str,
        offset: int = 0,
        limit: int = 2000,
    ) -> ReadResult:
        """Read a file inside the scoped namespace."""
        return self._sandbox.read(self._scope(file_path) or file_path, offset, limit)

    async def aread(
        self,
        file_path: str,
        offset: int = 0,
        limit: int = 2000,
    ) -> ReadResult:
        """Async version of :meth:`read`."""
        return await self._sandbox.aread(
            self._scope(file_path) or file_path,
            offset,
            limit,
        )

    def grep(
        self,
        pattern: str,
        path: str | None = None,
        glob: str | None = None,
        *,
        max_count: int | None = None,
    ) -> GrepResult:
        """Grep within the scoped namespace, unscoping result paths."""
        return self._map_grep(
            self._sandbox.grep(pattern, self._scope(path), glob, max_count=max_count),
        )

    async def agrep(
        self,
        pattern: str,
        path: str | None = None,
        glob: str | None = None,
        *,
        max_count: int | None = None,
    ) -> GrepResult:
        """Async version of :meth:`grep`."""
        return self._map_grep(
            await self._sandbox.agrep(
                pattern,
                self._scope(path),
                glob,
                max_count=max_count,
            ),
        )

    def glob(self, pattern: str, path: str | None = None) -> GlobResult:
        """Glob within the scoped namespace, unscoping result paths."""
        return self._map_glob(self._sandbox.glob(pattern, self._scope(path or "/")))

    async def aglob(self, pattern: str, path: str | None = None) -> GlobResult:
        """Async version of :meth:`glob`."""
        return self._map_glob(
            await self._sandbox.aglob(pattern, self._scope(path or "/")),
        )

    def write(self, file_path: str, content: str) -> WriteResult:
        """Write a file inside the scoped namespace (creating or overwriting)."""
        return self._map_write(
            self._sandbox.write(self._scope(file_path) or file_path, content),
        )

    async def awrite(self, file_path: str, content: str) -> WriteResult:
        """Async version of :meth:`write`."""
        return self._map_write(
            await self._sandbox.awrite(self._scope(file_path) or file_path, content),
        )

    def edit(
        self,
        file_path: str,
        old_string: str,
        new_string: str,
        replace_all: bool = False,  # noqa: FBT001, FBT002 -- mirrors BackendProtocol
    ) -> EditResult:
        """Edit a file inside the scoped namespace."""
        return self._map_edit(
            self._sandbox.edit(
                self._scope(file_path) or file_path,
                old_string,
                new_string,
                replace_all,
            ),
        )

    async def aedit(
        self,
        file_path: str,
        old_string: str,
        new_string: str,
        replace_all: bool = False,  # noqa: FBT001, FBT002 -- mirrors BackendProtocol
    ) -> EditResult:
        """Async version of :meth:`edit`."""
        return self._map_edit(
            await self._sandbox.aedit(
                self._scope(file_path) or file_path,
                old_string,
                new_string,
                replace_all,
            ),
        )

    def delete(self, file_path: str) -> DeleteResult:
        """Delete a path inside the scoped namespace, recursively.

        Deep Agents classifies `delete` as a *write* operation, and it
        removes everything nested under ``file_path``. The namespace prefix
        bounds which subtree the caller can name, but nothing else: a
        `delete` of the namespace root removes every file the route holds.
        """
        return self._map_delete(
            self._sandbox.delete(self._scope(file_path) or file_path),
        )

    async def adelete(self, file_path: str) -> DeleteResult:
        """Async version of :meth:`delete`."""
        return self._map_delete(
            await self._sandbox.adelete(self._scope(file_path) or file_path),
        )

    def upload_files(
        self,
        files: list[tuple[str, bytes]],
    ) -> list[FileUploadResponse]:
        """Upload many files at once into the scoped namespace."""
        scoped = [(self._scope(p) or p, content) for p, content in files]
        return [self._unscope_upload(r) for r in self._sandbox.upload_files(scoped)]

    async def aupload_files(
        self,
        files: list[tuple[str, bytes]],
    ) -> list[FileUploadResponse]:
        """Async version of :meth:`upload_files`."""
        scoped = [(self._scope(p) or p, content) for p, content in files]
        responses = await self._sandbox.aupload_files(scoped)
        return [self._unscope_upload(r) for r in responses]

    def download_files(self, paths: list[str]) -> list[FileDownloadResponse]:
        """Download many files at once from the scoped namespace."""
        scoped = [self._scope(p) or p for p in paths]
        return [self._unscope_download(r) for r in self._sandbox.download_files(scoped)]

    async def adownload_files(
        self,
        paths: list[str],
    ) -> list[FileDownloadResponse]:
        """Async version of :meth:`download_files`."""
        scoped = [self._scope(p) or p for p in paths]
        responses = await self._sandbox.adownload_files(scoped)
        return [self._unscope_download(r) for r in responses]


__all__ = ["WasmshFilesystemBackend", "WasmshNamespaceEscapeError"]
