"""Guards on the exact Deep Agents surface this adapter is pinned against.

The adapter declares `deepagents>=0.7.4,<0.8.0` and, inside that window,
reuses a handful of upstream helpers rather than reimplementing them — see
`langchain_wasmsh._file_ops`. That reuse is what keeps wasmsh's two transport
overrides semantically identical to upstream instead of a second copy that
drifts, but it only holds while those helpers exist with the shape the
adapter assumes. These tests turn a silent behavioural regression on a minor
upstream release into a loud test failure.
"""

from __future__ import annotations

import inspect
from typing import Any

import pytest
from deepagents import __version__ as deepagents_version
from deepagents.backends.protocol import (
    BackendProtocol,
    ReadResult,
    SandboxBackendProtocol,
)
from deepagents.middleware._utils import append_to_system_message
from langchain_core.messages import SystemMessage

from langchain_wasmsh import (
    WasmshFilesystemBackend,
    WasmshRemoteSandbox,
    WasmshSandbox,
)
from langchain_wasmsh._file_ops import UPSTREAM_INTERNALS
from langchain_wasmsh._prompt import append_system_prompt_block

_SUPPORTED_MAJOR_MINOR = (0, 7)


class TestPinnedVersion:
    def test_installed_deepagents_is_inside_the_declared_window(self) -> None:
        major, minor = (int(part) for part in deepagents_version.split(".")[:2])
        assert (major, minor) == _SUPPORTED_MAJOR_MINOR, (
            f"deepagents {deepagents_version} is outside the >=0.7.4,<0.8.0 "
            "window this adapter is tested against"
        )


class TestUpstreamInternals:
    @pytest.mark.parametrize("name", sorted(UPSTREAM_INTERNALS))
    def test_symbol_is_still_present(self, name: str) -> None:
        assert UPSTREAM_INTERNALS[name] is not None, name

    def test_edit_via_upload_signature(self) -> None:
        # `route_edit_via_upload` calls this unbound with a sandbox as the
        # first argument, so parameter order is load-bearing.
        sig = inspect.signature(UPSTREAM_INTERNALS["BaseSandbox._edit_via_upload"])
        assert list(sig.parameters) == [
            "self",
            "file_path",
            "old_string",
            "new_string",
            "replace_all",
        ]

    def test_aedit_via_upload_signature(self) -> None:
        sig = inspect.signature(UPSTREAM_INTERNALS["BaseSandbox._aedit_via_upload"])
        assert list(sig.parameters) == [
            "self",
            "file_path",
            "old_string",
            "new_string",
            "replace_all",
        ]

    def test_parse_grep_output_signature(self) -> None:
        parse = UPSTREAM_INTERNALS["deepagents.backends.sandbox._parse_grep_output"]
        assert list(inspect.signature(parse).parameters) == [
            "result",
            "path",
            "max_count",
        ]

    def test_map_edit_error_signature(self) -> None:
        mapper = UPSTREAM_INTERNALS["deepagents.backends.sandbox._map_edit_error"]
        assert list(inspect.signature(mapper).parameters) == [
            "error",
            "file_path",
            "old_string",
        ]

    def test_backend_read_file_type_classifies_media_as_binary(self) -> None:
        classify = UPSTREAM_INTERNALS[
            "deepagents.backends.utils._get_backend_read_file_type"
        ]
        assert classify("/a/logo.png") == "image"
        assert classify("/a/clip.mkv") == "video"
        assert classify("/a/notes.md") == "text"


def _shape(method: object) -> list[tuple[str, int, object]]:
    """Reduce a signature to (name, kind, default) per parameter.

    Annotations are deliberately excluded: this package uses
    `from __future__ import annotations`, so its annotations are strings
    while upstream's are objects, and comparing their text would fail on a
    difference that means nothing at call time. Parameter names, kinds, and
    defaults are what a caller actually depends on.
    """
    return [
        (name, param.kind.value, param.default)
        for name, param in inspect.signature(method).parameters.items()
    ]


_PROTOCOL_METHODS = [
    "ls",
    "als",
    "read",
    "aread",
    "write",
    "awrite",
    "edit",
    "aedit",
    "delete",
    "adelete",
    "grep",
    "agrep",
    "glob",
    "aglob",
    "upload_files",
    "aupload_files",
    "download_files",
    "adownload_files",
]


class TestBackendSignatureParity:
    """Both sandboxes must present the exact 0.7.4 protocol signatures."""

    @pytest.mark.parametrize("cls", [WasmshSandbox, WasmshRemoteSandbox])
    @pytest.mark.parametrize("method", _PROTOCOL_METHODS)
    def test_matches_protocol_signature(self, cls: type, method: str) -> None:
        assert _shape(getattr(cls, method)) == _shape(
            getattr(BackendProtocol, method),
        ), f"{cls.__name__}.{method}"

    @pytest.mark.parametrize("cls", [WasmshSandbox, WasmshRemoteSandbox])
    def test_execute_accepts_a_timeout(self, cls: type) -> None:
        params = inspect.signature(cls.execute).parameters
        assert "timeout" in params
        assert params["timeout"].kind is inspect.Parameter.KEYWORD_ONLY

    @pytest.mark.parametrize("cls", [WasmshSandbox, WasmshRemoteSandbox])
    def test_is_a_sandbox_backend(self, cls: type) -> None:
        assert issubclass(cls, SandboxBackendProtocol)

    @pytest.mark.parametrize("method", _PROTOCOL_METHODS)
    def test_namespace_backend_matches_protocol_signature(self, method: str) -> None:
        assert _shape(getattr(WasmshFilesystemBackend, method)) == _shape(
            getattr(BackendProtocol, method),
        ), f"WasmshFilesystemBackend.{method}"

    def test_namespace_backend_exposes_no_execute(self) -> None:
        # It routes file operations for a memory/skills prefix; handing it a
        # shell would make the namespace prefix meaningless.
        assert not hasattr(WasmshFilesystemBackend, "execute")


class TestReadResultStrictConstruction:
    """`ReadResult` rejects malformed pagination in 0.7.4; rely on that."""

    def test_window_fields_must_be_paired(self) -> None:
        with pytest.raises(ValueError, match="together"):
            ReadResult(start_line=1)

    def test_next_offset_must_equal_end_line(self) -> None:
        with pytest.raises(ValueError, match="next_offset"):
            ReadResult(start_line=1, end_line=5, next_offset=9)

    def test_no_lines_requested_excludes_pagination(self) -> None:
        with pytest.raises(ValueError, match="uninspected"):
            ReadResult(no_lines_requested=True, start_line=1, end_line=1)


class TestSystemPromptAppendParity:
    """The local block-append must behave like upstream's, plus metadata."""

    @staticmethod
    def _both(message: SystemMessage | None, text: str) -> tuple[Any, Any]:
        return (
            append_system_prompt_block(message, text).content,
            append_to_system_message(message, text).content,
        )

    def test_none_message(self) -> None:
        ours, theirs = self._both(None, "INTERPRETER")
        assert ours == theirs

    def test_plain_text_message(self) -> None:
        ours, theirs = self._both(SystemMessage(content="base"), "INTERPRETER")
        assert ours == theirs

    def test_block_message_with_cache_control(self) -> None:
        message = SystemMessage(
            content=[
                {
                    "type": "text",
                    "text": "memory + skills",
                    "cache_control": {"type": "ephemeral"},
                },
            ],
        )
        ours, theirs = self._both(message, "INTERPRETER")
        assert ours == theirs
        # The prompt-cache marker survives verbatim: flattening it would
        # silently re-bill the whole cached prefix on every request.
        assert ours[0]["cache_control"] == {"type": "ephemeral"}

    def test_blank_line_separator_matches_upstream(self) -> None:
        ours, _ = self._both(SystemMessage(content="base"), "INTERPRETER")
        assert ours[-1]["text"] == "\n\nINTERPRETER"

    def test_message_metadata_is_preserved(self) -> None:
        # Upstream drops these; we keep them, which is the one deliberate
        # difference and the reason for a local helper.
        message = SystemMessage(
            content="base",
            additional_kwargs={"provider": "x"},
            response_metadata={"n": 1},
            name="sys",
            id="msg-1",
        )
        appended = append_system_prompt_block(message, "INTERPRETER")
        assert appended.additional_kwargs == {"provider": "x"}
        assert appended.response_metadata == {"n": 1}
        assert appended.name == "sys"
        assert appended.id == "msg-1"

    def test_input_message_is_not_mutated(self) -> None:
        message = SystemMessage(content=[{"type": "text", "text": "base"}])
        before = list(message.content)
        append_system_prompt_block(message, "INTERPRETER")
        assert message.content == before
