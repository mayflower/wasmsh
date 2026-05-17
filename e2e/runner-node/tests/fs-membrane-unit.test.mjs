// Unit tests for the Pyodide FS membrane.
//
// The membrane wraps Emscripten's `Module.FS` so per-file, total-bytes,
// and inode quotas are enforced for both the shell (via EmscriptenFs)
// and direct Python `open()` access. These tests exercise the
// wrapper against a minimal FS double so the assertions stay
// independent of a real Pyodide boot.

import { test } from "node:test";
import assert from "node:assert/strict";

import { installFsMembrane } from "../../../packages/npm/wasmsh-pyodide/lib/fs-membrane.mjs";

/** Minimal stand-in for Emscripten's `Module.FS`. Backs files in a Map. */
function makeFakeFs() {
  const nodes = new Map(); // path -> { contents: Uint8Array, isDir: boolean }
  nodes.set("/", { isDir: true });
  nodes.set("/workspace", { isDir: true });

  function ensure(path) {
    if (!nodes.has(path)) throw new Error(`ENOENT ${path}`);
    return nodes.get(path);
  }

  return {
    isDir(mode) { return mode === 0o40000; },
    lookupPath(path) {
      const n = ensure(path);
      return {
        node: {
          mode: n.isDir ? 0o40000 : 0o100644,
          usedBytes: n.contents ? n.contents.length : 0,
          contents: n.contents,
        },
      };
    },
    readdir(path) {
      const n = ensure(path);
      if (!n.isDir) throw new Error(`ENOTDIR ${path}`);
      const prefix = path === "/" ? "/" : `${path}/`;
      const out = [];
      for (const p of nodes.keys()) {
        if (p === path) continue;
        if (!p.startsWith(prefix)) continue;
        const rest = p.slice(prefix.length);
        if (rest.includes("/")) continue;
        out.push(rest);
      }
      return out;
    },
    create(path /* , mode */) {
      nodes.set(path, { isDir: false, contents: new Uint8Array(0) });
    },
    mkdir(path /* , mode */) {
      nodes.set(path, { isDir: true });
    },
    writeFile(path, data /* , opts */) {
      const buf = data instanceof Uint8Array ? data : new TextEncoder().encode(String(data));
      nodes.set(path, { isDir: false, contents: buf });
    },
    write(stream, _buffer, _offset, length /* , _position, _canOwn */) {
      // The fake's write contract: the membrane's wrapper expects the
      // node's usedBytes to reflect the new size *after* originalWrite.
      const newSize = stream.node.usedBytes + length;
      stream.node.usedBytes = newSize;
      // Mirror back into the map so subsequent lookupPath sees it.
      const file = nodes.get(stream.path);
      if (file && !file.isDir) {
        const filled = new Uint8Array(newSize);
        filled.set(file.contents ?? new Uint8Array(0));
        file.contents = filled;
      }
      return length;
    },
    truncate(path, len) {
      const node = ensure(path);
      const buf = new Uint8Array(len ?? 0);
      buf.set((node.contents ?? new Uint8Array(0)).subarray(0, buf.length));
      node.contents = buf;
    },
    unlink(path) { nodes.delete(path); },
    rmdir(path) { nodes.delete(path); },
    _nodes: nodes,
  };
}

test("writeFile cap blocks files larger than maxFileBytes", () => {
  const FS = makeFakeFs();
  installFsMembrane(FS, {
    maxFileBytes: 64,
    maxTotalBytes: 1024,
    maxInodes: 1024,
  });
  assert.throws(
    () => FS.writeFile("/workspace/big.bin", new Uint8Array(200)),
    (e) => e.code === "WASMSH_FILE_BYTES_LIMIT",
  );
});

test("aggregate cap blocks growth across many files", () => {
  const FS = makeFakeFs();
  installFsMembrane(FS, {
    maxFileBytes: 64,
    maxTotalBytes: 100,
    maxInodes: 1024,
  });
  FS.writeFile("/workspace/a", new Uint8Array(60));
  assert.throws(
    () => FS.writeFile("/workspace/b", new Uint8Array(60)),
    (e) => e.code === "WASMSH_TOTAL_BYTES_LIMIT",
  );
  // After unlinking one, the freed budget allows a new write.
  FS.unlink("/workspace/a");
  FS.writeFile("/workspace/b", new Uint8Array(60));
});

test("inode cap blocks explosive file creation", () => {
  const FS = makeFakeFs();
  // The fake starts with /workspace (1 inode under mountRoot). Cap at 2
  // means one user-created file allowed; the second must fail.
  installFsMembrane(FS, {
    maxFileBytes: 1024,
    maxTotalBytes: 1024 * 1024,
    maxInodes: 2,
  });
  FS.create("/workspace/f1");
  assert.throws(
    () => FS.create("/workspace/f2"),
    (e) => e.code === "WASMSH_INODE_LIMIT",
  );
});

test("writes outside the membrane root are unaffected", () => {
  const FS = makeFakeFs();
  installFsMembrane(FS, {
    maxFileBytes: 1,
    maxTotalBytes: 1,
    maxInodes: 1024,
    mountRoot: "/workspace",
  });
  // /tmp is outside /workspace — should NOT trip the caps.
  FS.mkdir("/tmp");
  FS.writeFile("/tmp/out.bin", new Uint8Array(128));
});

test("write() through a stream is counted toward the aggregate cap", () => {
  const FS = makeFakeFs();
  installFsMembrane(FS, {
    maxFileBytes: 1024,
    maxTotalBytes: 100,
    maxInodes: 1024,
  });
  FS.create("/workspace/log");
  const stream = {
    path: "/workspace/log",
    node: FS.lookupPath("/workspace/log").node,
  };
  // First write fits.
  FS.write(stream, null, 0, 60, 0, false);
  // Second write would push past the 100-byte aggregate cap.
  assert.throws(
    () => FS.write(stream, null, 0, 60, 0, false),
    (e) => e.code === "WASMSH_TOTAL_BYTES_LIMIT",
  );
});

test("unlink restores both the byte and inode budgets", () => {
  const FS = makeFakeFs();
  // /workspace seed = 1 inode. Cap at 3 leaves room for two user files.
  installFsMembrane(FS, {
    maxFileBytes: 1024,
    maxTotalBytes: 200,
    maxInodes: 3,
  });
  FS.writeFile("/workspace/a", new Uint8Array(150));
  FS.writeFile("/workspace/b", new Uint8Array(40));
  // No room for a third file (inode cap).
  assert.throws(
    () => FS.writeFile("/workspace/c", new Uint8Array(1)),
    (e) => e.code === "WASMSH_INODE_LIMIT",
  );
  FS.unlink("/workspace/a");
  // Now both bytes and inodes are freed; the next write succeeds.
  FS.writeFile("/workspace/c", new Uint8Array(150));
});
