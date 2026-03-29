"""Helpers for locating packaged wasmsh Pyodide runtime assets."""

from __future__ import annotations

from importlib import resources
from pathlib import Path


def get_dist_dir() -> Path:
    """Return the directory containing packaged Pyodide assets."""
    return Path(resources.files("wasmsh_pyodide_runtime")) / "assets"


def get_asset_path(name: str) -> Path:
    """Return the path to a specific packaged runtime asset."""
    return get_dist_dir() / name


def get_node_host_script() -> Path:
    """Return the packaged Node host script path from the npm runtime package layout.

    The Python DeepAgents integration uses this as the stable entrypoint when
    launching the local Node-backed host process.
    """

    return get_dist_dir() / "node-host.mjs"
