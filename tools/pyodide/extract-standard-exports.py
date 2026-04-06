#!/usr/bin/env python3
"""Extract function export names from a standard Pyodide wasm build.

Reads pyodide.asm.wasm (from the CDN or a standard build) and writes
all function export names to stdout, one per line, prefixed with '_'.

This list is used with MAIN_MODULE=2 to explicitly export the same
symbols that MAIN_MODULE=1 would export automatically.  The custom
wasmsh build uses MAIN_MODULE=2 to avoid exporting Rust symbols that
contain '$' in their mangled names.  Adding the standard export list
gives compiled Python side modules (DuckDB, numpy, etc.) access to
the CPython / libc / C++ symbols they need to load.

Usage: python3 extract-standard-exports.py pyodide.asm.wasm > exports.txt
"""

import sys


def read_leb128(data, pos):
    result = 0
    shift = 0
    while True:
        byte = data[pos]
        pos += 1
        result |= (byte & 0x7F) << shift
        shift += 7
        if not (byte & 0x80):
            break
    return result, pos


def extract_function_exports(wasm_path):
    with open(wasm_path, "rb") as f:
        data = f.read()

    if data[:4] != b"\x00asm":
        print(f"error: {wasm_path} is not a wasm file", file=sys.stderr)
        sys.exit(1)

    pos = 8
    while pos < len(data):
        section_id = data[pos]
        pos += 1
        section_size, pos = read_leb128(data, pos)
        section_end = pos + section_size

        if section_id == 7:  # Export section
            count, pos = read_leb128(data, pos)
            for _ in range(count):
                name_len, pos = read_leb128(data, pos)
                name = data[pos : pos + name_len].decode("utf-8")
                pos += name_len
                kind = data[pos]
                pos += 1
                _idx, pos = read_leb128(data, pos)
                if kind == 0 and "$" not in name:  # function, no $
                    yield name
            return

        pos = section_end


def main():
    if len(sys.argv) != 2:
        print(f"Usage: {sys.argv[0]} <pyodide.asm.wasm>", file=sys.stderr)
        sys.exit(1)

    for name in sorted(extract_function_exports(sys.argv[1])):
        # Emscripten EXPORTED_FUNCTIONS expects '_' prefix
        if not name.startswith("_"):
            print(f"_{name}")
        else:
            print(name)


if __name__ == "__main__":
    main()
