export function composeWasmImports({ imports = {}, env = {}, sentinel = {} } = {}) {
  return {
    ...imports,
    env: {
      ...(imports.env ?? {}),
      ...env,
    },
    sentinel: {
      ...(imports.sentinel ?? {}),
      ...sentinel,
    },
  };
}
