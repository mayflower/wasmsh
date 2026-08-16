"""Tests for the optional Python-import bridge over upstream Agent Skills.

These run against a real `StoreBackend` rather than a mock so `glob` and
`download_files` behave the way an actual Deep Agents backend does — the
previous mock returned a shape no shipped backend produces.
"""

from __future__ import annotations

from typing import Any

import pytest
from deepagents.backends.protocol import FileDownloadResponse, GlobResult
from deepagents.backends.store import StoreBackend
from langgraph.store.memory import InMemoryStore

from langchain_wasmsh._skills import (
    MAX_SKILL_FILE_BYTES,
    SKILL_PYTHON_MODULE_KEY,
    SKILL_PYTHON_PACKAGE_KEY,
    InvalidSkillScopeError,
    LoadedSkill,
    SkillBundleCache,
    SkillInstallError,
    aload_skill,
    load_skill,
    python_package_name,
    resolve_importable_skills,
    scan_skill_references,
)


def _metadata(
    name: str,
    *,
    directory: str | None = None,
    extra: dict[str, str] | None = None,
) -> Any:
    directory = directory or f"/skills/{name}"
    return {
        "name": name,
        "path": f"{directory}/SKILL.md",
        "description": f"{name} description",
        "license": None,
        "compatibility": None,
        "metadata": extra or {},
        "allowed_tools": [],
    }


def _backend(files: dict[str, bytes]) -> StoreBackend:
    backend = StoreBackend(
        store=InMemoryStore(),
        namespace=lambda _rt: ("skills-test",),
    )
    responses = backend.upload_files(list(files.items()))
    for response in responses:
        assert response.error is None, response
    return backend


# ── import-name resolution ─────────────────────────────────────────────


class TestPythonPackageName:
    def test_kebab_case_maps_to_snake_case(self) -> None:
        assert python_package_name(_metadata("order-helpers")) == "order_helpers"

    def test_explicit_alias_wins(self) -> None:
        metadata = _metadata(
            "order-helpers",
            extra={SKILL_PYTHON_PACKAGE_KEY: "orders"},
        )
        assert python_package_name(metadata) == "orders"

    def test_non_ascii_name_is_not_importable_without_an_alias(self) -> None:
        # Upstream accepts this skill; we simply do not claim it can be
        # imported, rather than generating a package name that may collide
        # or fail at import time.
        assert python_package_name(_metadata("recherche-café")) is None

    def test_non_ascii_name_becomes_importable_with_an_alias(self) -> None:
        metadata = _metadata(
            "recherche-café",
            extra={SKILL_PYTHON_PACKAGE_KEY: "recherche_cafe"},
        )
        assert python_package_name(metadata) == "recherche_cafe"

    def test_name_colliding_with_a_keyword_is_not_importable(self) -> None:
        assert python_package_name(_metadata("class")) is None

    def test_invalid_explicit_alias_is_an_error(self) -> None:
        # An alias that cannot work is a mistake in the skill, unlike an
        # absent one, so it is surfaced instead of silently ignored.
        metadata = _metadata("demo", extra={SKILL_PYTHON_PACKAGE_KEY: "not-valid"})
        with pytest.raises(InvalidSkillScopeError, match="not a valid Python"):
            python_package_name(metadata)

    def test_no_top_level_module_field_is_consulted(self) -> None:
        # Exact 0.7.4 SkillMetadata has no `module` key; a stray one must not
        # influence anything.
        metadata = _metadata("demo")
        metadata["module"] = "helper.py"
        assert python_package_name(metadata) == "demo"


class TestResolveImportableSkills:
    def test_indexes_by_package_name_and_drops_unimportable(self) -> None:
        skills = {
            "order-helpers": _metadata("order-helpers"),
            "recherche-café": _metadata("recherche-café"),
        }
        resolved = resolve_importable_skills(skills)
        assert set(resolved) == {"order_helpers"}

    def test_duplicate_package_names_keep_the_first(
        self,
        caplog: pytest.LogCaptureFixture,
    ) -> None:
        skills = {
            "order-helpers": _metadata("order-helpers"),
            "clash": _metadata(
                "clash",
                extra={SKILL_PYTHON_PACKAGE_KEY: "order_helpers"},
            ),
        }
        with caplog.at_level("WARNING"):
            resolved = resolve_importable_skills(skills)
        assert resolved["order_helpers"]["name"] == "order-helpers"
        assert "distinct" in caplog.text

    def test_a_broken_alias_does_not_take_down_the_library(self) -> None:
        skills = {
            "good": _metadata("good"),
            "bad": _metadata("bad", extra={SKILL_PYTHON_PACKAGE_KEY: "1nvalid"}),
        }
        assert set(resolve_importable_skills(skills)) == {"good"}


# ── staging ────────────────────────────────────────────────────────────


class TestLoadSkill:
    def test_stages_every_regular_file_preserving_structure(self) -> None:
        backend = _backend(
            {
                "/skills/demo/SKILL.md": b"---\nname: demo\n---\n",
                "/skills/demo/helper.py": b"def add(a, b):\n    return a + b\n",
                "/skills/demo/scripts/run.sh": b"#!/bin/sh\necho hi\n",
                "/skills/demo/queries/report.sql": b"SELECT 1;\n",
                "/skills/demo/refs/notes.md": b"notes\n",
            },
        )
        loaded = load_skill(_metadata("demo"), backend)

        assert isinstance(loaded, LoadedSkill)
        assert loaded.package_name == "demo"
        # Shell scripts, SQL and references are staged too — the old
        # extension allowlist silently dropped all three.
        assert set(loaded.files) == {
            "/skills/demo/SKILL.md",
            "/skills/demo/helper.py",
            "/skills/demo/scripts/run.sh",
            "/skills/demo/queries/report.sql",
            "/skills/demo/refs/notes.md",
            "/skills/demo/__init__.py",
        }

    def test_binary_assets_survive_byte_for_byte(self) -> None:
        png = b"\x89PNG\r\n\x1a\n\x00\xff\xfe\xfd" * 16
        backend = _backend(
            {
                "/skills/demo/SKILL.md": b"---\nname: demo\n---\n",
                "/skills/demo/assets/logo.png": png,
            },
        )
        loaded = load_skill(_metadata("demo"), backend)
        assert loaded.files["/skills/demo/assets/logo.png"] == png

    def test_synthesises_a_package_init(self) -> None:
        backend = _backend({"/skills/demo/helper.py": b"X = 1\n"})
        loaded = load_skill(_metadata("demo"), backend)
        assert b"Auto-generated" in loaded.files["/skills/demo/__init__.py"]

    def test_authors_own_init_is_left_alone(self) -> None:
        backend = _backend({"/skills/demo/__init__.py": b"CUSTOM = 1\n"})
        loaded = load_skill(_metadata("demo"), backend)
        assert loaded.files["/skills/demo/__init__.py"] == b"CUSTOM = 1\n"

    def test_module_hint_is_reexported_from_init(self) -> None:
        backend = _backend(
            {
                "/skills/demo/SKILL.md": b"---\n",
                "/skills/demo/lib/report.py": b"def build():\n    return 1\n",
            },
        )
        metadata = _metadata("demo", extra={SKILL_PYTHON_MODULE_KEY: "lib/report.py"})
        loaded = load_skill(metadata, backend)
        assert b"from .lib.report import *" in loaded.files["/skills/demo/__init__.py"]

    def test_module_hint_pointing_at_nothing_is_an_error(self) -> None:
        backend = _backend({"/skills/demo/helper.py": b"X = 1\n"})
        metadata = _metadata("demo", extra={SKILL_PYTHON_MODULE_KEY: "missing.py"})
        with pytest.raises(InvalidSkillScopeError, match="does not match any file"):
            load_skill(metadata, backend)

    def test_module_hint_must_be_a_python_module(self) -> None:
        backend = _backend({"/skills/demo/run.sh": b"echo hi\n"})
        metadata = _metadata("demo", extra={SKILL_PYTHON_MODULE_KEY: "run.sh"})
        with pytest.raises(InvalidSkillScopeError, match="not a Python module"):
            load_skill(metadata, backend)

    def test_alias_changes_the_staged_package_directory(self) -> None:
        backend = _backend({"/skills/recherche-café/helper.py": b"X = 1\n"})
        metadata = _metadata(
            "recherche-café",
            extra={SKILL_PYTHON_PACKAGE_KEY: "recherche_cafe"},
        )
        loaded = load_skill(metadata, backend)
        assert "/skills/recherche_cafe/helper.py" in loaded.files

    def test_unimportable_skill_cannot_be_staged(self) -> None:
        backend = _backend({"/skills/recherche-café/helper.py": b"X = 1\n"})
        with pytest.raises(InvalidSkillScopeError, match="not importable"):
            load_skill(_metadata("recherche-café"), backend)

    def test_empty_skill_directory_is_an_error(self) -> None:
        backend = _backend({"/elsewhere/other.py": b"X = 1\n"})
        with pytest.raises(InvalidSkillScopeError, match="no files found"):
            load_skill(_metadata("demo"), backend)

    def test_oversized_file_is_rejected(self) -> None:
        backend = _backend(
            {"/skills/demo/big.bin": b"x" * (MAX_SKILL_FILE_BYTES + 1)},
        )
        with pytest.raises(SkillInstallError, match="per-file limit"):
            load_skill(_metadata("demo"), backend)

    def test_backend_path_outside_the_skill_directory_is_rejected(self) -> None:
        # Stand-in for a symlink the source backend resolved out of the skill
        # tree: whatever the reason, those bytes must not be uploaded.
        class EscapingBackend:
            def glob(self, pattern: str, path: str | None = None) -> Any:
                del pattern, path
                return GlobResult(
                    matches=[{"path": "/etc/shadow", "is_dir": False}],
                )

            def download_files(self, paths: list[str]) -> Any:
                msg = f"download must not be reached for {paths!r}"
                raise AssertionError(msg)

        with pytest.raises(SkillInstallError, match="outside the skill directory"):
            load_skill(_metadata("demo"), EscapingBackend())

    def test_relative_backend_paths_are_accepted(self) -> None:
        # `BaseSandbox.glob` reports paths relative to the search root while
        # `StoreBackend` reports absolute ones; both must load.
        class RelativeBackend:
            def glob(self, pattern: str, path: str | None = None) -> Any:
                del pattern, path
                return GlobResult(matches=[{"path": "helper.py", "is_dir": False}])

            def download_files(self, paths: list[str]) -> Any:
                return [FileDownloadResponse(path=p, content=b"X = 1\n") for p in paths]

        loaded = load_skill(_metadata("demo"), RelativeBackend())
        assert loaded.files["/skills/demo/helper.py"] == b"X = 1\n"

    async def test_async_loader_matches_the_sync_one(self) -> None:
        files = {
            "/skills/demo/SKILL.md": b"---\n",
            "/skills/demo/helper.py": b"X = 1\n",
        }
        assert (
            load_skill(_metadata("demo"), _backend(files)).files
            == (await aload_skill(_metadata("demo"), _backend(files))).files
        )


class TestFingerprintCache:
    @staticmethod
    def _load(helper: bytes) -> LoadedSkill:
        return load_skill(_metadata("demo"), _backend({"/skills/demo/h.py": helper}))

    def test_identical_bytes_are_considered_already_staged(self) -> None:
        cache = SkillBundleCache()
        first = self._load(b"X = 1\n")
        assert cache.is_current(first) is False
        cache.record(first)
        assert cache.is_current(self._load(b"X = 1\n")) is True

    def test_changed_bytes_produce_a_new_fingerprint(self) -> None:
        # Caching by name alone would have kept serving the stale bundle to
        # a fresh thread that reloaded the skill.
        cache = SkillBundleCache()
        cache.record(self._load(b"X = 1\n"))
        assert cache.is_current(self._load(b"X = 2\n")) is False


# ── source scanning ────────────────────────────────────────────────────


class TestScanSkillReferences:
    def test_handles_both_import_forms(self) -> None:
        source = "import skills.foo\nfrom skills.bar import baz\n"
        assert scan_skill_references(source) == frozenset({"foo", "bar"})

    def test_ignores_unrelated_imports(self) -> None:
        assert (
            scan_skill_references("import os\nfrom json import loads\n") == frozenset()
        )

    def test_returns_the_head_package_of_a_dotted_import(self) -> None:
        assert scan_skill_references("import skills.foo.bar") == frozenset({"foo"})
