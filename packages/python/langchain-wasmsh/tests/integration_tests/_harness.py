"""Shared pieces for the `create_deep_agent` composition tests.

Every test in this directory builds a *real* graph through
`create_deep_agent` and runs the path it is about. Instantiating a sandbox
and calling backend methods directly proves nothing about composition: the
filesystem middleware, permission rules, memory and skills assembly, and
subagent stacks all sit between the model and the backend, and each of them
is somewhere a mistake can hide.

The model is scripted rather than real, so the tests are deterministic and
need no API key, but everything below it — tools, middleware, backend,
store, checkpointer — is the genuine article.
"""

from __future__ import annotations

from collections.abc import (
    Iterator,  # noqa: TC003 -- pydantic resolves field annotations at runtime
    Sequence,  # noqa: TC003 -- pydantic resolves field annotations at runtime
)
from dataclasses import dataclass
from typing import Any

import pytest
from langchain_core.language_models.fake_chat_models import GenericFakeChatModel
from langchain_core.messages import AIMessage, BaseMessage, SystemMessage, ToolMessage
from pydantic import Field

try:
    from wasmsh_pyodide_runtime import get_dist_dir

    ASSETS_AVAILABLE = get_dist_dir().joinpath("pyodide.asm.wasm").exists()
except (ImportError, FileNotFoundError):  # pragma: no cover - packaging guard
    ASSETS_AVAILABLE = False

ASSETS_REASON = (
    "Pyodide assets not built (run just build-pyodide && just package-pyodide-runtime)"
)

requires_assets = pytest.mark.skipif(not ASSETS_AVAILABLE, reason=ASSETS_REASON)


@dataclass
class AgentContext:
    """Runtime identity used by the routed `StoreBackend` namespaces."""

    user_id: str = "user-a"
    assistant_id: str = "assistant-a"
    org_id: str = "org-a"


class ScriptedModel(GenericFakeChatModel):
    """A fake chat model that replays a fixed script and records its inputs.

    `bind_tools` returns `self` because the default binding would stop
    reading the scripted iterator. It records the bound tool names and every
    system message it was handed, which is how the prompt-assembly and
    tool-visibility tests observe what the graph actually built.

    The recording lives in a plain dict rather than on the instance: the
    model is a pydantic object with a closed field set.
    """

    messages: Iterator[AIMessage | str] = Field(exclude=True)
    log: dict[str, Any] = Field(default_factory=dict, exclude=True)

    def bind_tools(self, tools: Sequence[Any], **_: Any) -> ScriptedModel:
        self.log["tool_names"] = [getattr(t, "name", t) for t in tools]
        self.log["tools"] = list(tools)
        return self

    def _generate(self, messages: list[BaseMessage], *args: Any, **kwargs: Any) -> Any:
        system = [m for m in messages if isinstance(m, SystemMessage)]
        self.log.setdefault("system_messages", []).append(
            system[-1] if system else None,
        )
        return super()._generate(messages, *args, **kwargs)

    @property
    def tool_names(self) -> list[str]:
        return self.log.get("tool_names", [])

    @property
    def system_messages(self) -> list[SystemMessage | None]:
        return self.log.get("system_messages", [])

    @property
    def system_text(self) -> str:
        """Flatten the last system message into text for substring assertions."""
        message = self.system_messages[-1] if self.system_messages else None
        if message is None:
            return ""
        return system_text(message)


def system_text(message: SystemMessage) -> str:
    """Return a system message's text content, whatever shape it is in."""
    if isinstance(message.content, str):
        return message.content
    return "".join(
        block.get("text", "")
        for block in message.content
        if isinstance(block, dict) and block.get("type") == "text"
    )


def call(name: str, args: dict[str, Any], call_id: str = "call_1") -> AIMessage:
    """Build a one-tool-call assistant turn."""
    return AIMessage(
        content="",
        tool_calls=[
            {"name": name, "args": args, "id": call_id, "type": "tool_call"},
        ],
    )


def script(*messages: AIMessage | str) -> Iterator[AIMessage | str]:
    """Build a model script from assistant turns."""
    return iter(messages)


def tool_messages(result: dict[str, Any], name: str) -> list[ToolMessage]:
    """Return every `ToolMessage` a named tool produced during a run."""
    return [
        m for m in result["messages"] if isinstance(m, ToolMessage) and m.name == name
    ]


def last_tool_message(result: dict[str, Any], name: str) -> ToolMessage:
    """Return the final `ToolMessage` a named tool produced."""
    messages = tool_messages(result, name)
    assert messages, f"expected at least one {name} ToolMessage"
    return messages[-1]


def invoke(agent: Any, text: str = "go", **kwargs: Any) -> dict[str, Any]:
    """Run one user turn against a compiled agent."""
    return agent.invoke({"messages": [{"role": "user", "content": text}]}, **kwargs)


async def ainvoke(agent: Any, text: str = "go", **kwargs: Any) -> dict[str, Any]:
    """Async sibling of :func:`invoke`."""
    return await agent.ainvoke(
        {"messages": [{"role": "user", "content": text}]},
        **kwargs,
    )
