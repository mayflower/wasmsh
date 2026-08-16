"""Optional Python-import bridge over upstream Agent Skills.

Deep Agents' :class:`~deepagents.middleware.skills.SkillsMiddleware` owns
skill discovery, ``SKILL.md`` parsing, source precedence, and progressive
disclosure. This module adds one narrow extension on top of it: staging a
selected skill's files into the interpreter sandbox so generated code can::

    import skills.my_skill
    from skills.my_skill import compute

Nothing here replaces or duplicates upstream discovery. Metadata and
instructions stay where upstream put them; only a skill the running program
actually imports is copied into the sandbox.

Importability is opt-in, not assumed
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

An Agent Skill name is lowercase with hyphens and may contain non-ASCII
letters. A Python package name is a Python identifier. Those sets overlap
but are not the same, so this module does **not** claim every valid skill is
importable. ``my-skill`` maps to ``skills.my_skill`` because that mapping is
unambiguous; anything that does not produce a valid identifier must declare
one explicitly through :data:`SKILL_PYTHON_PACKAGE_KEY`.

Both extension keys live under the ``wasmsh.`` prefix inside upstream's
free-form ``metadata`` mapping. Exact 0.7.4 ``SkillMetadata`` has ``path``,
``name``, ``description``, ``license``, ``compatibility``, ``metadata``, and
``allowed_tools`` — and no top-level ``module`` field — so a namespaced key
inside ``metadata`` is the only place a wasmsh-specific hint can live
without colliding with a future upstream field.

```yaml
---
name: sales-report
description: Build the weekly sales report
metadata:
  wasmsh.python_package: sales_report   # optional import alias
  wasmsh.python_module: lib/report.py   # optional re-export entrypoint
---
```

``allowed-tools`` in skill frontmatter is descriptive metadata, not an
authorization boundary. Tool exposure inside the interpreter is governed by
the PTC allowlist; see :mod:`langchain_wasmsh._ptc`.

What gets staged
~~~~~~~~~~~~~~~~

Every regular file under the skill directory, not a hand-picked extension
list: skills ship shell scripts, SQL, templates, fixtures, and binary assets
alongside their Python, and a ``.py/.md/.json`` allowlist quietly dropped all
of it. Directory structure is preserved and bytes are copied verbatim —
assets are never decoded as text.

Three bounds apply, and a skill that exceeds any of them fails loudly rather
than being silently truncated: :data:`MAX_SKILL_FILE_BYTES` per file,
:data:`MAX_SKILL_BUNDLE_BYTES` per bundle, and :data:`MAX_SKILL_FILE_COUNT`
files. Every path is re-checked for containment after normalisation, so a
backend returning a path outside the skill directory — through a symlink or
otherwise — is rejected before anything is uploaded.

Reload semantics
~~~~~~~~~~~~~~~~

Staged bundles are cached by a content fingerprint, so a rebuilt bundle with
identical bytes is not re-uploaded, while changed bytes produce a different
fingerprint and a fresh staging. That is only half the story: upstream
``SkillsMiddleware`` loads ``skills_metadata`` once per thread and keeps it in
private state, so a thread already running keeps the skill *view* it started
with. Editing a skill persists immediately, a **new** thread sees it, and an
existing checkpointed thread does not. This module deliberately adds no
watcher or polling loop to paper over that; it is upstream's contract.
"""

from __future__ import annotations

import hashlib
import keyword
import logging
import posixpath
import re
from dataclasses import dataclass, field
from pathlib import PurePosixPath
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Sequence

    from deepagents.backends.protocol import (
        BackendProtocol,
        FileDownloadResponse,
        FileInfo,
    )
    from deepagents.middleware.skills import SkillMetadata

logger = logging.getLogger(__name__)


SKILL_PYTHON_PACKAGE_KEY = "wasmsh.python_package"
"""Frontmatter metadata key declaring the skill's Python package name."""

SKILL_PYTHON_MODULE_KEY = "wasmsh.python_module"
"""Frontmatter metadata key naming a module to re-export from ``__init__``.

Value is a path relative to the skill directory, e.g. ``lib/report.py``.
"""

MAX_SKILL_FILE_BYTES = 2 * 1024 * 1024
"""Largest single file that may be staged into the sandbox."""

MAX_SKILL_BUNDLE_BYTES = 8 * 1024 * 1024
"""Largest total bundle size for one skill."""

MAX_SKILL_FILE_COUNT = 512
"""Largest number of files in one staged skill bundle."""

_SKILL_NAME_RE = re.compile(r"^[^\s/\\]+$")
"""Reject only what cannot be a directory-safe skill name.

Upstream permits far more than ASCII kebab-case (lowercase Unicode letters,
for one), so validating against the narrower spelling here would refuse
skills that upstream loads happily. Importability is decided separately by
:func:`python_package_name`.
"""

_SKILL_IMPORT_RE = re.compile(
    r"""(?xm)
    ^\s*(?:
        from\s+skills\.(?P<from_name>[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)\s+import\b
        |
        import\s+skills\.(?P<import_name>[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)
    )
    """,
)
"""Detects ``from skills.<name> import …`` and ``import skills.<name>``."""


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
        name: The skill name from frontmatter.
        package_name: The Python import name under ``skills.``.
        files: ``{absolute_path_in_sandbox: bytes}``, ready to upload.
        fingerprint: Content hash over the staged file set. Two bundles with
            the same fingerprint are byte-identical, which is what lets the
            cache skip a re-upload without consulting the source backend.
    """

    name: str
    package_name: str
    files: dict[str, bytes]
    fingerprint: str


@dataclass
class SkillBundleCache:
    """Tracks which skill bundles are staged in one sandbox, by fingerprint."""

    installed: dict[str, str] = field(default_factory=dict)

    def is_current(self, loaded: LoadedSkill) -> bool:
        """Return whether ``loaded`` is already staged with identical bytes."""
        return self.installed.get(loaded.package_name) == loaded.fingerprint

    def record(self, loaded: LoadedSkill) -> None:
        """Remember that ``loaded`` is now staged."""
        self.installed[loaded.package_name] = loaded.fingerprint


# ── naming ─────────────────────────────────────────────────────────────


def _extra_metadata(metadata: SkillMetadata) -> dict[str, str]:
    extra = metadata.get("metadata") or {}
    return extra if isinstance(extra, dict) else {}


def _is_importable_identifier(name: str) -> bool:
    return bool(name) and name.isidentifier() and not keyword.iskeyword(name)


def python_package_name(metadata: SkillMetadata) -> str | None:
    """Return the ``skills.<name>`` package for a skill, or `None`.

    A `None` result means "this skill is not reachable through the Python
    import bridge" — not that it is broken. Upstream discovery, progressive
    disclosure, and `read_file` on its instructions all still work; only
    `import skills.<name>` is unavailable.

    Args:
        metadata: Upstream `SkillMetadata` for one skill.

    Returns:
        The package name to import under `skills.`, or `None` when the skill
            declares no alias and its name does not map to a Python
            identifier.

    Raises:
        InvalidSkillScopeError: If the skill declares a
            :data:`SKILL_PYTHON_PACKAGE_KEY` alias that is not a valid,
            non-keyword Python identifier. An explicit alias that cannot work
            is a mistake worth surfacing, unlike an absent one.
    """
    alias = _extra_metadata(metadata).get(SKILL_PYTHON_PACKAGE_KEY)
    if alias is not None:
        if not _is_importable_identifier(alias):
            msg = (
                f"skill {metadata['name']!r} declares "
                f"{SKILL_PYTHON_PACKAGE_KEY}={alias!r}, which is not a valid "
                "Python identifier"
            )
            raise InvalidSkillScopeError(msg)
        return alias

    derived = metadata["name"].replace("-", "_")
    if _is_importable_identifier(derived) and derived.isascii():
        return derived
    logger.debug(
        "skill %r is not importable: %r is not a Python identifier and no %s "
        "alias was declared",
        metadata["name"],
        derived,
        SKILL_PYTHON_PACKAGE_KEY,
    )
    return None


def resolve_importable_skills(
    skills: dict[str, SkillMetadata],
) -> dict[str, SkillMetadata]:
    """Index ``skills`` by the package name generated code would import.

    Skills that are not importable are omitted, and an invalid explicit alias
    is reported without taking the rest of the library down with it.
    """
    resolved: dict[str, SkillMetadata] = {}
    for metadata in skills.values():
        try:
            package = python_package_name(metadata)
        except InvalidSkillScopeError as exc:
            logger.warning("skipping skill with an unusable import alias: %s", exc)
            continue
        if package is None:
            continue
        if package in resolved:
            logger.warning(
                "two skills map to `skills.%s`; keeping %r and ignoring %r. "
                "Give one of them a distinct %s alias.",
                package,
                resolved[package]["name"],
                metadata["name"],
                SKILL_PYTHON_PACKAGE_KEY,
            )
            continue
        resolved[package] = metadata
    return resolved


# ── enumeration ────────────────────────────────────────────────────────


def _skill_dir_from_metadata(metadata: SkillMetadata) -> str:
    """Return the directory containing ``SKILL.md`` for this skill."""
    return str(PurePosixPath(metadata["path"]).parent)


def _sandbox_path(package_name: str, relative: str) -> str:
    """Return the absolute path the skill file is staged at inside the sandbox."""
    return f"/skills/{package_name}/{relative}"


def _relative_within(skill_dir: str, reported_path: str, skill_name: str) -> str:
    """Normalise a backend-reported path to a contained relative path.

    Backends disagree on shape: `BaseSandbox.glob` returns paths relative to
    the search root, while `StoreBackend` and `StateBackend` return absolute
    ones. Both are accepted, and both are re-checked for containment after
    normalisation — a backend that resolved a symlink out of the skill
    directory, or that returns a traversing path, is rejected here rather
    than having its bytes uploaded into the sandbox.
    """
    root = posixpath.normpath(skill_dir)
    candidate = reported_path
    if not candidate.startswith("/"):
        candidate = posixpath.join(root, candidate)
    resolved = posixpath.normpath(candidate)
    if resolved != root and not resolved.startswith(root.rstrip("/") + "/"):
        msg = (
            f"skill {skill_name!r}: backend returned {reported_path!r}, which "
            f"resolves outside the skill directory {skill_dir!r}"
        )
        raise SkillInstallError(msg)
    relative = posixpath.relpath(resolved, root)
    if relative == "." or relative.startswith(".."):
        msg = (
            f"skill {skill_name!r}: {reported_path!r} is not a file under {skill_dir!r}"
        )
        raise SkillInstallError(msg)
    return relative


def _file_paths_from_matches(
    matches: list[FileInfo] | None,
    skill_dir: str,
    skill_name: str,
) -> list[tuple[str, str]]:
    """Return sorted ``(reported_path, relative_path)`` pairs for regular files."""
    pairs: dict[str, str] = {}
    for match in matches or []:
        if match.get("is_dir"):
            continue
        relative = _relative_within(skill_dir, match["path"], skill_name)
        pairs[relative] = match["path"]
    if len(pairs) > MAX_SKILL_FILE_COUNT:
        msg = (
            f"skill {skill_name!r} has {len(pairs)} files, over the "
            f"{MAX_SKILL_FILE_COUNT}-file staging limit"
        )
        raise SkillInstallError(msg)
    return [(pairs[rel], rel) for rel in sorted(pairs)]


_GLOB_PATTERN = "**/*"
"""Everything under the skill directory, at any depth.

Python's `glob` — which every backend's implementation is built on — omits
dot-prefixed entries, so a skill's hidden files are not staged. That is
upstream `glob` behaviour rather than a wasmsh choice, and skills that need a
dotfile at runtime should write it from their own code.
"""


def _collect(
    metadata: SkillMetadata,
    matches: list[FileInfo] | None,
    contents: dict[str, bytes],
) -> LoadedSkill:
    """Assemble a :class:`LoadedSkill` from enumerated paths and their bytes."""
    name = metadata["name"]
    _validate_skill_name(name)
    package_name = python_package_name(metadata)
    if package_name is None:
        msg = (
            f"skill {name!r} is not importable from Python: its name is not a "
            f"valid identifier and it declares no {SKILL_PYTHON_PACKAGE_KEY}"
        )
        raise InvalidSkillScopeError(msg)

    skill_dir = _skill_dir_from_metadata(metadata)
    pairs = _file_paths_from_matches(matches, skill_dir, name)
    if not pairs:
        msg = f"skill {name!r}: no files found under {skill_dir!r}"
        raise InvalidSkillScopeError(msg)

    files: dict[str, bytes] = {}
    total = 0
    for reported, relative in pairs:
        content = contents[reported]
        if len(content) > MAX_SKILL_FILE_BYTES:
            msg = (
                f"skill {name!r}: {relative!r} is {len(content)} bytes, over "
                f"the {MAX_SKILL_FILE_BYTES}-byte per-file limit"
            )
            raise SkillInstallError(msg)
        total += len(content)
        if total > MAX_SKILL_BUNDLE_BYTES:
            msg = f"skill {name!r} bundle exceeds {MAX_SKILL_BUNDLE_BYTES} bytes"
            raise SkillInstallError(msg)
        files[_sandbox_path(package_name, relative)] = content

    _add_package_init(
        files=files,
        metadata=metadata,
        package_name=package_name,
        staged_relatives={rel for _, rel in pairs},
    )
    return LoadedSkill(
        name=name,
        package_name=package_name,
        files=files,
        fingerprint=_fingerprint(files),
    )


def _add_package_init(
    *,
    files: dict[str, bytes],
    metadata: SkillMetadata,
    package_name: str,
    staged_relatives: set[str],
) -> None:
    """Ensure ``skills.<package>`` is an importable package.

    A skill that ships its own ``__init__.py`` is left alone. Otherwise one is
    synthesised, re-exporting the module named by
    :data:`SKILL_PYTHON_MODULE_KEY` when the skill declares one.
    """
    if "__init__.py" in staged_relatives:
        return

    entry = _extra_metadata(metadata).get(SKILL_PYTHON_MODULE_KEY)
    body = b'"""Auto-generated skill package init."""\n'
    if entry:
        normalized = posixpath.normpath(entry.lstrip("/"))
        if normalized not in staged_relatives:
            msg = (
                f"skill {metadata['name']!r}: {SKILL_PYTHON_MODULE_KEY}="
                f"{entry!r} does not match any file in the skill directory"
            )
            raise InvalidSkillScopeError(msg)
        if not normalized.endswith(".py"):
            msg = (
                f"skill {metadata['name']!r}: {SKILL_PYTHON_MODULE_KEY}="
                f"{entry!r} is not a Python module"
            )
            raise InvalidSkillScopeError(msg)
        module = ".".join(PurePosixPath(normalized).with_suffix("").parts)
        if not all(_is_importable_identifier(part) for part in module.split(".")):
            msg = (
                f"skill {metadata['name']!r}: {SKILL_PYTHON_MODULE_KEY}="
                f"{entry!r} does not map to an importable module path"
            )
            raise InvalidSkillScopeError(msg)
        body += f"from .{module} import *  # noqa: F401,F403\n".encode()
    files[_sandbox_path(package_name, "__init__.py")] = body


def _fingerprint(files: dict[str, bytes]) -> str:
    digest = hashlib.sha256()
    for path in sorted(files):
        digest.update(path.encode("utf-8"))
        digest.update(b"\0")
        digest.update(hashlib.sha256(files[path]).digest())
    return digest.hexdigest()


def _validate_skill_name(name: str) -> None:
    if not _SKILL_NAME_RE.match(name):
        msg = f"skill name {name!r} contains whitespace or a path separator"
        raise InvalidSkillScopeError(msg)


def _downloaded_contents(
    responses: Sequence[FileDownloadResponse],
    skill_name: str,
) -> dict[str, bytes]:
    contents: dict[str, bytes] = {}
    for resp in responses:
        if resp.error or resp.content is None:
            msg = (
                f"skill {skill_name!r}: failed to download {resp.path!r}: {resp.error}"
            )
            raise SkillInstallError(msg)
        contents[resp.path] = resp.content
    return contents


# ── loaders ────────────────────────────────────────────────────────────


def load_skill(
    metadata: SkillMetadata,
    backend: BackendProtocol,
) -> LoadedSkill:
    """Load one skill's bundle from ``backend`` into a :class:`LoadedSkill`."""
    skill_dir = _skill_dir_from_metadata(metadata)
    result = backend.glob(_GLOB_PATTERN, skill_dir)
    if result.error:
        msg = f"failed to list skill dir {skill_dir}: {result.error}"
        raise SkillInstallError(msg)
    pairs = _file_paths_from_matches(result.matches, skill_dir, metadata["name"])
    responses = backend.download_files([reported for reported, _ in pairs])
    contents = _downloaded_contents(responses, metadata["name"])
    return _collect(metadata, result.matches, contents)


async def aload_skill(
    metadata: SkillMetadata,
    backend: BackendProtocol,
) -> LoadedSkill:
    """Async sibling of :func:`load_skill`."""
    skill_dir = _skill_dir_from_metadata(metadata)
    result = await backend.aglob(_GLOB_PATTERN, skill_dir)
    if result.error:
        msg = f"failed to list skill dir {skill_dir}: {result.error}"
        raise SkillInstallError(msg)
    pairs = _file_paths_from_matches(result.matches, skill_dir, metadata["name"])
    responses = await backend.adownload_files([reported for reported, _ in pairs])
    contents = _downloaded_contents(responses, metadata["name"])
    return _collect(metadata, result.matches, contents)


def scan_skill_references(source: str) -> frozenset[str]:
    """Return the set of skill package names referenced via ``import``.

    Detects literal ``import skills.<name>`` and ``from skills.<name> import``
    statements. Dynamic / computed imports are not detected.

    Names are returned as **package names** — the spelling generated code
    uses. Map them back to skills with :func:`resolve_importable_skills`.
    """
    seen: set[str] = set()
    for match in _SKILL_IMPORT_RE.finditer(source):
        name = match.group("from_name") or match.group("import_name") or ""
        head = name.split(".", 1)[0]
        if head:
            seen.add(head)
    return frozenset(seen)


__all__ = [
    "MAX_SKILL_BUNDLE_BYTES",
    "MAX_SKILL_FILE_BYTES",
    "MAX_SKILL_FILE_COUNT",
    "SKILL_PYTHON_MODULE_KEY",
    "SKILL_PYTHON_PACKAGE_KEY",
    "InvalidSkillScopeError",
    "LoadedSkill",
    "SkillBundleCache",
    "SkillInstallError",
    "SkillLoadError",
    "aload_skill",
    "load_skill",
    "python_package_name",
    "resolve_importable_skills",
    "scan_skill_references",
]
