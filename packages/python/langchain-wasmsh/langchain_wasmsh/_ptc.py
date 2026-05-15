"""Programmatic tool calling (PTC) types + prompt helpers.

PTC exposes the agent's LangChain tools inside the Python interpreter as
``tools.<snake_name>`` awaitables. The host-bridge that suspends user code
mid-execution and round-trips a call to LangChain lives in
:mod:`langchain_wasmsh._repl` (``_make_ptc_dispatcher``) and
:mod:`langchain_wasmsh.sandbox` (``run_ptc`` + ``_handle_host_call``).

This module owns just the public types and prompt helpers:

- :data:`PTCOption` — the ``ptc=`` constructor argument shape.
- :func:`filter_tools_for_ptc` — resolves a ``PTCOption`` against the live
  agent toolset, deduplicating and enforcing that the interpreter's own
  tool is never exposed.
- :func:`render_ptc_prompt` — builds the ``tools`` namespace section of the
  system prompt addendum.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from langchain_core.tools import BaseTool

from langchain_wasmsh import _prompt

if TYPE_CHECKING:
    from collections.abc import Sequence


PTCOption = list[str | BaseTool]


def filter_tools_for_ptc(
    tools: Sequence[BaseTool],
    config: PTCOption,
    *,
    self_tool_name: str,
) -> list[BaseTool]:
    """Return the subset of ``tools`` PTC would expose inside the interpreter.

    ``self_tool_name`` is the interpreter's own tool name; it is always
    excluded to prevent self-recursive bridging. ``config`` is allowlist-only:

    - ``str`` entries: expose matching tool names from ``tools``.
    - ``BaseTool`` entries: expose those tools directly.

    Mixed lists are merged. Explicit ``BaseTool`` entries are included first,
    then name-matched agent tools are appended. Duplicate names are
    deduplicated.
    """
    if not isinstance(config, list):
        msg = (
            "Unsupported `ptc` config type. "
            "Use a list of tool names, list of BaseTool instances, or None."
        )
        raise TypeError(msg)
    explicit_tools: list[BaseTool] = []
    allow_names: set[str] = set()
    for entry in config:
        if isinstance(entry, BaseTool):
            if entry.name != self_tool_name:
                explicit_tools.append(entry)
            continue
        if isinstance(entry, str):
            allow_names.add(entry)
            continue
        msg = "ptc list entries must be str or BaseTool"
        raise TypeError(msg)
    selected: list[BaseTool] = [
        *explicit_tools,
        *[t for t in tools if t.name != self_tool_name and t.name in allow_names],
    ]
    deduped: list[BaseTool] = []
    seen_names: set[str] = set()
    for tool in selected:
        if tool.name in seen_names:
            continue
        seen_names.add(tool.name)
        deduped.append(tool)
    _raise_on_invalid_ptc_tools(deduped)
    return deduped


def _raise_on_invalid_ptc_tools(tools: Sequence[BaseTool]) -> None:
    for tool in tools:
        snake = _prompt.to_snake_case(tool.name)
        if _prompt.is_valid_python_identifier(snake):
            continue
        msg = (
            f"PTC tool name {tool.name!r} cannot be exposed as Python "
            f"identifier {snake!r}. Tool names must map to "
            "`/^[A-Za-z_][A-Za-z0-9_]*$/`."
        )
        raise ValueError(msg)


def render_ptc_prompt(
    tools: Sequence[BaseTool],
    *,
    tool_name: str = "py_eval",
) -> str:
    """Build the ``tools`` namespace section of the system prompt."""
    if not tools:
        return ""
    _raise_on_invalid_ptc_tools(tools)
    return _prompt.render_ptc_prompt(tools, tool_name=tool_name)
