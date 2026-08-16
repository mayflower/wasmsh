"""Harness and provider profile composition, with no network and no model.

Both registries are process-global and additive: `register_harness_profile`
mutates module state that outlives the test. So every test registers under a
key unique to itself and restores the registry afterwards, rather than
reaching for a production reset API that would exist only for tests.

Harness profiles are resolved from the model *spec*, so these tests pass a
`"provider:model"` string and monkeypatch model resolution to hand back a
scripted fake — which exercises the real lookup path without a paid model.
"""

from __future__ import annotations

import uuid
from collections.abc import (
    Iterator,  # noqa: TC003 -- pydantic resolves field annotations at runtime
)
from typing import Any

import pytest
from deepagents import (
    GeneralPurposeSubagentProfile,
    HarnessProfile,
    HarnessProfileConfig,
    ProviderProfile,
    create_deep_agent,
    register_harness_profile,
    register_provider_profile,
)
from deepagents import graph as deepagents_graph
from deepagents.backends.state import StateBackend
from deepagents.middleware.filesystem import FilesystemMiddleware
from deepagents.profiles.harness import harness_profiles
from deepagents.profiles.provider import provider_profiles
from langchain.agents.middleware.types import AgentMiddleware
from langchain_core.language_models.fake_chat_models import GenericFakeChatModel
from langchain_core.messages import AIMessage, SystemMessage
from pydantic import Field

from langchain_wasmsh import WasmshInterpreterMiddleware
from langchain_wasmsh._prompt import append_system_prompt_block


class _Model(GenericFakeChatModel):
    """Minimal scripted model; `bind_tools` must return `self`."""

    messages: Iterator[AIMessage | str] = Field(exclude=True)
    log: dict[str, Any] = Field(default_factory=dict, exclude=True)

    def bind_tools(self, tools: Any, **_: Any) -> _Model:
        self.log["tool_names"] = [getattr(t, "name", t) for t in tools]
        self.log["tools"] = list(tools)
        return self

    def _generate(self, messages: list[Any], *args: Any, **kwargs: Any) -> Any:
        system = [m for m in messages if isinstance(m, SystemMessage)]
        self.log["system"] = system[-1] if system else None
        return super()._generate(messages, *args, **kwargs)

    @property
    def system_text(self) -> str:
        message = self.log.get("system")
        if message is None:
            return ""
        if isinstance(message.content, str):
            return message.content
        return "".join(
            block.get("text", "")
            for block in message.content
            if isinstance(block, dict)
        )


@pytest.fixture
def isolated_registries() -> Iterator[None]:
    """Snapshot and restore both process-global profile registries."""
    harness_snapshot = dict(harness_profiles._HARNESS_PROFILES)
    provider_snapshot = dict(provider_profiles._PROVIDER_PROFILES)
    try:
        yield
    finally:
        harness_profiles._HARNESS_PROFILES.clear()
        harness_profiles._HARNESS_PROFILES.update(harness_snapshot)
        provider_profiles._PROVIDER_PROFILES.clear()
        provider_profiles._PROVIDER_PROFILES.update(provider_snapshot)


@pytest.fixture
def provider() -> str:
    """A registry key no other test can collide with."""
    return f"wasmshtest{uuid.uuid4().hex[:8]}"


@pytest.fixture
def scripted(monkeypatch: pytest.MonkeyPatch) -> Any:
    """Build an agent from a model *string* while returning a fake model.

    The string is what the harness-profile registry is keyed on, so this is
    the only way to exercise profile resolution end to end without a real
    provider.
    """

    def build(spec: str, **kwargs: Any) -> tuple[_Model, Any]:
        model = _Model(messages=iter([AIMessage(content="done")]))
        monkeypatch.setattr(
            deepagents_graph,
            "resolve_model",
            lambda _spec: model,
        )
        agent = create_deep_agent(model=spec, backend=StateBackend(), **kwargs)
        return model, agent

    return build


def run(agent: Any) -> dict[str, Any]:
    return agent.invoke({"messages": [{"role": "user", "content": "go"}]})


class _MarkerMiddleware(AgentMiddleware):
    """Appends a marker so profile `extra_middleware` placement is visible."""

    def __init__(self, marker: str) -> None:
        super().__init__()
        self.marker = marker

    def wrap_model_call(self, request: Any, handler: Any) -> Any:
        return handler(
            request.override(
                system_message=append_system_prompt_block(
                    request.system_message,
                    self.marker,
                ),
            ),
        )


# ── registration and lookup ────────────────────────────────────────────


class TestHarnessProfileRegistration:
    def test_provider_wide_and_exact_model_keys_both_resolve(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
    ) -> None:
        register_harness_profile(
            provider,
            HarnessProfile(base_system_prompt="PROVIDER-WIDE"),
        )
        register_harness_profile(
            f"{provider}:special",
            HarnessProfile(base_system_prompt="EXACT-MODEL"),
        )
        resolve = harness_profiles._get_harness_profile
        assert resolve(f"{provider}:other").base_system_prompt == "PROVIDER-WIDE"
        assert resolve(f"{provider}:special").base_system_prompt == "EXACT-MODEL"

    def test_registrations_layer_additively(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
    ) -> None:
        # Registering the same key twice merges rather than replacing, which
        # is why tests must isolate the registry instead of clearing it.
        register_harness_profile(
            provider,
            HarnessProfile(base_system_prompt="BASE"),
        )
        register_harness_profile(
            provider,
            HarnessProfile(system_prompt_suffix="SUFFIX"),
        )
        merged = harness_profiles._get_harness_profile(provider)
        assert merged.base_system_prompt == "BASE"
        assert merged.system_prompt_suffix == "SUFFIX"

    def test_an_unregistered_key_resolves_to_nothing(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
    ) -> None:
        assert harness_profiles._get_harness_profile(f"{provider}:none") is None

    @pytest.mark.parametrize(
        "bad_key",
        ["", " leading", "a:b:c", "a: b", ":model", "provider:"],
    )
    def test_malformed_keys_are_rejected(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        bad_key: str,
    ) -> None:
        with pytest.raises(ValueError, match=r"[Pp]rofile key"):
            register_harness_profile(bad_key, HarnessProfile())


# ── effects on an assembled agent ──────────────────────────────────────


class TestHarnessProfileEffects:
    def test_caller_prompt_precedes_the_profile_base_and_suffix(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
        scripted: Any,
    ) -> None:
        register_harness_profile(
            provider,
            HarnessProfile(
                base_system_prompt="PROFILE-BASE",
                system_prompt_suffix="PROFILE-SUFFIX",
            ),
        )
        model, agent = scripted(f"{provider}:m", system_prompt="CALLER")
        run(agent)
        text = model.system_text
        assert text.index("CALLER") < text.index("PROFILE-BASE")
        assert text.index("PROFILE-BASE") < text.index("PROFILE-SUFFIX")

    def test_tool_description_overrides_are_applied(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
        scripted: Any,
    ) -> None:
        register_harness_profile(
            provider,
            HarnessProfile(
                tool_description_overrides={"ls": "CUSTOM-LS-DESCRIPTION"},
            ),
        )
        model, agent = scripted(f"{provider}:m")
        run(agent)
        ls_tool = next(t for t in model.log["tools"] if t.name == "ls")
        assert ls_tool.description == "CUSTOM-LS-DESCRIPTION"

    def test_excluded_tools_are_not_bound(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
        scripted: Any,
    ) -> None:
        register_harness_profile(
            provider,
            HarnessProfile(excluded_tools=frozenset({"glob", "grep"})),
        )
        model, agent = scripted(f"{provider}:m")
        run(agent)
        assert "glob" not in model.log["tool_names"]
        assert "grep" not in model.log["tool_names"]
        assert "read_file" in model.log["tool_names"]

    def test_extra_middleware_runs_without_displacing_the_prompt_tail(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
        scripted: Any,
    ) -> None:
        register_harness_profile(
            provider,
            HarnessProfile(
                base_system_prompt="PROFILE-BASE",
                extra_middleware=[_MarkerMiddleware("EXTRA-MIDDLEWARE")],
            ),
        )
        model, agent = scripted(f"{provider}:m", system_prompt="CALLER")
        run(agent)
        text = model.system_text
        assert text.count("EXTRA-MIDDLEWARE") == 1
        assert "PROFILE-BASE" in text
        assert text.index("CALLER") < text.index("PROFILE-BASE")

    def test_general_purpose_subagent_can_be_described_differently(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
        scripted: Any,
    ) -> None:
        register_harness_profile(
            provider,
            HarnessProfile(
                general_purpose_subagent=GeneralPurposeSubagentProfile(
                    description="CUSTOM-GP-DESCRIPTION",
                ),
            ),
        )
        model, agent = scripted(f"{provider}:m")
        run(agent)
        task_tool = next(t for t in model.log["tools"] if t.name == "task")
        assert "CUSTOM-GP-DESCRIPTION" in task_tool.description

    def test_disabling_the_default_subagent_removes_task(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
        scripted: Any,
    ) -> None:
        # With no other synchronous subagents there is nothing left for
        # `task` to dispatch to, so the tool disappears entirely.
        register_harness_profile(
            provider,
            HarnessProfile(
                general_purpose_subagent=GeneralPurposeSubagentProfile(enabled=False),
            ),
        )
        model, agent = scripted(f"{provider}:m")
        run(agent)
        assert "task" not in model.log["tool_names"]

    def test_excluding_required_scaffolding_fails_at_construction(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
        scripted: Any,
    ) -> None:
        # Silently degrading permissions or file tools would be worse than
        # refusing the profile; upstream rejects it at registration time,
        # before any graph can be built from it.
        del scripted
        with pytest.raises(ValueError, match="scaffolding cannot be excluded"):
            register_harness_profile(
                provider,
                HarnessProfile(excluded_middleware=frozenset({FilesystemMiddleware})),
            )


# ── the interpreter's stable identity ──────────────────────────────────


class TestInterpreterExclusion:
    def test_the_interpreter_can_be_excluded_by_class(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
        scripted: Any,
    ) -> None:
        register_harness_profile(
            provider,
            HarnessProfile(
                excluded_middleware=frozenset({WasmshInterpreterMiddleware}),
            ),
        )
        model, agent = scripted(
            f"{provider}:m",
            middleware=[WasmshInterpreterMiddleware(sandbox_factory=object)],
        )
        run(agent)
        assert "py_eval" not in model.log["tool_names"]

    def test_the_interpreter_can_be_excluded_by_its_public_name(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
        scripted: Any,
    ) -> None:
        register_harness_profile(
            provider,
            HarnessProfile(
                excluded_middleware=frozenset({"WasmshInterpreterMiddleware"}),
            ),
        )
        model, agent = scripted(
            f"{provider}:m",
            middleware=[WasmshInterpreterMiddleware(sandbox_factory=object)],
        )
        run(agent)
        assert "py_eval" not in model.log["tool_names"]

    def test_a_class_exclusion_round_trips_through_config(self) -> None:
        # Serializing a class-form exclusion requires the class to advertise
        # `serialized_name`; without one upstream refuses rather than
        # inventing a class path. That is exactly why the middleware pins it.
        profile = HarnessProfile(
            excluded_middleware=frozenset({WasmshInterpreterMiddleware}),
        )
        config = HarnessProfileConfig.from_harness_profile(profile)
        assert config.excluded_middleware == frozenset({"WasmshInterpreterMiddleware"})
        assert HarnessProfileConfig.from_dict(config.to_dict()) == config


class TestHarnessProfileConfigRoundTrip:
    def test_every_field_survives_to_dict_from_dict(self) -> None:
        config = HarnessProfileConfig(
            base_system_prompt="BASE",
            system_prompt_suffix="SUFFIX",
            tool_description_overrides={"execute": "run a command"},
            excluded_tools=frozenset({"glob"}),
            excluded_middleware=frozenset({"SummarizationMiddleware"}),
            general_purpose_subagent=GeneralPurposeSubagentProfile(
                description="GP",
                system_prompt="GP-PROMPT",
            ),
        )
        assert HarnessProfileConfig.from_dict(config.to_dict()) == config

    def test_runtime_middleware_instances_cannot_be_serialized(self) -> None:
        # An instance has no config-file representation; upstream says so
        # instead of dropping it silently.
        profile = HarnessProfile(extra_middleware=[_MarkerMiddleware("X")])
        with pytest.raises(ValueError, match="extra_middleware"):
            HarnessProfileConfig.from_harness_profile(profile)


# ── provider profiles ──────────────────────────────────────────────────


class TestProviderProfiles:
    def test_static_init_kwargs_are_applied(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
    ) -> None:
        register_provider_profile(
            provider,
            ProviderProfile(init_kwargs={"temperature": 0.1}),
        )
        assert provider_profiles.apply_provider_profile(f"{provider}:m") == {
            "temperature": 0.1,
        }

    def test_dynamic_kwargs_factory_is_consulted(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
    ) -> None:
        register_provider_profile(
            provider,
            ProviderProfile(init_kwargs_factory=lambda: {"max_tokens": 7}),
        )
        assert provider_profiles.apply_provider_profile(f"{provider}:m") == {
            "max_tokens": 7,
        }

    def test_caller_kwargs_take_precedence_over_profile_defaults(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
    ) -> None:
        register_provider_profile(
            provider,
            ProviderProfile(init_kwargs={"temperature": 0.1, "top_p": 0.9}),
        )
        merged = provider_profiles.apply_provider_profile(
            f"{provider}:m",
            {"temperature": 0.7},
        )
        assert merged == {"temperature": 0.7, "top_p": 0.9}

    def test_provider_and_exact_model_profiles_merge(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
    ) -> None:
        register_provider_profile(
            provider,
            ProviderProfile(init_kwargs={"temperature": 0.1}),
        )
        register_provider_profile(
            f"{provider}:m",
            ProviderProfile(init_kwargs={"max_tokens": 32}),
        )
        merged = provider_profiles.apply_provider_profile(f"{provider}:m")
        assert merged == {"temperature": 0.1, "max_tokens": 32}

    def test_pre_init_runs_and_its_failure_propagates(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
    ) -> None:
        calls: list[str] = []

        def pre_init(spec: str) -> None:
            calls.append(spec)
            msg = "missing credentials"
            raise RuntimeError(msg)

        register_provider_profile(provider, ProviderProfile(pre_init=pre_init))
        with pytest.raises(RuntimeError, match="missing credentials"):
            provider_profiles.apply_provider_profile(f"{provider}:m")
        assert calls == [f"{provider}:m"]

    def test_an_unregistered_spec_returns_the_caller_kwargs_unchanged(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
    ) -> None:
        assert provider_profiles.apply_provider_profile(
            f"{provider}:m",
            {"temperature": 0.2},
        ) == {"temperature": 0.2}

    @pytest.mark.parametrize("bad_key", ["", "a:b:c", " x", "provider:"])
    def test_malformed_keys_are_rejected(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        bad_key: str,
    ) -> None:
        with pytest.raises(ValueError, match=r"[Pp]rofile key"):
            register_provider_profile(bad_key, ProviderProfile())

    def test_the_registry_holds_no_backend_or_tenant_state(self) -> None:
        # Provider profiles configure model construction only. Sandbox
        # credentials, tenant identity, memory namespaces, and filesystem
        # policy deliberately live elsewhere.
        assert set(ProviderProfile.__dataclass_fields__) == {
            "init_kwargs",
            "pre_init",
            "init_kwargs_factory",
        }


class TestRegistryIsolation:
    def test_a_registration_does_not_leak_between_tests(
        self,
        isolated_registries: None,  # noqa: ARG002 -- fixture restores global registries
        provider: str,
    ) -> None:
        register_harness_profile(provider, HarnessProfile(base_system_prompt="X"))
        assert harness_profiles._get_harness_profile(provider) is not None
