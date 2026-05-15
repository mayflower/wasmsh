"""In-sandbox launcher script.

This module owns a single string constant — the Python program that runs
inside the wasmsh sandbox on every interpreter call. The launcher:

- restores the persisted globals dict (if any) from
  ``/tmp/__wasmsh_repl_globals.pkl``;
- parses the user's source so the final expression can be returned
  REPL-style alongside any captured stdout/stderr;
- runs the program with both streams redirected to in-memory buffers;
- pickles the new globals, dropping unpicklable entries;
- emits a single ``__WASMSH_REPL_RESULT__:<json>`` marker line on stdout.

The host parses that marker; the JSON envelope has a stable shape so it can
evolve independently of the LangChain ``ToolMessage`` rendering on the host
side.
"""

from __future__ import annotations

LAUNCHER_PATH = "/tmp/__wasmsh_repl_launcher.py"  # noqa: S108 -- sandbox VFS path
GLOBALS_PATH = "/tmp/__wasmsh_repl_globals.pkl"  # noqa: S108 -- sandbox VFS path
CODE_PATH = "/tmp/__wasmsh_repl_code.py"  # noqa: S108 -- sandbox VFS path
RESULT_MARKER = "__WASMSH_REPL_RESULT__:"
SKILLS_ROOT = "/skills"


LAUNCHER_SCRIPT: str = '''"""wasmsh REPL launcher (runs inside the sandbox)."""

import ast
import io
import json
import os
import pickle
import sys
import traceback

GLOBALS_PATH = "/tmp/__wasmsh_repl_globals.pkl"
RESULT_MARKER = "__WASMSH_REPL_RESULT__:"
SKILLS_ROOT = "/skills"
_MAX_REPR = 4000


def _load_globals():
    if not os.path.exists(GLOBALS_PATH):
        return {"__name__": "__main__", "__builtins__": __builtins__}
    try:
        with open(GLOBALS_PATH, "rb") as f:
            ns = pickle.load(f)
    except Exception:
        ns = {}
    ns["__name__"] = "__main__"
    ns["__builtins__"] = __builtins__
    return ns


# NOTE: keep this skip-list in sync with the PTC helper in
# packages/npm/wasmsh-pyodide/lib/ptc-helper.mjs (_save_globals there).
_SKIP_NAMES = (
    "sys", "os", "io", "json", "pickle", "ast", "asyncio",
    "builtins", "traceback", "tools",
)


def _save_globals(ns):
    keep = {}
    for k, v in ns.items():
        if k.startswith("__"):
            continue
        if k in _SKIP_NAMES:
            continue
        try:
            pickle.dumps(v)
        except Exception:
            continue
        keep[k] = v
    try:
        with open(GLOBALS_PATH, "wb") as f:
            pickle.dump(keep, f, protocol=pickle.HIGHEST_PROTOCOL)
    except Exception as exc:
        sys.stderr.write(f"[wasmsh] failed to persist globals: {exc}\\n")


def _safe_value(value):
    """Return the trailing-expression value in a JSON-safe shape.

    Primitives pass through unchanged so callers see the tool's native value.
    Lists / dicts recurse. Anything else falls back to ``repr`` truncated to
    ``_MAX_REPR`` chars, surfaced as a string with the original type in a
    suffix so the model can see what was elided.
    """
    if value is None or isinstance(value, (bool, int, float, str)):
        return value
    if isinstance(value, list):
        return [_safe_value(v) for v in value]
    if isinstance(value, tuple):
        return [_safe_value(v) for v in value]
    if isinstance(value, dict):
        return {str(k): _safe_value(v) for k, v in value.items()}
    try:
        text = repr(value)
    except Exception as exc:
        return f"<unrepresentable: {type(value).__name__}: {exc}>"
    if len(text) > _MAX_REPR:
        text = text[: _MAX_REPR - 1] + "\\u2026"
    return text


def _emit(envelope):
    sys.stdout.write(RESULT_MARKER + json.dumps(envelope, ensure_ascii=False) + "\\n")
    sys.stdout.flush()


def main():
    if SKILLS_ROOT not in sys.path:
        sys.path.insert(0, SKILLS_ROOT)

    # wasmsh's python3 builtin runs via PyRun_SimpleString and does not pass
    # argv (sys.argv is just [""]). The host uploads the user code to a
    # fixed VFS path and we read it from there instead.
    code_path = "/tmp/__wasmsh_repl_code.py"
    try:
        with open(code_path, "r", encoding="utf-8") as f:
            source = f.read()
    except OSError as exc:
        _emit({"ok": False, "error": "LauncherError", "message": str(exc)})
        return

    ns = _load_globals()

    try:
        tree = ast.parse(source, filename="<wasmsh>")
    except SyntaxError as exc:
        _emit({
            "ok": False,
            "error": "SyntaxError",
            "message": str(exc),
            "stdout": "",
            "stderr": "",
        })
        return

    last_expr_node = None
    if tree.body and isinstance(tree.body[-1], ast.Expr):
        last_expr_node = tree.body[-1].value
        tree.body = tree.body[:-1]

    stdout_buf = io.StringIO()
    stderr_buf = io.StringIO()
    real_stdout = sys.stdout
    real_stderr = sys.stderr
    sys.stdout = stdout_buf
    sys.stderr = stderr_buf

    envelope = {"ok": True, "stdout": "", "stderr": "", "value": None}
    try:
        if tree.body:
            exec(compile(tree, "<wasmsh>", "exec"), ns)
        if last_expr_node is not None:
            expr_code = compile(ast.Expression(last_expr_node), "<wasmsh>", "eval")
            value = eval(expr_code, ns)
            if value is not None:
                envelope["value"] = _safe_value(value)
    except SystemExit as sysexit:
        envelope.update(
            ok=False,
            error="SystemExit",
            message=str(sysexit.code) if sysexit.code is not None else "",
        )
    except BaseException as exc:
        envelope.update(
            ok=False,
            error=type(exc).__name__,
            message=str(exc),
            traceback=traceback.format_exc(),
        )
    finally:
        sys.stdout = real_stdout
        sys.stderr = real_stderr

    envelope["stdout"] = stdout_buf.getvalue()
    envelope["stderr"] = stderr_buf.getvalue()
    _save_globals(ns)
    _emit(envelope)


if __name__ == "__main__":
    main()
'''
"""Source of the launcher script, written verbatim into the sandbox VFS."""
