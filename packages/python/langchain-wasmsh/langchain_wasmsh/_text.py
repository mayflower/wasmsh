"""Text + binary payload helpers shared by the local and remote backends.

The sandbox standard suite (`langchain-tests>=1.1.5`) makes byte-exact
assertions against the output of `read()`, so the pagination and
truncation logic must be identical regardless of transport.
"""

from __future__ import annotations

import base64

# Binary preview + text-output caps, kept identical to BaseSandbox so the
# `langchain-tests` standard suite's exact-match assertions pass.
MAX_BINARY_PREVIEW_BYTES = 500 * 1024
MAX_TEXT_OUTPUT_BYTES = 500 * 1024
TEXT_TRUNCATION_NOTE = (
    "\n\n[Output was truncated due to size limits. "
    "This paginated read result exceeded the sandbox stdout limit. "
    "Continue reading with a larger offset or smaller limit to inspect "
    "the rest of the file.]"
)


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


def paginate_text(text: str, *, offset: int, limit: int) -> str:
    """Apply (offset, limit) line-window + 500 KiB output cap.

    Matches BaseSandbox's semantics: lines are 0-indexed starting from
    `offset`, at most `limit` lines are returned, and the assembled
    content is truncated at ~500 KiB with a user-facing note so agents
    know to paginate further.
    """
    lines = text.split("\n")
    window = lines[offset : offset + limit]
    if not window:
        return ""

    parts: list[str] = []
    current_bytes = 0
    truncation_bytes = len(TEXT_TRUNCATION_NOTE.encode("utf-8"))
    effective_limit = MAX_TEXT_OUTPUT_BYTES - truncation_bytes
    truncated = False
    for i, line in enumerate(window):
        piece = line if i == 0 else "\n" + line
        piece_bytes = len(piece.encode("utf-8"))
        if current_bytes + piece_bytes > effective_limit:
            truncated = True
            remaining = effective_limit - current_bytes
            if remaining > 0:
                prefix = piece.encode("utf-8")[:remaining].decode(
                    "utf-8", errors="ignore"
                )
                if prefix:
                    parts.append(prefix)
            break
        parts.append(piece)
        current_bytes += piece_bytes

    output = "".join(parts)
    if truncated:
        output += TEXT_TRUNCATION_NOTE
    return output
