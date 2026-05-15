"""System-prompt renderers for the Python REPL interpreter middleware."""

from __future__ import annotations

import re
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Sequence

    from langchain_core.tools import BaseTool

_PY_IDENTIFIER = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
_KEBAB_SEP = re.compile(r"-")

_REPL_SYSTEM_PROMPT_TEMPLATE = (
    "### Interpreter\n\n"
    "An `{tool_name}` tool is available. It runs Python in a persistent "
    "wasmsh sandbox.\n"
    "{state_persistence_line}\n"
    "- A virtual filesystem is available under `/workspace`; shell utilities "
    "(bash, grep, jq, sed, awk, curl, …) are reachable via `subprocess.run`.\n"
    "- Network access is allowlisted host-side; ad-hoc outbound calls will fail "
    "unless their host was permitted.\n"
    "- Per-call timeout: {timeout}s. Result and stdout are independently "
    "truncated to {max_result_chars} characters before returning to the model.\n"
    "- Use `print(...)` for intermediate values. The last expression of the "
    "evaluated block is also returned automatically if it is non-`None`."
)


def render_repl_system_prompt(
    *,
    tool_name: str,
    timeout: float,
    max_result_chars: int,
    snapshot_between_turns: bool,
) -> str:
    """Render the base REPL system prompt text for ``WasmshInterpreterMiddleware``."""
    state_persistence_line = (
        "- State (variables, imports, defined functions) persists across "
        "tool calls and across multiple turns for this conversation thread."
        if snapshot_between_turns
        else "- State persists across tool calls within a single turn of "
        "conversation. It DOES NOT persist across multiple turns."
    )
    return _REPL_SYSTEM_PROMPT_TEMPLATE.format(
        tool_name=tool_name,
        state_persistence_line=state_persistence_line,
        timeout=timeout,
        max_result_chars=max_result_chars,
    )


def to_snake_case(name: str) -> str:
    """Convert ``kebab-case`` → ``snake_case``. ``snake_case`` is returned as-is."""
    return _KEBAB_SEP.sub("_", name)


def is_valid_python_identifier(name: str) -> bool:
    """Return whether ``name`` is a valid Python identifier."""
    return _PY_IDENTIFIER.fullmatch(name) is not None


def is_valid_ptc_tool_name(name: str) -> bool:
    """Return whether a tool can be exposed as ``tools.<snake_case_name>``."""
    return is_valid_python_identifier(to_snake_case(name))


def render_ptc_prompt(tools: Sequence[BaseTool], *, tool_name: str = "eval") -> str:
    """Build the ``tools`` namespace section of the system prompt.

    Renders one async-function signature per exposed tool, in the shape the
    model will see inside its Python program::

        async def search(query: str) -> str: ...

    The prompt addendum is injected by ``WasmshInterpreterMiddleware`` when
    ``ptc=`` is set; the actual host-call bridge is wired in
    :mod:`langchain_wasmsh.sandbox` (``run_ptc`` + ``_handle_host_call``).
    """
    if not tools:
        return ""
    blocks: list[str] = []
    for tool in tools:
        snake = to_snake_case(tool.name)
        description = (
            (tool.description or "").strip().splitlines()[0] if tool.description else ""
        )
        signature = f"async def {snake}(**kwargs) -> str: ..."
        if description:
            blocks.append(f'    """{description}"""\n    {signature}')
        else:
            blocks.append(f"    {signature}")
    body = "\n\n".join(blocks)
    return (
        "\n\n"
        "### API Reference — `tools` namespace\n\n"
        "The agent tools listed below are exposed inside the Python interpreter "
        "as awaitable attributes of the global `tools` object. Each takes "
        "keyword arguments and returns the tool's native value.\n\n"
        "Invocation: `await tools.<name>(**kwargs)`. Use `asyncio.gather(...)` "
        "to fan out independent calls.\n\n"
        f"- If the task needs multiple tool calls, prefer one `{tool_name}` "
        "invocation that performs all of them rather than splitting the work "
        f"across multiple `{tool_name}` calls.\n"
        "- Pipeline dependent calls within a single program: if a result from "
        "one tool is needed as input to a later tool, chain them in one "
        "program instead of returning the intermediate value to the model.\n\n"
        "```python\n"
        "class tools:\n"
        f"{body}\n"
        "```"
    )
