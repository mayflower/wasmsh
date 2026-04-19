"""Error-mapping helpers shared by the local and remote sandbox backends.

Both backends receive the same `events` / error-message strings from the
underlying wasmsh runtime (in-process via stdio JSON, remote via dispatcher
HTTP).  Keeping a single classifier here prevents the two call sites from
drifting apart as new error shapes appear.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from deepagents.backends.protocol import FileOperationError


def map_error(message: str | None) -> FileOperationError:
    """Classify a runtime error message into a deepagents FileOperationError."""
    normalized = (message or "").lower()
    if "not found" in normalized:
        return "file_not_found"
    if "directory" in normalized:
        return "is_directory"
    if "permission" in normalized:
        return "permission_denied"
    return "invalid_path"


def extract_diagnostic(events: list[dict[str, Any]] | None) -> str | None:
    """Return the first `Diagnostic` message from a wasmsh event list, if any."""
    if not events:
        return None
    for event in events:
        diagnostic = event.get("Diagnostic")
        if isinstance(diagnostic, list) and len(diagnostic) >= 2:  # noqa: PLR2004
            return str(diagnostic[1])
    return None
