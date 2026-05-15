"""Python-skill loader for the wasmsh interpreter.

Loads a skill's files from a DeepAgents ``BackendProtocol`` and mirrors them
into the sandbox VFS under ``/skills/<name>/`` so that user code can::

    import skills.my_skill
    from skills.my_skill import compute

The package's ``__init__.py`` is auto-generated when missing so that bare
``import skills.<name>`` always works.

The host-side scan ``scan_skill_references`` looks at user-submitted code for
``import skills.<name>`` / ``from skills.<name>`` patterns; the middleware
loads each referenced skill once per session.
"""

from __future__ import annotations

import logging
import re
from dataclasses import dataclass
from pathlib import PurePosixPath
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from deepagents.backends.protocol import BackendProtocol
    from deepagents.middleware.skills import SkillMetadata

logger = logging.getLogger(__name__)


SKILL_MODULE_EXTENSIONS: tuple[str, ...] = (".py",)
"""Extensions treated as Python source files for skill loading."""

SKILL_DATA_EXTENSIONS: tuple[str, ...] = (
    ".txt",
    ".json",
    ".yaml",
    ".yml",
    ".csv",
    ".md",
)
"""Extensions treated as data assets that should also be staged into the VFS."""


_MAX_BUNDLE_BYTES = 1 * 1024 * 1024
"""Hard cap on a single skill's total bundle size, in bytes."""


_SKILL_NAME_RE = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
"""Agent Skills spec: lowercase kebab-case, no leading/trailing/consecutive dashes."""


_SKILL_IMPORT_RE = re.compile(
    r"""(?xm)
    ^\s*(?:
        from\s+skills\.(?P<from_name>[a-z0-9_]+(?:\.[a-z0-9_]+)*)\s+import\b
        |
        import\s+skills\.(?P<import_name>[a-z0-9_]+(?:\.[a-z0-9_]+)*)
    )
    """,
)
"""Detects ``from skills.<name> import …`` and ``import skills.<name>`` literals."""


class SkillLoadError(Exception):
    """Base class for skill-load failures."""


class InvalidSkillScopeError(SkillLoadError):
    """Skill directory contains nothing installable or has a malformed name."""


class SkillInstallError(SkillLoadError):
    """Backend fetch failed or produced unreadable content for a skill."""


@dataclass(frozen=True)
class LoadedSkill:
    """A skill's install-ready file set.

    Attributes:
        name: The kebab-case skill name from frontmatter.
        package_name: The Python import name (``-`` → ``_``).
        files: ``{absolute_path_in_sandbox: bytes}``, ready to upload.
    """

    name: str
    package_name: str
    files: dict[str, bytes]


def _skill_dir_from_metadata(metadata: SkillMetadata) -> str:
    """Return the directory containing ``SKILL.md`` for this skill."""
    return str(PurePosixPath(metadata["path"]).parent)


def _package_name(skill_name: str) -> str:
    """Convert a kebab-case skill name to a Python-importable package name."""
    return skill_name.replace("-", "_")


def _sandbox_path(package_name: str, relative: str) -> str:
    """Return the absolute path the skill file is staged at inside the sandbox."""
    return f"/skills/{package_name}/{relative}"


def _enumerate_skill_files(
    backend: BackendProtocol,
    skill_dir: str,
) -> list[str]:
    """List every Python/data file under ``skill_dir`` (recursive, sorted)."""
    seen: set[str] = set()
    for ext in (*SKILL_MODULE_EXTENSIONS, *SKILL_DATA_EXTENSIONS):
        pattern = f"**/*{ext}"
        result = backend.glob(pattern, skill_dir)
        if result.error:
            msg = f"failed to list skill dir {skill_dir}: {result.error}"
            raise SkillInstallError(msg)
        for match in result.matches or []:
            seen.add(match["path"])
    return sorted(seen)


async def _aenumerate_skill_files(
    backend: BackendProtocol,
    skill_dir: str,
) -> list[str]:
    """Async sibling of :func:`_enumerate_skill_files`."""
    seen: set[str] = set()
    for ext in (*SKILL_MODULE_EXTENSIONS, *SKILL_DATA_EXTENSIONS):
        pattern = f"**/*{ext}"
        result = await backend.aglob(pattern, skill_dir)
        if result.error:
            msg = f"failed to list skill dir {skill_dir}: {result.error}"
            raise SkillInstallError(msg)
        for match in result.matches or []:
            seen.add(match["path"])
    return sorted(seen)


def _relative(skill_dir: str, absolute_path: str) -> str:
    """Return ``absolute_path`` expressed relative to ``skill_dir``."""
    skill_root = PurePosixPath(skill_dir)
    abs_path = PurePosixPath(absolute_path)
    try:
        rel = abs_path.relative_to(skill_root)
    except ValueError as exc:
        msg = f"file {absolute_path!r} is not under skill dir {skill_dir!r}"
        raise SkillInstallError(msg) from exc
    return str(rel)


def _build_files_map(
    *,
    skill_dir: str,
    package_name: str,
    entry_rel: str | None,
    file_pairs: list[tuple[str, bytes]],
) -> dict[str, bytes]:
    """Build the absolute-path → bytes map for one skill.

    Always installs an ``__init__.py`` for the skill package, either copied
    from a file the author shipped or synthesised when the entrypoint is a
    sibling file. This keeps ``import skills.<name>`` working regardless of
    how the skill is structured.
    """
    files: dict[str, bytes] = {}
    has_init = False
    entry_found = entry_rel is None
    for abs_path, content in file_pairs:
        rel = _relative(skill_dir, abs_path)
        target = _sandbox_path(package_name, rel)
        files[target] = content
        if rel == "__init__.py":
            has_init = True
        if entry_rel is not None and rel == entry_rel:
            entry_found = True
    if entry_rel is not None and not entry_found:
        msg = (
            f"skill {package_name!r}: module path {entry_rel!r} did not "
            "match any file in the skill directory"
        )
        raise InvalidSkillScopeError(msg)
    if not has_init:
        # Auto-generate the package __init__.py so plain `import skills.foo`
        # works even when the author shipped only a flat helper file.
        synth = b'"""Auto-generated skill package init."""\n'
        if entry_rel is not None and entry_rel.endswith(".py"):
            module_stem = PurePosixPath(entry_rel).stem
            if module_stem != "__init__":
                synth += f"from .{module_stem} import *  # noqa: F401,F403\n".encode()
        files[_sandbox_path(package_name, "__init__.py")] = synth
    return files


def _validate_bundle_size(files: dict[str, bytes], skill_name: str) -> None:
    total = sum(len(c) for c in files.values())
    if total > _MAX_BUNDLE_BYTES:
        msg = (
            f"skill {skill_name!r} bundle exceeds {_MAX_BUNDLE_BYTES} bytes "
            f"(total {total})"
        )
        raise SkillInstallError(msg)


def _validate_skill_name(name: str) -> None:
    if not _SKILL_NAME_RE.match(name):
        msg = f"skill name {name!r} is not a valid kebab-case identifier"
        raise InvalidSkillScopeError(msg)


def load_skill(
    metadata: SkillMetadata,
    backend: BackendProtocol,
) -> LoadedSkill:
    """Load one skill's bundle from ``backend`` into a :class:`LoadedSkill`."""
    name = metadata["name"]
    _validate_skill_name(name)
    entry_rel = metadata.get("module")
    skill_dir = _skill_dir_from_metadata(metadata)

    file_paths = _enumerate_skill_files(backend, skill_dir)
    if not file_paths:
        msg = f"skill {name!r}: no Python files under {skill_dir!r}"
        raise InvalidSkillScopeError(msg)

    responses = backend.download_files(file_paths)
    file_pairs: list[tuple[str, bytes]] = []
    for resp in responses:
        if resp.error or resp.content is None:
            msg = f"skill {name!r}: failed to download {resp.path!r}: {resp.error}"
            raise SkillInstallError(msg)
        file_pairs.append((resp.path, resp.content))

    package_name = _package_name(name)
    files = _build_files_map(
        skill_dir=skill_dir,
        package_name=package_name,
        entry_rel=entry_rel,
        file_pairs=file_pairs,
    )
    _validate_bundle_size(files, name)
    return LoadedSkill(name=name, package_name=package_name, files=files)


async def aload_skill(
    metadata: SkillMetadata,
    backend: BackendProtocol,
) -> LoadedSkill:
    """Async sibling of :func:`load_skill`."""
    name = metadata["name"]
    _validate_skill_name(name)
    entry_rel = metadata.get("module")
    skill_dir = _skill_dir_from_metadata(metadata)

    file_paths = await _aenumerate_skill_files(backend, skill_dir)
    if not file_paths:
        msg = f"skill {name!r}: no Python files under {skill_dir!r}"
        raise InvalidSkillScopeError(msg)

    responses = await backend.adownload_files(file_paths)
    file_pairs: list[tuple[str, bytes]] = []
    for resp in responses:
        if resp.error or resp.content is None:
            msg = f"skill {name!r}: failed to download {resp.path!r}: {resp.error}"
            raise SkillInstallError(msg)
        file_pairs.append((resp.path, resp.content))

    package_name = _package_name(name)
    files = _build_files_map(
        skill_dir=skill_dir,
        package_name=package_name,
        entry_rel=entry_rel,
        file_pairs=file_pairs,
    )
    _validate_bundle_size(files, name)
    return LoadedSkill(name=name, package_name=package_name, files=files)


def scan_skill_references(source: str) -> frozenset[str]:
    """Return the set of skill package names referenced via ``import``.

    Detects literal ``import skills.<name>`` and ``from skills.<name> import``
    statements. Dynamic / computed imports are not detected.

    Names are returned as **package names** (underscores), matching how the
    user writes them in code; convert back to the kebab-case skill name with
    ``name.replace("_", "-")`` before looking up metadata.
    """
    seen: set[str] = set()
    for match in _SKILL_IMPORT_RE.finditer(source):
        name = match.group("from_name") or match.group("import_name") or ""
        head = name.split(".", 1)[0]
        if head:
            seen.add(head)
    return frozenset(seen)


__all__ = [
    "SKILL_DATA_EXTENSIONS",
    "SKILL_MODULE_EXTENSIONS",
    "InvalidSkillScopeError",
    "LoadedSkill",
    "SkillInstallError",
    "SkillLoadError",
    "aload_skill",
    "load_skill",
    "scan_skill_references",
]
