"""Programmatic tool calling (PTC): selection, runtime injection, coercion.

PTC exposes the agent's LangChain tools inside the Python interpreter as
``tools.<snake_name>`` awaitables. The transport that suspends user code
mid-execution and round-trips a call to LangChain lives in
:mod:`langchain_wasmsh.sandbox` (``run_ptc`` + ``_handle_host_call``); the
per-call dispatcher that drives this module lives in
:mod:`langchain_wasmsh._repl`.

A nested PTC call does not travel through LangGraph's ``ToolNode``, so
nothing wires up the context a tool declares via ``ToolRuntime``,
``InjectedState``, ``InjectedStore``, or ``InjectedToolCallId``. This module
replays that wiring — using LangGraph's own ``_get_all_injected_args`` for
detection, so the set of recognised annotations cannot drift — against a
child runtime derived from the ``py_eval`` invocation that started the
program.

Two limits are deliberate and stated in the system prompt and the docs:

- The allowlist is the permission boundary. Nested calls bypass the
  approval-capable execution path, so ``interrupt_on`` never fires for
  them. Gate the outer ``py_eval`` tool instead, expose only tools that are
  safe to call unattended, or leave PTC off.
- A per-evaluation call budget bounds runaway loops in generated code.
"""

from __future__ import annotations

import uuid
from typing import TYPE_CHECKING, Any

from langchain_core.messages import ToolMessage
from langchain_core.tools import BaseTool
from langgraph.types import Command
from pydantic import BaseModel

from langchain_wasmsh import _prompt

if TYPE_CHECKING:
    from collections.abc import Sequence


PTCOption = list[str | BaseTool]

DEFAULT_MAX_PTC_CALLS = 256
"""Default per-evaluation cap on nested tool calls."""

_NATIVE_SCALARS = (bool, int, float, str, type(None))


class PTCCallBudgetExceededError(RuntimeError):
    """Raised when one evaluation exceeds its configured PTC call budget."""

    def __init__(self, *, limit: int, tool_name: str) -> None:
        """Record the limit that was hit and the call that would have exceeded it."""
        self.limit = limit
        self.tool_name = tool_name
        super().__init__(
            f"PTC call budget exceeded (limit={limit}, attempted={limit + 1}, "
            f"tool={tool_name!r}). Restructure the program to make fewer "
            "nested tool calls, or raise `max_ptc_calls`.",
        )


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


# ── runtime injection ──────────────────────────────────────────────────


def synth_tool_call_id(tool_name: str) -> str:
    """Mint a child ``tool_call_id`` for one PTC invocation.

    The real call id belongs to the outer ``py_eval`` tool call. Tools that
    stamp their id into an emitted ``ToolMessage`` (``task`` among them)
    need a non-empty one of their own, and a distinct child id is what lets
    tracing and checkpointed state correlate a nested call back to the
    interpreter cell that issued it.
    """
    return f"ptc_{tool_name}_{uuid.uuid4().hex[:8]}"


def normalize_tool_input(raw: Any) -> dict[str, Any]:
    """Coerce whatever the interpreter passed into ``tools.X(...)`` to a dict.

    Generated code occasionally calls a tool with ``None``, a bare string,
    or a number. Wrapping a scalar under a conventional key routes it into
    the tool's own schema validation, which produces an error the model can
    act on, instead of a silent argument miss.
    """
    if raw is None:
        return {}
    if isinstance(raw, dict):
        return dict(raw)
    return {"input": raw}


def inject_tool_args_for_ptc(
    tool: BaseTool,
    payload: dict[str, Any],
    outer_runtime: Any,
    tool_call_id: str,
) -> dict[str, Any]:
    """Replay ``ToolNode._inject_tool_args`` for one nested PTC call.

    Detection is delegated to LangGraph's own ``_get_all_injected_args`` so
    the recognised annotation set stays in lockstep with the installed
    release. ``InjectedToolCallId`` is handled separately, at the invocation
    site, because it must arrive as a ``tool_call_id`` keyword.

    Args:
        tool: The tool about to be invoked.
        payload: Arguments the interpreter supplied.
        outer_runtime: ``ToolRuntime`` captured from the ``py_eval`` call, or
            `None` when the interpreter ran outside a tool invocation.
        tool_call_id: The freshly minted child id for this call.

    Returns:
        ``payload`` plus the injected arguments. Any interpreter-supplied
            value for an injected key is dropped first, so generated code
            cannot forge state, store, or runtime.
    """
    try:
        from langgraph.prebuilt.tool_node import (  # noqa: PLC0415 -- optional import kept catchable
            _get_all_injected_args,
        )
    except ImportError:  # pragma: no cover -- langgraph is a hard dependency
        return dict(payload)

    injected = _get_all_injected_args(tool)
    if not injected or outer_runtime is None:
        return dict(payload)

    # Strip caller-supplied values for injected keys before adding trusted
    # ones, mirroring ToolNode: the interpreter is model-authored code and
    # must not be able to substitute its own `runtime`/`store`/state.
    enriched = {
        key: value
        for key, value in payload.items()
        if key not in injected.all_injected_keys
    }

    # Derive the child runtime from the outer instance's own type rather
    # than importing ToolRuntime, so added fields travel automatically.
    derived = type(outer_runtime)(
        state=outer_runtime.state,
        context=outer_runtime.context,
        config=outer_runtime.config,
        stream_writer=outer_runtime.stream_writer,
        tool_call_id=tool_call_id,
        store=outer_runtime.store,
        tools=outer_runtime.tools,
        execution_info=getattr(outer_runtime, "execution_info", None),
        server_info=getattr(outer_runtime, "server_info", None),
    )

    if injected.runtime:
        enriched[injected.runtime] = derived
    if injected.state:
        state = outer_runtime.state
        for arg_name, state_field in injected.state.items():
            if not state_field:
                enriched[arg_name] = state
            elif isinstance(state, dict):
                enriched[arg_name] = state.get(state_field)
            else:
                enriched[arg_name] = getattr(state, state_field, None)
    if injected.store and outer_runtime.store is not None:
        enriched[injected.store] = outer_runtime.store
    return enriched


def tool_uses_injected_tool_call_id(tool: BaseTool) -> bool:
    """Return whether ``tool`` declares an ``InjectedToolCallId`` parameter.

    Passing ``tool_call_id`` to ``BaseTool.arun`` makes LangChain wrap the
    result in a ``ToolMessage`` with string-coerced content, which destroys
    native return types. So it is passed only when the tool actually asks
    for it; everything else gets ``None`` and keeps its native value.
    """
    from typing import get_type_hints  # noqa: PLC0415 -- kept local, see below

    from langchain_core.tools.base import (  # noqa: PLC0415
        InjectedToolCallId,
        _is_injected_arg_type,
        get_all_basemodel_annotations,
    )

    try:
        schema_annotations = get_all_basemodel_annotations(tool.get_input_schema())
    except Exception:  # noqa: BLE001 -- schema introspection is best-effort
        schema_annotations = {}
    func = getattr(tool, "func", None) or getattr(tool, "coroutine", None)
    try:
        func_annotations = (
            get_type_hints(func, include_extras=True) if func is not None else {}
        )
    except Exception:  # noqa: BLE001 -- type-hint resolution is best-effort
        func_annotations = {}

    # Same merge order as langgraph: schema annotations win.
    all_annotations = {**func_annotations, **schema_annotations}
    return any(
        _is_injected_arg_type(type_, injected_type=InjectedToolCallId)
        for type_ in all_annotations.values()
    )


# ── result coercion ────────────────────────────────────────────────────


def coerce_tool_output_for_ptc(value: Any) -> Any:
    """Coerce a tool result into a JSON-serialisable shape for the sandbox.

    The wire between host and sandbox is JSON, so the result must reduce to
    primitives, lists, and dicts. What it must *not* do is flatten a
    structured result into ``str(...)`` before that reduction: a tool that
    returns a list of dicts should arrive in the interpreter as a list of
    dicts. ``ToolMessage`` and ``Command`` envelopes are unwrapped to their
    payload (matching the QuickJS interpreter's selection rules) and Pydantic
    models are dumped to their field shape. Only values with no JSON
    counterpart at all — datetimes, custom classes — become strings, and
    only at the leaf where they occur, so the surrounding structure stays
    navigable.
    """
    if isinstance(value, Command):
        return coerce_tool_output_for_ptc(_extract_command_content(value))
    if isinstance(value, ToolMessage):
        return coerce_tool_output_for_ptc(value.content)
    if isinstance(value, list):
        for entry in reversed(value):
            if isinstance(entry, ToolMessage):
                return coerce_tool_output_for_ptc(entry.content)
            if isinstance(entry, Command):
                return coerce_tool_output_for_ptc(_extract_command_content(entry))
    return _coerce_for_json(value)


def _coerce_for_json(value: Any) -> Any:
    if isinstance(value, _NATIVE_SCALARS):
        return value
    if isinstance(value, dict):
        return {str(k): _coerce_for_json(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_coerce_for_json(v) for v in value]
    if isinstance(value, BaseModel):
        return _coerce_for_json(value.model_dump())
    return str(value)


def _extract_command_content(command: Command) -> Any:
    """Return the trailing message content from a ``Command`` update, if any."""
    update = command.update
    if isinstance(update, dict):
        messages = update.get("messages")
        if isinstance(messages, list):
            for entry in reversed(messages):
                content = getattr(entry, "content", None)
                if content is not None:
                    return content
    return str(update)
