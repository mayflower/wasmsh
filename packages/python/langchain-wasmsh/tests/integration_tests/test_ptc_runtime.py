"""Programmatic tool calling against a real Pyodide sandbox.

The unit tests cover the dispatcher in isolation. These run the whole path:
a `create_deep_agent` graph, a `py_eval` call whose Python source awaits
`tools.<name>(...)`, the sandbox suspending mid-program, the host resolving
the call against the real LangChain tool with an injected child runtime, and
the value travelling back into the running program.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

import pytest
from deepagents import create_deep_agent
from langchain.tools import (
    ToolRuntime,  # noqa: TC002 -- needed at runtime for ToolNode's injection scan
)
from langchain_core.messages import AIMessage
from langchain_core.tools import tool
from langgraph.store.memory import InMemoryStore

from langchain_wasmsh import WasmshInterpreterMiddleware, WasmshSandbox
from tests.integration_tests._harness import (
    ScriptedModel,
    call,
    invoke,
    last_tool_message,
    requires_assets,
    script,
)

pytestmark = requires_assets


@pytest.fixture(autouse=True)
def _requires_host_call(interpreter_sandbox: Any) -> None:
    """Skip when the installed host predates the PTC protocol.

    `host_call` arrived in `wasmsh-pyodide-runtime` 0.7.0. The package floor
    requires it, but a stale environment would otherwise fail these tests
    with a protocol error that looks like an adapter bug.
    """
    if "host_call" not in interpreter_sandbox.host_capabilities():
        pytest.skip("installed wasmsh host does not advertise host_call")


@dataclass
class Identity:
    """Runtime context the nested tools read."""

    user_id: str


@pytest.fixture
def interpreter_sandbox() -> Any:
    sandbox = WasmshSandbox()
    try:
        yield sandbox
    finally:
        sandbox.close()


def agent_with(
    sandbox: Any,
    tools: list[Any],
    code: str,
    *,
    store: Any = None,
    max_ptc_calls: int | None = 256,
) -> tuple[ScriptedModel, Any]:
    model = ScriptedModel(
        messages=script(call("py_eval", {"code": code}), AIMessage(content="done")),
    )
    agent = create_deep_agent(
        model=model,
        backend=sandbox,
        tools=tools,
        context_schema=Identity,
        store=store,
        middleware=[
            WasmshInterpreterMiddleware(
                sandbox_factory=lambda: sandbox,
                ptc=[t.name for t in tools],
                max_ptc_calls=max_ptc_calls,
                mode="turn",
            ),
        ],
    )
    return model, agent


class TestProgrammaticToolCalling:
    def test_a_nested_call_returns_its_value_into_the_program(
        self,
        interpreter_sandbox: Any,
    ) -> None:
        @tool
        def lookup(city: str) -> str:
            """Look up a city code."""
            return f"code-for-{city}"

        _model, agent = agent_with(
            interpreter_sandbox,
            [lookup],
            "result = await tools.lookup(city='berlin')\nprint(result)\nresult",
        )
        result = invoke(agent, context=Identity(user_id="u1"))
        assert "code-for-berlin" in last_tool_message(result, "py_eval").content

    def test_the_nested_tool_receives_the_runtime_context(
        self,
        interpreter_sandbox: Any,
    ) -> None:
        @tool
        def whoami(runtime: ToolRuntime) -> str:
            """Report the caller identity."""
            return runtime.context.user_id

        _, agent = agent_with(
            interpreter_sandbox,
            [whoami],
            "who = await tools.whoami()\nprint(who)\nwho",
        )
        result = invoke(agent, context=Identity(user_id="carla"))
        # The identity crossed the sandbox boundary and came back — the old
        # raw `tool.invoke(args)` bridge could not do this at all.
        assert "carla" in last_tool_message(result, "py_eval").content

    def test_the_nested_tool_receives_the_store(
        self,
        interpreter_sandbox: Any,
    ) -> None:
        store = InMemoryStore()
        store.put(("facts",), "answer", {"value": 42})

        @tool
        def recall(runtime: ToolRuntime) -> int:
            """Read a fact from the store."""
            return runtime.store.get(("facts",), "answer").value["value"]

        _, agent = agent_with(
            interpreter_sandbox,
            [recall],
            "v = await tools.recall()\nprint(v)\nv",
            store=store,
        )
        result = invoke(agent, context=Identity(user_id="u1"))
        assert "42" in last_tool_message(result, "py_eval").content

    def test_structured_results_arrive_as_structured_data(
        self,
        interpreter_sandbox: Any,
    ) -> None:
        @tool
        def rows() -> list[dict[str, Any]]:
            """Return tabular data."""
            return [{"name": "a", "n": 1}, {"name": "b", "n": 2}]

        _, agent = agent_with(
            interpreter_sandbox,
            [rows],
            # Indexing and arithmetic only work if the list survived as a
            # list of dicts rather than being stringified on the way in.
            "data = await tools.rows()\ntotal = sum(r['n'] for r in data)\ntotal",
        )
        result = invoke(agent, context=Identity(user_id="u1"))
        assert "3" in last_tool_message(result, "py_eval").content

    def test_several_calls_in_one_program_all_run(
        self,
        interpreter_sandbox: Any,
    ) -> None:
        @tool
        def double(n: int) -> int:
            """Double a number."""
            return n * 2

        _, agent = agent_with(
            interpreter_sandbox,
            [double],
            "vals = [await tools.double(n=i) for i in range(3)]\nvals",
        )
        result = invoke(agent, context=Identity(user_id="u1"))
        content = last_tool_message(result, "py_eval").content
        assert "0" in content
        assert "4" in content

    def test_the_call_budget_stops_a_runaway_program(
        self,
        interpreter_sandbox: Any,
    ) -> None:
        @tool
        def ping() -> str:
            """Cheap tool."""
            return "pong"

        _, agent = agent_with(
            interpreter_sandbox,
            [ping],
            "for _ in range(10):\n    await tools.ping()\n",
            max_ptc_calls=3,
        )
        result = invoke(agent, context=Identity(user_id="u1"))
        content = last_tool_message(result, "py_eval").content
        assert "budget" in content.lower()

    def test_an_unknown_tool_is_reported_to_the_program(
        self,
        interpreter_sandbox: Any,
    ) -> None:
        @tool
        def known() -> str:
            """The only exposed tool."""
            return "ok"

        _, agent = agent_with(
            interpreter_sandbox,
            [known],
            "try:\n    await tools.ghost()\nexcept Exception as exc:\n"
            "    print('caught', type(exc).__name__)\n",
        )
        result = invoke(agent, context=Identity(user_id="u1"))
        assert "caught" in last_tool_message(result, "py_eval").content

    def test_the_interpreter_never_exposes_itself_for_recursion(
        self,
        interpreter_sandbox: Any,
    ) -> None:
        @tool
        def known() -> str:
            """The only exposed tool."""
            return "ok"

        model = ScriptedModel(
            messages=script(
                call("py_eval", {"code": "print(sorted(dir(tools)))"}),
                AIMessage(content="done"),
            ),
        )
        agent = create_deep_agent(
            model=model,
            backend=interpreter_sandbox,
            tools=[known],
            middleware=[
                WasmshInterpreterMiddleware(
                    sandbox_factory=lambda: interpreter_sandbox,
                    # Naming the interpreter's own tool must not expose it.
                    ptc=["known", "py_eval"],
                    mode="turn",
                ),
            ],
        )
        content = last_tool_message(invoke(agent), "py_eval").content
        assert "known" in content
        assert "py_eval" not in content

    async def test_the_async_agent_path_runs_nested_tools(
        self,
        interpreter_sandbox: Any,
    ) -> None:
        @tool
        async def fetch(key: str) -> str:
            """An async-only nested tool."""
            return f"fetched:{key}"

        _, agent = agent_with(
            interpreter_sandbox,
            [fetch],
            "v = await tools.fetch(key='x')\nprint(v)\nv",
        )
        result = await agent.ainvoke(
            {"messages": [{"role": "user", "content": "go"}]},
            context=Identity(user_id="u1"),
        )
        assert "fetched:x" in last_tool_message(result, "py_eval").content
