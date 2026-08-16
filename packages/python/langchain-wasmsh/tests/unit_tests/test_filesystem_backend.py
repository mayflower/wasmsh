"""Tests for `WasmshFilesystemBackend`, the namespace/path adapter.

It is deliberately thin, so almost every test here is about one of two
things: the path rewriting in and out of the namespace, and forwarding a
result without losing a field. Both are places where a quiet mistake turns
into a wrong answer rather than an error — a dropped `max_count` silently
returns more matches than asked for, a dropped `truncated` reports a
partial result as complete, and a leaked scoped path tells the model a file
lives somewhere it cannot address.
"""

from __future__ import annotations

from typing import Any

import pytest
from deepagents.backends.protocol import (
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

from langchain_wasmsh import WasmshFilesystemBackend
from langchain_wasmsh.filesystem import WasmshNamespaceEscapeError


class _RecordingSandbox:
    """Records the scoped call it received and returns a scripted result."""

    def __init__(self, result: Any = None) -> None:
        self.result = result
        self.calls: list[tuple[str, tuple[Any, ...], dict[str, Any]]] = []

    def _record(self, name: str, *args: Any, **kwargs: Any) -> Any:
        self.calls.append((name, args, kwargs))
        return self.result

    def __getattr__(self, name: str) -> Any:
        def _call(*args: Any, **kwargs: Any) -> Any:
            return self._record(name, *args, **kwargs)

        async def _acall(*args: Any, **kwargs: Any) -> Any:
            return self._record(name, *args, **kwargs)

        return _acall if name.startswith("a") else _call

    @property
    def last(self) -> tuple[str, tuple[Any, ...], dict[str, Any]]:
        return self.calls[-1]


def _backend(result: Any = None, namespace: str = "/mem") -> tuple[Any, Any]:
    sandbox = _RecordingSandbox(result)
    return WasmshFilesystemBackend(sandbox, namespace=namespace), sandbox


# ── namespace mapping ──────────────────────────────────────────────────


class TestNamespaceMapping:
    def test_prefix_applied_to_uploads(self) -> None:
        backend, sandbox = _backend([FileUploadResponse(path="/mem/note.txt")])
        backend.upload_files([("/note.txt", b"hi")])
        assert sandbox.last[1][0] == [("/mem/note.txt", b"hi")]

    def test_download_response_path_is_unscoped(self) -> None:
        backend, _ = _backend(
            [FileDownloadResponse(path="/mem/note.txt", content=b"hi")],
        )
        responses = backend.download_files(["/note.txt"])
        assert responses[0].path == "/note.txt"
        assert responses[0].content == b"hi"

    def test_trailing_slash_in_namespace_is_normalised(self) -> None:
        backend, sandbox = _backend([FileUploadResponse(path="/mem/n")], "/mem/")
        backend.upload_files([("/n", b"hi")])
        assert sandbox.last[1][0] == [("/mem/n", b"hi")]

    def test_empty_namespace_is_passthrough(self) -> None:
        backend, sandbox = _backend([FileUploadResponse(path="/note.txt")], "")
        backend.upload_files([("/note.txt", b"hi")])
        assert sandbox.last[1][0] == [("/note.txt", b"hi")]

    def test_namespace_root_maps_to_the_prefix(self) -> None:
        backend, sandbox = _backend(LsResult(entries=[]))
        backend.ls("/")
        assert sandbox.last[1][0] == "/mem"


class TestTraversalContainment:
    """Pyodide's POSIX VFS resolves `..` segments; the namespace must hold."""

    ESCAPE = "/../skills/secret.py"

    def test_every_entry_point_rejects_an_escape(self) -> None:
        backend, sandbox = _backend()
        with pytest.raises(WasmshNamespaceEscapeError):
            backend.read(self.ESCAPE)
        with pytest.raises(WasmshNamespaceEscapeError):
            backend.write(self.ESCAPE, "x")
        with pytest.raises(WasmshNamespaceEscapeError):
            backend.edit(self.ESCAPE, "a", "b")
        with pytest.raises(WasmshNamespaceEscapeError):
            backend.delete(self.ESCAPE)
        with pytest.raises(WasmshNamespaceEscapeError):
            backend.upload_files([(self.ESCAPE, b"x")])
        with pytest.raises(WasmshNamespaceEscapeError):
            backend.download_files([self.ESCAPE])
        with pytest.raises(WasmshNamespaceEscapeError):
            backend.ls(self.ESCAPE)
        with pytest.raises(WasmshNamespaceEscapeError):
            backend.glob("*.py", self.ESCAPE)
        with pytest.raises(WasmshNamespaceEscapeError):
            backend.grep("TODO", self.ESCAPE)
        # Nothing reached the sandbox.
        assert sandbox.calls == []

    async def test_every_async_entry_point_rejects_an_escape(self) -> None:
        backend, sandbox = _backend()
        with pytest.raises(WasmshNamespaceEscapeError):
            await backend.aread(self.ESCAPE)
        with pytest.raises(WasmshNamespaceEscapeError):
            await backend.awrite(self.ESCAPE, "x")
        with pytest.raises(WasmshNamespaceEscapeError):
            await backend.aedit(self.ESCAPE, "a", "b")
        with pytest.raises(WasmshNamespaceEscapeError):
            await backend.adelete(self.ESCAPE)
        with pytest.raises(WasmshNamespaceEscapeError):
            await backend.aupload_files([(self.ESCAPE, b"x")])
        with pytest.raises(WasmshNamespaceEscapeError):
            await backend.adownload_files([self.ESCAPE])
        with pytest.raises(WasmshNamespaceEscapeError):
            await backend.als(self.ESCAPE)
        with pytest.raises(WasmshNamespaceEscapeError):
            await backend.aglob("*.py", self.ESCAPE)
        with pytest.raises(WasmshNamespaceEscapeError):
            await backend.agrep("TODO", self.ESCAPE)
        assert sandbox.calls == []

    def test_multi_segment_dotdot_payload_rejected(self) -> None:
        backend, _ = _backend()
        with pytest.raises(WasmshNamespaceEscapeError, match="escapes namespace"):
            backend.read("../../skills/secret.py")

    def test_interior_dotdot_landing_outside_namespace_rejected(self) -> None:
        backend, _ = _backend()
        with pytest.raises(WasmshNamespaceEscapeError):
            backend.read("/x/../../etc/passwd")

    def test_sibling_prefix_attack_rejected(self) -> None:
        """`/memstore` shares the prefix `/mem` but is a different sibling."""
        backend, _ = _backend()
        with pytest.raises(WasmshNamespaceEscapeError):
            backend.read("/../memstore/x")

    def test_interior_dotdot_staying_inside_namespace_allowed(self) -> None:
        backend, sandbox = _backend([FileUploadResponse(path="/mem/b/x")])
        backend.upload_files([("/a/../b/x", b"hi")])
        assert sandbox.last[1][0] == [("/mem/b/x", b"hi")]

    def test_dot_segments_allowed(self) -> None:
        backend, sandbox = _backend([FileUploadResponse(path="/mem/sub/x")])
        backend.upload_files([("/./sub/x", b"hi")])
        assert sandbox.last[1][0] == [("/mem/sub/x", b"hi")]

    def test_a_result_path_outside_the_namespace_is_refused(self) -> None:
        # Defence in depth: a sandbox bug must not leak a foreign path into
        # the caller's view as if it were addressable.
        backend, _ = _backend(LsResult(entries=[{"path": "/etc/passwd"}]))
        with pytest.raises(WasmshNamespaceEscapeError):
            backend.ls("/")


# ── result forwarding ──────────────────────────────────────────────────


class TestGrepForwarding:
    def test_max_count_reaches_the_sandbox(self) -> None:
        backend, sandbox = _backend(GrepResult(matches=[]))
        backend.grep("TODO", "/notes", "*.md", max_count=5)
        name, args, kwargs = sandbox.last
        assert name == "grep"
        assert args == ("TODO", "/mem/notes", "*.md")
        assert kwargs == {"max_count": 5}

    def test_truncated_is_preserved(self) -> None:
        # Dropping this would report a capped search as a complete one.
        backend, _ = _backend(
            GrepResult(
                matches=[{"path": "/mem/a.md", "line": 1, "text": "TODO"}],
                truncated=True,
            ),
        )
        result = backend.grep("TODO", max_count=1)
        assert result.truncated is True
        assert result.matches == [{"path": "/a.md", "line": 1, "text": "TODO"}]

    def test_none_path_stays_none(self) -> None:
        backend, sandbox = _backend(GrepResult(matches=[]))
        backend.grep("TODO")
        assert sandbox.last[1][1] is None

    def test_error_is_forwarded_untouched(self) -> None:
        backend, _ = _backend(GrepResult(error="Path '/mem/x': boom"))
        assert backend.grep("TODO", "/x").error == "Path '/mem/x': boom"

    async def test_async_parity(self) -> None:
        backend, sandbox = _backend(GrepResult(matches=[], truncated=True))
        result = await backend.agrep("TODO", "/notes", max_count=2)
        assert sandbox.last[0] == "agrep"
        assert sandbox.last[2] == {"max_count": 2}
        assert result.truncated is True


class TestGlobForwarding:
    def test_none_path_defaults_to_the_namespace_root(self) -> None:
        backend, sandbox = _backend(GlobResult(matches=[]))
        backend.glob("**/*.md")
        assert sandbox.last[1] == ("**/*.md", "/mem")

    def test_truncated_is_preserved(self) -> None:
        backend, _ = _backend(
            GlobResult(matches=[{"path": "/mem/a.md"}], truncated=True),
        )
        result = backend.glob("**/*.md")
        assert result.truncated is True
        assert result.matches == [{"path": "/a.md"}]

    async def test_async_parity(self) -> None:
        backend, sandbox = _backend(GlobResult(matches=[], truncated=True))
        result = await backend.aglob("**/*.md")
        assert sandbox.last[0] == "aglob"
        assert result.truncated is True


class TestResultPathUnscoping:
    def test_write_result_path(self) -> None:
        backend, _ = _backend(WriteResult(path="/mem/note.txt"))
        assert backend.write("/note.txt", "x").path == "/note.txt"

    def test_edit_result_path_and_occurrences(self) -> None:
        backend, _ = _backend(EditResult(path="/mem/note.txt", occurrences=3))
        result = backend.edit("/note.txt", "a", "b", replace_all=True)
        assert result.path == "/note.txt"
        assert result.occurrences == 3

    def test_delete_result_path(self) -> None:
        backend, sandbox = _backend(DeleteResult(path="/mem/dir"))
        assert backend.delete("/dir").path == "/dir"
        assert sandbox.last[1] == ("/mem/dir",)

    def test_ls_entry_paths(self) -> None:
        backend, _ = _backend(
            LsResult(entries=[{"path": "/mem/a", "is_dir": False}]),
        )
        assert backend.ls("/").entries == [{"path": "/a", "is_dir": False}]

    def test_errors_keep_the_scoped_path(self) -> None:
        # The error names the path the sandbox actually operated on;
        # rewriting it could change what the message means.
        backend, _ = _backend(WriteResult(error="Failed to write '/mem/note.txt'"))
        assert backend.write("/note.txt", "x").error == (
            "Failed to write '/mem/note.txt'"
        )

    async def test_async_result_paths_are_unscoped_too(self) -> None:
        backend, _ = _backend(DeleteResult(path="/mem/dir"))
        assert (await backend.adelete("/dir")).path == "/dir"


class TestReadForwarding:
    def test_offset_and_limit_are_positional_like_the_protocol(self) -> None:
        backend, sandbox = _backend(ReadResult(error="x"))
        backend.read("/note.txt", 5, 10)
        assert sandbox.last[1] == ("/mem/note.txt", 5, 10)

    async def test_async_parity(self) -> None:
        backend, sandbox = _backend(ReadResult(error="x"))
        await backend.aread("/note.txt", 5, 10)
        assert sandbox.last[0] == "aread"
        assert sandbox.last[1] == ("/mem/note.txt", 5, 10)
