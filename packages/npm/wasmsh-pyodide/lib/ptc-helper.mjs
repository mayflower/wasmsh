// PTC helper — Python source that runs inside Pyodide to drive one PTC eval.
//
// The host (node-host.mjs) loads this string into Pyodide's globals once per
// session, then on every `runPtc` request:
//   1. installs `__wasmsh_host_call` + a `tools` namespace as Python globals
//   2. calls `_wasmsh_run_ptc_block(code)` which returns a JSON envelope
//
// The helper itself owns the launcher concerns that the legacy file-based
// launcher script handled: globals pickle, stdout/stderr capture, last-expr
// REPL value, async top-level support.
export const PTC_HELPER_SOURCE = String.raw`
import ast
import asyncio
import builtins
import io
import json
import os
import pickle
import sys
import traceback

_GLOBALS_PATH = "/tmp/__wasmsh_repl_globals.pkl"
_MAX_REPR = 4000


def _load_globals():
    if not os.path.exists(_GLOBALS_PATH):
        return {"__name__": "__main__", "__builtins__": builtins}
    try:
        with open(_GLOBALS_PATH, "rb") as f:
            ns = pickle.load(f)
    except Exception:
        ns = {}
    ns["__name__"] = "__main__"
    ns["__builtins__"] = builtins
    return ns


# NOTE: keep this skip-list in sync with the file launcher in
# packages/python/langchain-wasmsh/langchain_wasmsh/_launcher.py (_SKIP_NAMES).
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
        with open(_GLOBALS_PATH, "wb") as f:
            pickle.dump(keep, f, protocol=pickle.HIGHEST_PROTOCOL)
    except Exception as exc:
        sys.stderr.write("[wasmsh] failed to persist globals: %s\n" % exc)


def _safe_value(value):
    # Trailing-expression coercion: primitives pass through; lists/dicts
    # recurse; anything else falls back to a truncated repr string.
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
        return "<unrepresentable: %s: %s>" % (type(value).__name__, exc)
    if len(text) > _MAX_REPR:
        text = text[: _MAX_REPR - 1] + "…"
    return text


def _build_wrapper(tree):
    # Wraps the module body in an async function so top-level await works.
    # If the last statement is an expression, it is converted to a return so
    # the coroutine's resolved value is the trailing expression's value.
    body = list(tree.body) if tree.body else [ast.Pass()]
    if isinstance(body[-1], ast.Expr):
        last = body[-1]
        body[-1] = ast.Return(value=last.value)
    func = ast.AsyncFunctionDef(
        name="_wasmsh_ptc_main",
        args=ast.arguments(
            posonlyargs=[], args=[], kwonlyargs=[], kw_defaults=[], defaults=[]
        ),
        body=body,
        decorator_list=[],
        returns=None,
    )
    module = ast.Module(body=[func], type_ignores=[])
    ast.fix_missing_locations(module)
    return module


async def _wasmsh_run_ptc_block(code):
    ns = _load_globals()
    # Make sure the model's "tools" / "__wasmsh_host_call" come from the
    # session-scoped pyodide globals; we copy refs into the run namespace.
    g = globals()
    for name in ("tools", "__wasmsh_host_call"):
        if name in g:
            ns[name] = g[name]

    try:
        tree = ast.parse(code, filename="<wasmsh>")
    except SyntaxError as exc:
        return json.dumps({
            "ok": False,
            "error": "SyntaxError",
            "message": str(exc),
            "stdout": "",
            "stderr": "",
        })

    wrapper_module = _build_wrapper(tree)

    stdout_buf, stderr_buf = io.StringIO(), io.StringIO()
    real_stdout, real_stderr = sys.stdout, sys.stderr
    sys.stdout, sys.stderr = stdout_buf, stderr_buf

    envelope = {"ok": True, "stdout": "", "stderr": "", "value": None}
    try:
        compiled = compile(wrapper_module, "<wasmsh>", "exec")
        exec(compiled, ns)
        value = await ns["_wasmsh_ptc_main"]()
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
        sys.stdout, sys.stderr = real_stdout, real_stderr

    envelope["stdout"] = stdout_buf.getvalue()
    envelope["stderr"] = stderr_buf.getvalue()
    _save_globals(ns)
    return json.dumps(envelope, ensure_ascii=False)


class _PtcTool:
    """Awaitable bridge for a single host-registered tool."""

    __slots__ = ("_name",)

    def __init__(self, name):
        self._name = name

    async def __call__(self, **kwargs):
        if "__wasmsh_host_call" not in globals():
            raise RuntimeError(
                "PTC bridge not installed; tools.* requires runPtc"
            )
        bridge = globals()["__wasmsh_host_call"]
        result = await bridge(self._name, kwargs)
        try:
            return result.to_py()
        except AttributeError:
            return result


class _PtcTools:
    def __init__(self, names):
        for name in names:
            setattr(self, name, _PtcTool(name))


def _wasmsh_install_tools(names):
    """Install the PTC tools namespace at module scope for this session."""
    g = globals()
    g["tools"] = _PtcTools(list(names))
`;
