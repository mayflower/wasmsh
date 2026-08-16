"""Durable memory, user profiles, and long-term store scopes.

The wasmsh VFS is an execution workspace, not durable memory: it dies with
its host process. So everything that must outlive a session is routed to
upstream `StoreBackend` through a `CompositeBackend`, and these tests check
the routing, the scoping, and — just as importantly — the *isolation*
between users, assistants, and organizations.

They also pin upstream's reload contract rather than a wished-for one:
`MemoryMiddleware` loads its sources only when private state lacks
`memory_contents`, so a checkpointed thread keeps the view it started with
and a fresh thread reloads. That is deliberate upstream behaviour and this
adapter does not add a watcher to hide it.
"""

from __future__ import annotations

from typing import Any

import pytest
from deepagents import create_deep_agent
from deepagents.backends.composite import CompositeBackend
from deepagents.backends.store import StoreBackend
from langchain_core.messages import AIMessage
from langgraph.checkpoint.memory import InMemorySaver
from langgraph.store.memory import InMemoryStore

from langchain_wasmsh import WasmshSandbox
from tests.integration_tests._harness import (
    AgentContext,
    ScriptedModel,
    call,
    invoke,
    requires_assets,
    script,
)

pytestmark = requires_assets

MEMORY_SOURCES = [
    "/profiles/user.md",
    "/memories/user/AGENTS.md",
    "/memories/agent/AGENTS.md",
    "/policies/AGENTS.md",
]


def routed_backend(sandbox: Any, store: InMemoryStore) -> CompositeBackend:
    """The canonical composition: wasmsh executes, the store remembers."""
    return CompositeBackend(
        default=sandbox,
        routes={
            "/profiles/": StoreBackend(
                store=store,
                namespace=lambda rt: ("deepagents", "profile", rt.context.user_id),
            ),
            "/memories/user/": StoreBackend(
                store=store,
                namespace=lambda rt: ("deepagents", "memory-user", rt.context.user_id),
            ),
            "/memories/agent/": StoreBackend(
                store=store,
                namespace=lambda rt: (
                    "deepagents",
                    "memory-agent",
                    rt.context.assistant_id,
                ),
            ),
            "/policies/": StoreBackend(
                store=store,
                namespace=lambda rt: ("deepagents", "policy", rt.context.org_id),
            ),
        },
    )


def build_agent(  # noqa: PLR0913 -- one builder for every composition variant
    sandbox: Any,
    store: InMemoryStore,
    *,
    checkpointer: Any = None,
    memory: list[str] | None = None,
    permissions: list[Any] | None = None,
    messages: Any = None,
) -> tuple[ScriptedModel, Any]:
    model = ScriptedModel(messages=messages or script(AIMessage(content="done")))
    agent = create_deep_agent(
        model=model,
        backend=routed_backend(sandbox, store),
        context_schema=AgentContext,
        checkpointer=checkpointer,
        store=store,
        memory=memory if memory is not None else MEMORY_SOURCES,
        permissions=permissions,
    )
    return model, agent


@pytest.fixture
def store() -> InMemoryStore:
    return InMemoryStore()


class TestMemoryAssembly:
    def test_ordered_sources_all_appear_in_the_prompt(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        context = AgentContext()
        backend = routed_backend(shared_sandbox, store)
        writer_model = ScriptedModel(
            messages=script(
                call(
                    "write_file",
                    {"file_path": "/profiles/user.md", "content": "PREFERS-METRIC"},
                    "c1",
                ),
                call(
                    "write_file",
                    {
                        "file_path": "/memories/user/AGENTS.md",
                        "content": "USER-MEMORY",
                    },
                    "c2",
                ),
                call(
                    "write_file",
                    {
                        "file_path": "/policies/AGENTS.md",
                        "content": "ORG-POLICY",
                    },
                    "c3",
                ),
                AIMessage(content="seeded"),
            ),
        )
        invoke(
            create_deep_agent(
                model=writer_model,
                backend=backend,
                context_schema=AgentContext,
                store=store,
                memory=[],
            ),
            context=context,
        )

        reader = ScriptedModel(messages=script(AIMessage(content="done")))
        invoke(
            create_deep_agent(
                model=reader,
                backend=backend,
                context_schema=AgentContext,
                store=store,
                memory=MEMORY_SOURCES,
            ),
            context=context,
        )

        text = reader.system_text
        assert "PREFERS-METRIC" in text
        assert "USER-MEMORY" in text
        assert "ORG-POLICY" in text
        # Source order is the caller's list order.
        assert text.index("PREFERS-METRIC") < text.index("USER-MEMORY")
        assert text.index("USER-MEMORY") < text.index("ORG-POLICY")

    def test_missing_sources_are_skipped_without_failing(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        _, agent = build_agent(shared_sandbox, store)
        assert invoke(agent, context=AgentContext())["messages"][-1].content == "done"


class TestPersistenceAcrossThreadsAndSandboxes:
    def test_a_user_preference_survives_a_new_thread_and_a_new_sandbox(
        self,
        store: InMemoryStore,
    ) -> None:
        context = AgentContext(user_id="u-1")

        first_sandbox = WasmshSandbox()
        try:
            _writer, agent = build_agent(
                first_sandbox,
                store,
                memory=[],
                messages=script(
                    call(
                        "write_file",
                        {
                            "file_path": "/memories/user/AGENTS.md",
                            "content": "LIKES-SHORT-ANSWERS",
                        },
                    ),
                    AIMessage(content="ok"),
                ),
            )
            invoke(agent, context=context)
        finally:
            first_sandbox.close()

        # New sandbox process, new thread, same store: the preference is
        # still there because it never lived in the VFS.
        second_sandbox = WasmshSandbox()
        try:
            reader, reader_agent = build_agent(second_sandbox, store)
            invoke(reader_agent, context=context)
            assert "LIKES-SHORT-ANSWERS" in reader.system_text
        finally:
            second_sandbox.close()

    def test_another_user_does_not_see_it(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        _, writer = build_agent(
            shared_sandbox,
            store,
            memory=[],
            messages=script(
                call(
                    "write_file",
                    {
                        "file_path": "/memories/user/AGENTS.md",
                        "content": "USER-ONE-SECRET",
                    },
                ),
                AIMessage(content="ok"),
            ),
        )
        invoke(writer, context=AgentContext(user_id="u-1"))

        other, other_agent = build_agent(shared_sandbox, store)
        invoke(other_agent, context=AgentContext(user_id="u-2"))
        assert "USER-ONE-SECRET" not in other.system_text

    def test_agent_memory_is_shared_across_users_of_one_assistant(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        _, writer = build_agent(
            shared_sandbox,
            store,
            memory=[],
            messages=script(
                call(
                    "write_file",
                    {
                        "file_path": "/memories/agent/AGENTS.md",
                        "content": "SHARED-AGENT-KNOWLEDGE",
                    },
                ),
                AIMessage(content="ok"),
            ),
        )
        invoke(writer, context=AgentContext(user_id="u-1", assistant_id="a-1"))

        same_assistant, agent = build_agent(shared_sandbox, store)
        invoke(agent, context=AgentContext(user_id="u-2", assistant_id="a-1"))
        assert "SHARED-AGENT-KNOWLEDGE" in same_assistant.system_text

        other_assistant, other = build_agent(shared_sandbox, store)
        invoke(other, context=AgentContext(user_id="u-1", assistant_id="a-2"))
        assert "SHARED-AGENT-KNOWLEDGE" not in other_assistant.system_text

    def test_org_policy_is_scoped_to_the_organization(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        _, writer = build_agent(
            shared_sandbox,
            store,
            memory=[],
            messages=script(
                call(
                    "write_file",
                    {"file_path": "/policies/AGENTS.md", "content": "ORG-ONE-RULE"},
                ),
                AIMessage(content="ok"),
            ),
        )
        invoke(writer, context=AgentContext(org_id="org-1"))

        inside, inside_agent = build_agent(shared_sandbox, store)
        invoke(inside_agent, context=AgentContext(user_id="u-9", org_id="org-1"))
        assert "ORG-ONE-RULE" in inside.system_text

        outside, outside_agent = build_agent(shared_sandbox, store)
        invoke(outside_agent, context=AgentContext(org_id="org-2"))
        assert "ORG-ONE-RULE" not in outside.system_text


class TestUpstreamReloadSemantics:
    """Exact 0.7.4 behaviour: cached per thread, reloaded per new thread."""

    def test_a_resumed_thread_keeps_its_original_memory_view(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        context = AgentContext(user_id="reload-user")
        backend = routed_backend(shared_sandbox, store)
        saver = InMemorySaver()

        def run(model: ScriptedModel, thread: str) -> None:
            invoke(
                create_deep_agent(
                    model=model,
                    backend=backend,
                    context_schema=AgentContext,
                    checkpointer=saver,
                    store=store,
                    memory=["/memories/user/AGENTS.md"],
                ),
                context=context,
                config={"configurable": {"thread_id": thread}},
            )

        def write(content: str) -> None:
            invoke(
                create_deep_agent(
                    model=ScriptedModel(
                        messages=script(
                            call(
                                "write_file",
                                {
                                    "file_path": "/memories/user/AGENTS.md",
                                    "content": content,
                                },
                            ),
                            AIMessage(content="ok"),
                        ),
                    ),
                    backend=backend,
                    context_schema=AgentContext,
                    store=store,
                    memory=[],
                ),
                context=context,
            )

        write("VERSION-ONE")
        first = ScriptedModel(
            messages=script(AIMessage(content="a1"), AIMessage(content="a2")),
        )
        run(first, "thread-a")
        assert "VERSION-ONE" in first.system_text

        write("VERSION-TWO")

        # Same thread: upstream keeps `memory_contents` in private state, so
        # the running thread is not entitled to a refresh.
        run(first, "thread-a")
        assert "VERSION-ONE" in first.system_text
        assert "VERSION-TWO" not in first.system_text

        # Fresh thread: reloads and sees the update.
        second = ScriptedModel(messages=script(AIMessage(content="b1")))
        run(second, "thread-b")
        assert "VERSION-TWO" in second.system_text

    async def test_async_loading_produces_the_same_prompt(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        context = AgentContext(user_id="async-user")
        backend = routed_backend(shared_sandbox, store)
        invoke(
            create_deep_agent(
                model=ScriptedModel(
                    messages=script(
                        call(
                            "write_file",
                            {
                                "file_path": "/memories/user/AGENTS.md",
                                "content": "ASYNC-PARITY",
                            },
                        ),
                        AIMessage(content="ok"),
                    ),
                ),
                backend=backend,
                context_schema=AgentContext,
                store=store,
                memory=[],
            ),
            context=context,
        )

        def build() -> ScriptedModel:
            return ScriptedModel(messages=script(AIMessage(content="done")))

        sync_model = build()
        invoke(
            create_deep_agent(
                model=sync_model,
                backend=backend,
                context_schema=AgentContext,
                store=store,
                memory=["/memories/user/AGENTS.md"],
            ),
            context=context,
        )
        async_model = build()
        await create_deep_agent(
            model=async_model,
            backend=backend,
            context_schema=AgentContext,
            store=store,
            memory=["/memories/user/AGENTS.md"],
        ).ainvoke(
            {"messages": [{"role": "user", "content": "go"}]},
            context=context,
        )
        assert sync_model.system_text == async_model.system_text
