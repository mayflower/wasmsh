/**
 * Install a quota-enforcing wrapper around Pyodide's `Module.FS`.
 *
 * The wasmsh shell talks to the Pyodide VFS through the Rust
 * EmscriptenFs facade; Python code talks to the same VFS directly via
 * libc through `open()`, `write()`, `os.mkdir()`, etc. Both paths
 * eventually reach Emscripten's FS layer, which is the only place a
 * single guard can catch every write.
 *
 * The membrane (audit F6) enforces:
 * - Per-file byte cap: a single file cannot exceed MAX_FILE_BYTES.
 * - Aggregate byte cap: the sum of file sizes under `MOUNT_ROOT`
 *   cannot exceed MAX_TOTAL_BYTES.
 * - Inode cap: the number of files and directories under `MOUNT_ROOT`
 *   cannot exceed MAX_INODES.
 *
 * Limits are conservative defaults that mirror the MemoryFs constants
 * in `crates/wasmsh-fs/src/memfs.rs`. Hosts can override at install
 * time.
 *
 * Idempotency: re-installing is a no-op unless the limits change, in
 * which case the existing wrappers stay in place and only the cap
 * fields are updated. The wrappers close over the limits via a
 * shared state object so updates propagate without re-wrapping.
 */

const MEMBRANE_FLAG = Symbol.for("wasmsh.fs-membrane");

const DEFAULTS = {
  maxFileBytes: 64 * 1024 * 1024,
  maxTotalBytes: 256 * 1024 * 1024,
  maxInodes: 100_000,
  mountRoot: "/workspace",
};

function pathInRoot(path, root) {
  if (typeof path !== "string") return false;
  if (root === "/") return true;
  return path === root || path.startsWith(`${root}/`);
}

function nodeSize(node) {
  // Emscripten MEMFS files use `node.usedBytes` for the live byte
  // count; some versions store `node.contents.length`. Read both.
  if (!node) return 0;
  if (typeof node.usedBytes === "number") return node.usedBytes;
  if (node.contents && typeof node.contents.length === "number") {
    return node.contents.length;
  }
  return 0;
}

function isDir(FS, node) {
  return node && typeof FS.isDir === "function" && FS.isDir(node.mode);
}

function snapshotState(FS, root) {
  // Walk the tree under root once to seed the byte / inode counters.
  // Cheap on a fresh session (workspace is empty); for restored
  // snapshots it costs O(N) but only at install time.
  let totalBytes = 0;
  let inodes = 0;
  const stack = [root];
  while (stack.length) {
    const path = stack.pop();
    let node;
    try {
      node = FS.lookupPath(path, { follow: false }).node;
    } catch {
      continue;
    }
    inodes += 1;
    if (isDir(FS, node)) {
      let children;
      try {
        children = FS.readdir(path);
      } catch {
        children = [];
      }
      for (const child of children) {
        if (child === "." || child === "..") continue;
        stack.push(path === "/" ? `/${child}` : `${path}/${child}`);
      }
    } else {
      totalBytes += nodeSize(node);
    }
  }
  return { totalBytes, inodes };
}

class QuotaExceededError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "WasmshQuotaExceededError";
    this.code = code;
  }
}

export function installFsMembrane(FS, options = {}) {
  if (!FS || typeof FS !== "object") {
    return null;
  }
  const config = { ...DEFAULTS, ...options };
  const existing = FS[MEMBRANE_FLAG];
  if (existing) {
    existing.config = config;
    return existing;
  }

  const seed = snapshotState(FS, config.mountRoot);
  const state = {
    config,
    totalBytes: seed.totalBytes,
    inodes: seed.inodes,
  };

  const originalWrite = FS.write?.bind(FS);
  const originalWriteFile = FS.writeFile?.bind(FS);
  const originalCreate = FS.create?.bind(FS);
  const originalMkdir = FS.mkdir?.bind(FS);
  const originalUnlink = FS.unlink?.bind(FS);
  const originalRmdir = FS.rmdir?.bind(FS);
  const originalTruncate = FS.truncate?.bind(FS);

  function checkInodeRoom(path) {
    if (!pathInRoot(path, config.mountRoot)) return;
    if (state.inodes + 1 > config.maxInodes) {
      throw new QuotaExceededError(
        "WASMSH_INODE_LIMIT",
        `wasmsh: filesystem inode limit exceeded (${config.maxInodes})`,
      );
    }
  }

  function projectBytesDelta(node, delta, pathHint) {
    // Returns the new total if accepted; throws otherwise. `delta`
    // can be negative (truncate to 0, unlink).
    if (!pathHint || !pathInRoot(pathHint, config.mountRoot)) {
      return state.totalBytes + delta;
    }
    const projected = Math.max(0, state.totalBytes + delta);
    if (projected > config.maxTotalBytes) {
      throw new QuotaExceededError(
        "WASMSH_TOTAL_BYTES_LIMIT",
        `wasmsh: filesystem quota exceeded (${projected} > ${config.maxTotalBytes})`,
      );
    }
    const futureFileSize = nodeSize(node) + delta;
    if (futureFileSize > config.maxFileBytes) {
      throw new QuotaExceededError(
        "WASMSH_FILE_BYTES_LIMIT",
        `wasmsh: per-file size limit exceeded (${futureFileSize} > ${config.maxFileBytes})`,
      );
    }
    return projected;
  }

  if (originalCreate) {
    FS.create = function membraneCreate(path, mode) {
      checkInodeRoom(path);
      const node = originalCreate(path, mode);
      if (pathInRoot(path, config.mountRoot)) state.inodes += 1;
      return node;
    };
  }

  if (originalMkdir) {
    FS.mkdir = function membraneMkdir(path, mode) {
      checkInodeRoom(path);
      const node = originalMkdir(path, mode);
      if (pathInRoot(path, config.mountRoot)) state.inodes += 1;
      return node;
    };
  }

  if (originalWrite) {
    FS.write = function membraneWrite(stream, buffer, offset, length, position, canOwn) {
      const node = stream?.node;
      const path = stream?.path;
      const newTotal = projectBytesDelta(node, length, path);
      const wrote = originalWrite(stream, buffer, offset, length, position, canOwn);
      // Recompute size after the write so we reflect what actually landed.
      const after = nodeSize(node);
      const before = after - wrote;
      if (pathInRoot(path, config.mountRoot)) {
        state.totalBytes = Math.max(0, state.totalBytes + (after - before));
      } else {
        // path outside our root; leave totalBytes untouched
        void newTotal;
      }
      return wrote;
    };
  }

  if (originalWriteFile) {
    FS.writeFile = function membraneWriteFile(path, data, opts) {
      // Compute the projected delta against any pre-existing file.
      let oldSize = 0;
      let existed = false;
      try {
        const node = FS.lookupPath(path, { follow: false }).node;
        existed = true;
        oldSize = nodeSize(node);
      } catch { /* new file */ }
      const newSize = data?.length ?? data?.byteLength ?? 0;
      if (pathInRoot(path, config.mountRoot)) {
        if (newSize > config.maxFileBytes) {
          throw new QuotaExceededError(
            "WASMSH_FILE_BYTES_LIMIT",
            `wasmsh: per-file size limit exceeded (${newSize} > ${config.maxFileBytes})`,
          );
        }
        const projected = state.totalBytes - oldSize + newSize;
        if (projected > config.maxTotalBytes) {
          throw new QuotaExceededError(
            "WASMSH_TOTAL_BYTES_LIMIT",
            `wasmsh: filesystem quota exceeded (${projected} > ${config.maxTotalBytes})`,
          );
        }
        if (!existed) checkInodeRoom(path);
      }
      const result = originalWriteFile(path, data, opts);
      if (pathInRoot(path, config.mountRoot)) {
        if (!existed) state.inodes += 1;
        state.totalBytes = state.totalBytes - oldSize + newSize;
      }
      return result;
    };
  }

  if (originalTruncate) {
    FS.truncate = function membraneTruncate(path, len) {
      let oldSize = 0;
      try {
        const node = FS.lookupPath(path, { follow: false }).node;
        oldSize = nodeSize(node);
      } catch { /* not found; let original raise */ }
      const result = originalTruncate(path, len);
      if (pathInRoot(path, config.mountRoot)) {
        const delta = (len ?? 0) - oldSize;
        state.totalBytes = Math.max(0, state.totalBytes + delta);
      }
      return result;
    };
  }

  if (originalUnlink) {
    FS.unlink = function membraneUnlink(path) {
      let size = 0;
      try {
        const node = FS.lookupPath(path, { follow: false }).node;
        size = nodeSize(node);
      } catch { /* let original raise */ }
      const result = originalUnlink(path);
      if (pathInRoot(path, config.mountRoot)) {
        state.totalBytes = Math.max(0, state.totalBytes - size);
        state.inodes = Math.max(0, state.inodes - 1);
      }
      return result;
    };
  }

  if (originalRmdir) {
    FS.rmdir = function membraneRmdir(path) {
      const result = originalRmdir(path);
      if (pathInRoot(path, config.mountRoot)) {
        state.inodes = Math.max(0, state.inodes - 1);
      }
      return result;
    };
  }

  Object.defineProperty(FS, MEMBRANE_FLAG, {
    value: state,
    writable: false,
    enumerable: false,
    configurable: true,
  });
  return state;
}
