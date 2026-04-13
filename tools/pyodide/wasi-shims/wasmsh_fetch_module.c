/*
 * _wasmsh_fetch — CPython extension module for host-backed HTTP fetch.
 *
 * Provides a Python-callable fetch(url, method="GET") that routes through
 * the C-level wasmsh_js_http_fetch → __wasmsh_host_fetch host import.
 * This lets micropip and Python code make HTTP requests without sockets.
 *
 * Registered as a built-in module via PyImport_AppendInittab before
 * Py_Initialize, so `import _wasmsh_fetch` works immediately.
 */
#include <Python.h>
#include <stdlib.h>
#include <string.h>

/* Provided by standalone_shim.c, routes through the host import. */
extern char* wasmsh_js_http_fetch(
    const char* url,
    const char* method,
    const char* headers_json,
    const unsigned char* body,
    unsigned int body_len,
    int follow_redirects
);

/* _wasmsh_fetch.fetch(url, method="GET") → str (JSON response) */
static PyObject* fetch_impl(PyObject* self, PyObject* args) {
    (void)self;
    const char* url;
    const char* method = "GET";
    if (!PyArg_ParseTuple(args, "s|s", &url, &method))
        return NULL;

    char* response = wasmsh_js_http_fetch(url, method, "[]", NULL, 0, 1);
    if (!response)
        Py_RETURN_NONE;

    PyObject* result = PyUnicode_FromString(response);
    free(response);
    return result;
}

static PyMethodDef fetch_methods[] = {
    {"fetch", fetch_impl, METH_VARARGS,
     "HTTP fetch via host import. Returns JSON string with status + body_base64."},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef fetch_module_def = {
    PyModuleDef_HEAD_INIT,
    "_wasmsh_fetch",
    "Host-backed HTTP fetch for the standalone WASI runtime.",
    -1,
    fetch_methods
};

PyMODINIT_FUNC PyInit__wasmsh_fetch(void) {
    return PyModule_Create(&fetch_module_def);
}

/* Register as built-in module during _initialize (before Py_Initialize). */
__attribute__((constructor))
static void register_wasmsh_fetch(void) {
    PyImport_AppendInittab("_wasmsh_fetch", PyInit__wasmsh_fetch);
}
