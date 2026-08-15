r"""Transport-specific file-operation helpers shared by both wasmsh sandboxes.

`WasmshSandbox` and `WasmshRemoteSandbox` are ordinary
:class:`deepagents.backends.sandbox.BaseSandbox` subclasses: `ls`, `read`,
`write`, `delete`, `glob`, `upload_files`, and `download_files` all run the
upstream Deep Agents 0.7.4 implementations unchanged, so their result shapes
and error strings are upstream's by construction rather than by imitation.

Two operations cannot use the upstream server-side command as written. Both
divergences are transport bugs in the wasmsh shell, not semantic
disagreements, so the helpers here re-route the same upstream logic instead
of reimplementing it:

`edit`
    Upstream's default route feeds its JSON payload to `python3 -c` through
    a heredoc on stdin. wasmsh's in-process `python3` builtin runs via
    `PyRun_SimpleString` and never receives the shell's stdin, so
    `json.loads(sys.stdin.read())` raises before the edit starts. Upstream's
    own large-payload route (`_edit_via_upload`) writes `old_string` /
    `new_string` to temp files and passes only base64 paths on the command
    line — no stdin — and works unmodified. :func:`route_edit_via_upload`
    therefore forces that route for every payload size. The replacement
    algorithm, CRLF handling, occurrence counting, and error strings all
    stay upstream's.

`grep`
    Upstream runs `grep -rHnFZ`, where `-Z` asks for a NUL between the file
    name and the line data so a path containing `:` stays unambiguous.
    wasmsh's `grep` silently accepts unknown short flags, so `-Z` is a no-op
    and every record arrives as `path:line:text`. Upstream's parser then
    fails to split the record and reports the *matches* as an error string.
    :func:`build_grep_cmd` emits an in-sandbox Python script that produces
    exactly the `path\0line:text` records upstream expects, and parsing
    stays with upstream's `_parse_grep_output` so `max_count`, `truncated`,
    and error formatting are unchanged.

Everything else in this module is plain wire encoding shared by the two
transports.
"""

from __future__ import annotations

import base64
from typing import TYPE_CHECKING, Any

from deepagents.backends.protocol import (
    EditResult,  # noqa: TC002 -- constructed at runtime
)
from deepagents.backends.sandbox import (
    BaseSandbox,
    _map_edit_error,
    _parse_grep_output,
)
from deepagents.backends.utils import _get_backend_read_file_type

if TYPE_CHECKING:
    from deepagents.backends.protocol import ExecuteResponse, GrepResult

TIMEOUT_EXIT_CODE = 124
"""Exit code reported when `execute(timeout=N)` hits its deadline.

Matches GNU `timeout(1)`, which Deep Agents' documentation cites as the
conventional signal for "the command did not finish in time".
"""


def timeout_response_output(command: str, timeout: int) -> str:
    """Render the model-facing output for a timed-out `execute` call.

    Args:
        command: The command that was still running when the deadline hit.
        timeout: The deadline in seconds.

    Returns:
        A single-line explanation that also states the session was
            destroyed, so the model does not assume interpreter state
            survived.
    """
    return (
        f"Error: command timed out after {timeout}s and was terminated: {command}\n"
        "The wasmsh session could not be interrupted safely and was destroyed; "
        "any interpreter state from this session is gone."
    )


# ── wire encoding ───────────────────────────────────────────────────────


def encode_content(content: bytes) -> str:
    """Encode raw bytes as ascii-safe base64 for wire transport."""
    return base64.b64encode(content).decode("ascii")


def decode_content(content: str) -> bytes:
    """Decode a base64 string back into raw bytes."""
    return base64.b64decode(content.encode("ascii"))


def to_initial_files(
    files: dict[str, str | bytes] | None,
) -> list[dict[str, str]]:
    """Convert a user-supplied file dict into the wasmsh `initialFiles` payload."""
    if not files:
        return []
    encoded: list[dict[str, str]] = []
    for path, content in files.items():
        payload = content.encode("utf-8") if isinstance(content, str) else content
        encoded.append({"path": path, "contentBase64": encode_content(payload)})
    return encoded


# ── edit ────────────────────────────────────────────────────────────────


def reject_binary_edit(file_path: str) -> EditResult | None:
    """Refuse to edit a path whose extension marks it as non-text.

    Upstream's edit script rejects a file whose bytes fail to decode as
    UTF-8, which catches most binaries. It does not catch a `.png` (or any
    other known media extension) that happens to decode cleanly. That gap
    matters here because upstream `read` already returns such a file
    base64-encoded: an `old_string` the model derived from that read is
    base64 text, and replacing it inside the raw bytes would silently
    corrupt the file.

    Args:
        file_path: Absolute path the caller asked to edit.

    Returns:
        The upstream `not_a_text_file` `EditResult` when the extension is a
            known image/audio/video/document type, else `None`.
    """
    if _get_backend_read_file_type(file_path) == "text":
        return None
    return _map_edit_error("not_a_text_file", file_path, "")


def route_edit_via_upload(
    sandbox: BaseSandbox,
    file_path: str,
    old_string: str,
    new_string: str,
    replace_all: bool,  # noqa: FBT001 -- positional to mirror BackendProtocol.edit
) -> EditResult:
    """Run upstream's temp-file edit route regardless of payload size.

    See the module docstring for why the inline (heredoc) route cannot work
    against wasmsh's `python3` builtin.
    """
    rejected = reject_binary_edit(file_path)
    if rejected is not None:
        return rejected
    return BaseSandbox._edit_via_upload(  # noqa: SLF001 -- deliberate upstream reuse
        sandbox,
        file_path,
        old_string,
        new_string,
        replace_all,
    )


async def aroute_edit_via_upload(
    sandbox: BaseSandbox,
    file_path: str,
    old_string: str,
    new_string: str,
    replace_all: bool,  # noqa: FBT001 -- positional to mirror BackendProtocol.edit
) -> EditResult:
    """Async sibling of :func:`route_edit_via_upload`."""
    rejected = reject_binary_edit(file_path)
    if rejected is not None:
        return rejected
    return await BaseSandbox._aedit_via_upload(  # noqa: SLF001 -- deliberate upstream reuse
        sandbox,
        file_path,
        old_string,
        new_string,
        replace_all,
    )


# ── grep ────────────────────────────────────────────────────────────────


_GREP_TEMPLATE = """python3 -c "
import base64, fnmatch, glob as globmod, os, sys

search_path = base64.b64decode('{path_b64}').decode('utf-8')
glob_raw = base64.b64decode('{glob_b64}').decode('utf-8')
glob_pat = glob_raw or None
pattern = base64.b64decode('{pattern_b64}').decode('utf-8')
max_count = {max_count}

NUL = chr(0)
NL = chr(10)
BACKSLASH = chr(92)

targets = []
if os.path.isdir(search_path):
    real_root = os.path.realpath(search_path)
    if glob_pat is not None and '/' in glob_pat:
        rel_glob = glob_pat.lstrip('/')
        if any(seg == '..' for seg in rel_glob.replace(BACKSLASH, '/').split('/')):
            sys.stderr.write('glob contains path traversal' + NL)
            sys.exit(2)
        os.chdir(search_path)
        for rel in sorted(globmod.glob(rel_glob, recursive=True)):
            real_open = os.path.realpath(rel)
            if real_open != real_root and not real_open.startswith(real_root + os.sep):
                continue
            if not os.path.isfile(real_open):
                continue
            rel_display = os.path.relpath(real_open, real_root)
            targets.append((real_open, os.path.join(search_path, rel_display)))
    else:
        for dirpath, dirnames, filenames in os.walk(search_path):
            dirnames.sort()
            for fname in sorted(filenames):
                if glob_pat is not None and not fnmatch.fnmatchcase(fname, glob_pat):
                    continue
                full = os.path.join(dirpath, fname)
                targets.append((full, full))
elif os.path.exists(search_path):
    targets = [(search_path, search_path)]
else:
    sys.stderr.write('grep search root does not exist' + NL)
    sys.exit(2)

match_count = 0
for open_path, display_path in targets:
    try:
        handle = open(open_path, 'r', encoding='utf-8', errors='ignore')
    except OSError:
        continue
    try:
        for i, line in enumerate(handle, 1):
            if pattern in line:
                record = display_path + NUL + str(i) + ':' + line.rstrip(NL)
                sys.stdout.write(record + NL)
                match_count += 1
                if max_count is not None and match_count > max_count:
                    sys.exit(0)
    except OSError:
        pass
    finally:
        handle.close()
" 2>/dev/null"""
r"""Literal-substring grep implemented in-sandbox, emitting upstream records.

Mirrors the two include-glob behaviours upstream splits across `grep
--include` and its own path-glob template:

- a glob without `/` matches the *basename* at any depth (`*.py` finds
  `a/b/target.py`), which is what GNU `grep --include` does;
- a glob containing `/` is resolved relative to the search root with `**`
  support, matching upstream's `_GREP_PATH_GLOB_TEMPLATE` (including its
  traversal guard and realpath containment check).

Records are `path\0line:text` terminated by a newline — byte-identical to
`grep -rHnFZ` — so upstream's `_parse_grep_output` consumes them unchanged.
One record past `max_count` is emitted deliberately so that parser can tell
"exactly at the cap" (complete) from "capped early" (truncated).

`stderr` is discarded but the exit code is not masked: a legitimate
zero-match search exits 0, while a missing search root or a traversing glob
exits 2 and surfaces as `GrepResult.error`.
"""


def _b64(value: str) -> str:
    return base64.b64encode(value.encode("utf-8")).decode("ascii")


def build_grep_cmd(
    pattern: str,
    path: str | None,
    glob: str | None,
    max_count: int | None,
) -> str:
    """Render the in-sandbox grep script for one search."""
    return _GREP_TEMPLATE.format(
        path_b64=_b64(path or "."),
        glob_b64=_b64(glob or ""),
        pattern_b64=_b64(pattern),
        max_count="None" if max_count is None else int(max_count),
    )


def parse_grep_output(
    result: ExecuteResponse,
    path: str | None,
    max_count: int | None,
) -> GrepResult:
    """Parse grep records with upstream's parser (identical record format)."""
    return _parse_grep_output(result, path, max_count)


# ── upstream-internal surface used above ────────────────────────────────

UPSTREAM_INTERNALS: dict[str, Any] = {
    "BaseSandbox._edit_via_upload": BaseSandbox._edit_via_upload,  # noqa: SLF001
    "BaseSandbox._aedit_via_upload": BaseSandbox._aedit_via_upload,  # noqa: SLF001
    "deepagents.backends.sandbox._map_edit_error": _map_edit_error,
    "deepagents.backends.sandbox._parse_grep_output": _parse_grep_output,
    "deepagents.backends.utils._get_backend_read_file_type": (
        _get_backend_read_file_type
    ),
}
"""Every private Deep Agents symbol this adapter depends on, by name.

The adapter pins `deepagents>=0.7.4,<0.8.0`; within that window these
helpers are stable, and reusing them is what keeps wasmsh's two transport
overrides semantically identical to the upstream implementations rather
than a second copy that drifts. `tests/unit_tests/test_upstream_contract.py`
asserts each entry still exists with the expected signature, so a minor
upstream release that moves one of them fails loudly at test time instead of
silently at run time.
"""


__all__ = [
    "TIMEOUT_EXIT_CODE",
    "UPSTREAM_INTERNALS",
    "aroute_edit_via_upload",
    "build_grep_cmd",
    "decode_content",
    "encode_content",
    "parse_grep_output",
    "reject_binary_edit",
    "route_edit_via_upload",
    "timeout_response_output",
    "to_initial_files",
]
