"""System-prompt renderers for the Python REPL interpreter middleware."""

from __future__ import annotations

import re
from typing import TYPE_CHECKING, Any, Literal

from langchain_core.messages import SystemMessage

if TYPE_CHECKING:
    from collections.abc import Sequence

    from langchain_core.tools import BaseTool

PersistenceMode = Literal["thread", "turn", "call"]
"""How long one interpreter session outlives the call that created it."""

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


_STATE_PERSISTENCE_LINES: dict[str, str] = {
    "thread": (
        "- State (variables, imports, defined functions) persists across "
        "tool calls and across multiple turns for this conversation thread."
    ),
    "turn": (
        "- State persists across tool calls within a single turn of "
        "conversation. It DOES NOT persist across multiple turns."
    ),
    "call": (
        "- State does NOT persist: every call runs in a fresh interpreter. "
        "Write self-contained code, or keep intermediate results in files "
        "under `/workspace`."
    ),
}


def render_repl_system_prompt(
    *,
    tool_name: str,
    timeout: float,
    max_result_chars: int,
    mode: PersistenceMode,
) -> str:
    """Render the base REPL system prompt text for ``WasmshInterpreterMiddleware``."""
    state_persistence_line = _STATE_PERSISTENCE_LINES[mode]
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


def append_system_prompt_block(
    system_message: SystemMessage | None,
    text: str,
) -> SystemMessage:
    """Append ``text`` to ``system_message`` as one additional text block.

    Deliberately mirrors Deep Agents' own `append_to_system_message`, down to
    the blank line inserted before the appended text when prior content
    exists, rather than importing it: the adapter supports a `<0.8` window,
    and depending on a private helper would trade one version-skew risk for
    another. `tests/unit_tests/test_prompt_blocks.py` asserts parity against
    the installed release, so a divergence fails loudly.

    Two things this does that a naive rebuild of `SystemMessage.content`
    would not:

    - Existing content blocks are carried over **as they are**, so
      provider-specific keys survive — most importantly `cache_control`,
      which prompt caching writes onto the block that memory, skills, and
      harness-profile middleware assembled. Flattening the message into one
      string would silently drop it and re-bill the whole prefix.
    - Message-level metadata (`additional_kwargs`, `response_metadata`,
      `name`, `id`) is preserved, which upstream's helper does not do.

    Args:
        system_message: Existing system message, or `None` when this
            middleware is the first to contribute one.
        text: The interpreter prompt fragment to append.

    Returns:
        A new `SystemMessage`; the input is never mutated.
    """
    if system_message is None:
        return SystemMessage(content_blocks=[{"type": "text", "text": text}])

    blocks: list[Any] = list(system_message.content_blocks)
    if blocks:
        text = f"\n\n{text}"
    blocks.append({"type": "text", "text": text})

    preserved: dict[str, Any] = {}
    for field in ("additional_kwargs", "response_metadata", "name", "id"):
        value = getattr(system_message, field, None)
        if value:
            preserved[field] = value
    return SystemMessage(content_blocks=blocks, **preserved)
