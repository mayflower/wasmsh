const BLOCKED_GLOBALS = Object.freeze([
  "require",
  "process",
  "Deno",
  "fetch",
  "WebSocket",
]);

export function createSandboxGlobalSurface(source = globalThis) {
  const surface = {};
  for (const name of Object.getOwnPropertyNames(source)) {
    if (BLOCKED_GLOBALS.includes(name)) {
      continue;
    }
    surface[name] = source[name];
  }
  return surface;
}

export function captureScopedGlobals(names, source = globalThis) {
  return names.map((name) => ({
    name,
    existed: Object.prototype.hasOwnProperty.call(source, name),
    value: source[name],
  }));
}

export function restoreScopedGlobals(snapshot, source = globalThis) {
  for (const entry of snapshot) {
    if (entry.existed) {
      source[entry.name] = entry.value;
    } else {
      delete source[entry.name];
    }
  }
}
