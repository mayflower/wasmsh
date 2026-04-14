#include <Python.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

typedef void (*wasmsh_sighandler_t)(int);

struct tms {
    clock_t tms_utime;
    clock_t tms_stime;
    clock_t tms_cutime;
    clock_t tms_cstime;
};

static wasmsh_sighandler_t wasmsh_signal_handlers[64];
static char wasmsh_python_init_error[512];

PyObject* PyInit__sqlite3(void);

static void wasmsh_set_python_init_error(const char* message) {
  if (message == NULL || message[0] == '\0') {
    message = "unknown CPython initialization error";
  }
  snprintf(
      wasmsh_python_init_error,
      sizeof(wasmsh_python_init_error),
      "%s",
      message);
}

const char* wasmsh_python_initialize_error(void) {
  return wasmsh_python_init_error;
}

int wasmsh_python_initialize(void) {
  if (Py_IsInitialized()) {
    wasmsh_python_init_error[0] = '\0';
    return 0;
  }

  PyStatus status;
  PyConfig config;
  PyConfig_InitPythonConfig(&config);

  config.use_environment = 0;
  config.isolated = 1;
  config.site_import = 0;
  config.safe_path = 1;
  config.write_bytecode = 0;
  config.user_site_directory = 0;
  config.module_search_paths_set = 1;

  status = PyConfig_SetString(&config, &config.home, L"/");
  if (PyStatus_Exception(status)) {
    wasmsh_set_python_init_error(status.err_msg);
    PyConfig_Clear(&config);
    return -1;
  }

  status = PyConfig_SetString(&config, &config.program_name, L"/python3");
  if (PyStatus_Exception(status)) {
    wasmsh_set_python_init_error(status.err_msg);
    PyConfig_Clear(&config);
    return -1;
  }

  status = PyWideStringList_Append(&config.module_search_paths, L"/Lib");
  if (PyStatus_Exception(status)) {
    wasmsh_set_python_init_error(status.err_msg);
    PyConfig_Clear(&config);
    return -1;
  }

  status = PyWideStringList_Append(&config.module_search_paths, L"/lib/python3.13");
  if (PyStatus_Exception(status)) {
    wasmsh_set_python_init_error(status.err_msg);
    PyConfig_Clear(&config);
    return -1;
  }

  if (PyImport_AppendInittab("_sqlite3", PyInit__sqlite3) == -1) {
    wasmsh_set_python_init_error("failed to register built-in _sqlite3");
    PyConfig_Clear(&config);
    return -1;
  }

  status = Py_InitializeFromConfig(&config);
  PyConfig_Clear(&config);
  if (PyStatus_Exception(status)) {
    wasmsh_set_python_init_error(status.err_msg);
    return -1;
  }

  wasmsh_python_init_error[0] = '\0';
  return 0;
}

void __SIG_IGN(int sig) {
  (void)sig;
}

void __SIG_ERR(int sig) {
    (void)sig;
}

wasmsh_sighandler_t signal(int sig, wasmsh_sighandler_t handler) {
    if (sig >= 0 && sig < (int)(sizeof(wasmsh_signal_handlers) / sizeof(wasmsh_signal_handlers[0]))) {
        wasmsh_sighandler_t previous = wasmsh_signal_handlers[sig];
        wasmsh_signal_handlers[sig] = handler;
        return previous;
    }
    return handler;
}

int raise(int sig) {
    wasmsh_sighandler_t handler = NULL;
    if (sig >= 0 && sig < (int)(sizeof(wasmsh_signal_handlers) / sizeof(wasmsh_signal_handlers[0]))) {
        handler = wasmsh_signal_handlers[sig];
    }
    if (handler != NULL) {
        handler(sig);
    }
    return 0;
}

pid_t getpid(void) {
    return 1;
}

int clock_gettime(clockid_t clock_id, struct timespec* tp) {
    (void)clock_id;
    if (tp == NULL) {
        errno = EINVAL;
        return -1;
    }
    if (timespec_get(tp, TIME_UTC) != TIME_UTC) {
        errno = EIO;
        return -1;
    }
    return 0;
}

clock_t times(struct tms* buf) {
  if (buf != NULL) {
    memset(buf, 0, sizeof(*buf));
  }
  return 0;
}

clock_t clock(void) {
  return 0;
}

char* strsignal(int sig) {
  (void)sig;
  return "unsupported signal";
}
