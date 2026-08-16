"""Upstream Agent Skills composed with wasmsh, plus the Python bridge.

Skill discovery, `SKILL.md` parsing, source precedence, and progressive
disclosure all belong to upstream `SkillsMiddleware`. What is tested here is
that they work when the backend is wasmsh, and that the optional
`import skills.<name>` bridge sits on top without changing any of it.
"""

from __future__ import annotations

import textwrap
from typing import Any

import pytest
from deepagents import create_deep_agent
from deepagents.backends.composite import CompositeBackend
from deepagents.backends.store import StoreBackend
from langchain_core.messages import AIMessage
from langgraph.store.memory import InMemoryStore

from tests.integration_tests._harness import (
    AgentContext,
    ScriptedModel,
    call,
    invoke,
    last_tool_message,
    requires_assets,
    script,
)

pytestmark = requires_assets


def skill_md(name: str, description: str, **extra: str) -> bytes:
    metadata = ""
    if extra:
        lines = "\n".join(f"  {k}: {v}" for k, v in extra.items())
        metadata = f"metadata:\n{lines}\n"
    return textwrap.dedent(
        f"""\
        ---
        name: {name}
        description: {description}
        {metadata}---

        # {name}

        Instructions for {name}.
        """,
    ).encode()


@pytest.fixture
def store() -> InMemoryStore:
    return InMemoryStore()


def routed(sandbox: Any, store: InMemoryStore) -> CompositeBackend:
    namespace = ("deepagents", "skills")
    return CompositeBackend(
        default=sandbox,
        routes={
            "/skills/agent/": StoreBackend(
                store=store,
                namespace=lambda _rt: (*namespace, "agent"),
            ),
            "/skills/user/": StoreBackend(
                store=store,
                namespace=lambda _rt: (*namespace, "user"),
            ),
        },
    )


def seed(backend: Any, store: InMemoryStore, files: dict[str, bytes]) -> None:
    model = ScriptedModel(
        messages=script(
            *[
                call(
                    "write_file",
                    {"file_path": path, "content": content.decode()},
                    f"c{i}",
                )
                for i, (path, content) in enumerate(files.items())
            ],
            AIMessage(content="seeded"),
        ),
    )
    agent = create_deep_agent(
        model=model,
        backend=backend,
        context_schema=AgentContext,
        store=store,
    )
    invoke(agent, context=AgentContext())


class TestSkillDiscovery:
    def test_skill_metadata_reaches_the_system_prompt(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        backend = routed(shared_sandbox, store)
        seed(
            backend,
            store,
            {
                "/skills/agent/report/SKILL.md": skill_md(
                    "report",
                    "Build the weekly report",
                ),
            },
        )
        model = ScriptedModel(messages=script(AIMessage(content="done")))
        invoke(
            create_deep_agent(
                model=model,
                backend=backend,
                context_schema=AgentContext,
                store=store,
                skills=["/skills/agent/"],
            ),
            context=AgentContext(),
        )
        text = model.system_text
        assert "report" in text
        # Progressive disclosure: the description is in the prompt, the body
        # is not — the agent reads SKILL.md when it decides the skill applies.
        assert "Build the weekly report" in text
        assert "Instructions for report" not in text

    def test_a_later_source_overrides_an_earlier_skill_of_the_same_name(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        backend = routed(shared_sandbox, store)
        seed(
            backend,
            store,
            {
                "/skills/agent/report/SKILL.md": skill_md(
                    "report",
                    "AGENT-SCOPED-DESCRIPTION",
                ),
                "/skills/user/report/SKILL.md": skill_md(
                    "report",
                    "USER-SCOPED-DESCRIPTION",
                ),
            },
        )
        model = ScriptedModel(messages=script(AIMessage(content="done")))
        invoke(
            create_deep_agent(
                model=model,
                backend=backend,
                context_schema=AgentContext,
                store=store,
                # Later sources win on a name collision.
                skills=["/skills/agent/", "/skills/user/"],
            ),
            context=AgentContext(),
        )
        text = model.system_text
        assert "USER-SCOPED-DESCRIPTION" in text
        assert "AGENT-SCOPED-DESCRIPTION" not in text

    def test_the_agent_can_read_a_skills_body_and_its_assets(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        backend = routed(shared_sandbox, store)
        seed(
            backend,
            store,
            {
                "/skills/agent/report/SKILL.md": skill_md("report", "Report skill"),
                "/skills/agent/report/scripts/run.sh": b"#!/bin/sh\necho generated\n",
            },
        )
        model = ScriptedModel(
            messages=script(
                call(
                    "read_file",
                    {"file_path": "/skills/agent/report/SKILL.md", "limit": 1000},
                    "c1",
                ),
                call(
                    "read_file",
                    {"file_path": "/skills/agent/report/scripts/run.sh"},
                    "c2",
                ),
                AIMessage(content="done"),
            ),
        )
        result = invoke(
            create_deep_agent(
                model=model,
                backend=backend,
                context_schema=AgentContext,
                store=store,
                skills=["/skills/agent/"],
            ),
            context=AgentContext(),
        )
        reads = [
            m.content
            for m in result["messages"]
            if getattr(m, "name", None) == "read_file"
        ]
        assert any("Instructions for report" in c for c in reads)
        assert any("echo generated" in c for c in reads)

    def test_a_malformed_skill_does_not_break_the_run(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        backend = routed(shared_sandbox, store)
        seed(
            backend,
            store,
            {
                "/skills/agent/broken/SKILL.md": b"no frontmatter at all\n",
                "/skills/agent/good/SKILL.md": skill_md("good", "A working skill"),
            },
        )
        model = ScriptedModel(messages=script(AIMessage(content="done")))
        result = invoke(
            create_deep_agent(
                model=model,
                backend=backend,
                context_schema=AgentContext,
                store=store,
                skills=["/skills/agent/"],
            ),
            context=AgentContext(),
        )
        assert result["messages"][-1].content == "done"
        assert "A working skill" in model.system_text

    def test_skills_compose_with_memory_and_the_execute_backend(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        backend = routed(shared_sandbox, store)
        seed(
            backend,
            store,
            {"/skills/agent/report/SKILL.md": skill_md("report", "Report skill")},
        )
        shared_sandbox.upload_files([("/workspace/mem/a.md", b"AGENT-MEMORY\n")])
        model = ScriptedModel(
            messages=script(
                call("execute", {"command": "echo from-shell"}),
                AIMessage(content="done"),
            ),
        )
        result = invoke(
            create_deep_agent(
                model=model,
                backend=backend,
                context_schema=AgentContext,
                store=store,
                skills=["/skills/agent/"],
                memory=["/workspace/mem/a.md"],
            ),
            context=AgentContext(),
        )
        text = model.system_text
        assert "Report skill" in text
        assert "AGENT-MEMORY" in text
        assert "from-shell" in last_tool_message(result, "execute").content

    async def test_async_skill_loading_matches_sync(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        backend = routed(shared_sandbox, store)
        seed(
            backend,
            store,
            {"/skills/agent/report/SKILL.md": skill_md("report", "Report skill")},
        )

        def build() -> ScriptedModel:
            return ScriptedModel(messages=script(AIMessage(content="done")))

        def graph(model: ScriptedModel) -> Any:
            return create_deep_agent(
                model=model,
                backend=backend,
                context_schema=AgentContext,
                store=store,
                skills=["/skills/agent/"],
            )

        sync_model = build()
        invoke(graph(sync_model), context=AgentContext())
        async_model = build()
        await graph(async_model).ainvoke(
            {"messages": [{"role": "user", "content": "go"}]},
            context=AgentContext(),
        )
        assert sync_model.system_text == async_model.system_text


class TestSubagentSkillIsolation:
    def test_a_declarative_subagent_sees_only_its_own_skill_sources(
        self,
        shared_sandbox: Any,
        store: InMemoryStore,
    ) -> None:
        backend = routed(shared_sandbox, store)
        seed(
            backend,
            store,
            {
                "/skills/agent/parent-skill/SKILL.md": skill_md(
                    "parent-skill",
                    "PARENT-SKILL-DESCRIPTION",
                ),
                "/skills/user/sub-skill/SKILL.md": skill_md(
                    "sub-skill",
                    "SUB-SKILL-DESCRIPTION",
                ),
            },
        )
        sub_model = ScriptedModel(messages=script(AIMessage(content="sub done")))
        parent = ScriptedModel(
            messages=script(
                call("task", {"subagent_type": "narrow", "description": "go"}),
                AIMessage(content="done"),
            ),
        )
        invoke(
            create_deep_agent(
                model=parent,
                backend=backend,
                context_schema=AgentContext,
                store=store,
                skills=["/skills/agent/"],
                subagents=[
                    {
                        "name": "narrow",
                        "description": "own skills only",
                        "system_prompt": "sub",
                        "model": sub_model,
                        "skills": ["/skills/user/"],
                    },
                ],
            ),
            context=AgentContext(),
        )
        assert "PARENT-SKILL-DESCRIPTION" in parent.system_text
        # Top-level skills are not a blanket inheritance contract.
        assert "SUB-SKILL-DESCRIPTION" in sub_model.system_text
        assert "PARENT-SKILL-DESCRIPTION" not in sub_model.system_text
