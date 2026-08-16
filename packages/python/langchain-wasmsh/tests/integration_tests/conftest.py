"""Shared fixtures for integration tests."""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

from langchain_wasmsh import WasmshSandbox

if TYPE_CHECKING:
    from collections.abc import Iterator


@pytest.fixture
def sandbox() -> Iterator[WasmshSandbox]:
    """A fresh wasmsh sandbox, closed when the test finishes."""
    s = WasmshSandbox()
    try:
        yield s
    finally:
        s.close()


@pytest.fixture(scope="module")
def shared_sandbox() -> Iterator[WasmshSandbox]:
    """One sandbox per test module.

    Booting Pyodide costs a second or two, and the composition tests are
    about graph assembly rather than sandbox isolation, so they share a
    session and write under distinct paths.
    """
    s = WasmshSandbox()
    try:
        yield s
    finally:
        s.close()
