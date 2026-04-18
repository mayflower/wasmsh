import { runCommand } from "./process.mjs";

export function createHelm({ kubeconfig, context, namespace }) {
  const baseArgs = [];
  if (kubeconfig) baseArgs.push("--kubeconfig", kubeconfig);
  if (context) baseArgs.push("--kube-context", context);
  if (namespace) baseArgs.push("--namespace", namespace);

  async function run(args, options = {}) {
    return runCommand("helm", [...baseArgs, ...args], options);
  }

  return {
    run,

    async install(release, chartPath, { values, setFlags = [], wait = true, timeoutMs = 300_000 } = {}) {
      const args = ["upgrade", "--install", release, chartPath, "--create-namespace"];
      if (values) {
        for (const valuesFile of [].concat(values)) {
          args.push("--values", valuesFile);
        }
      }
      for (const flag of setFlags) {
        args.push("--set", flag);
      }
      if (wait) {
        args.push("--wait", `--timeout=${Math.floor(timeoutMs / 1000)}s`);
      }
      return run(args, { timeoutMs: timeoutMs + 10_000 });
    },

    async uninstall(release, { waitMs = 60_000 } = {}) {
      try {
        await run(["uninstall", release, `--timeout=${Math.floor(waitMs / 1000)}s`]);
      } catch (error) {
        if (!error.stderr?.includes("not found")) throw error;
      }
    },

    async template(release, chartPath, { values, setFlags = [] } = {}) {
      const args = ["template", release, chartPath];
      if (values) {
        for (const valuesFile of [].concat(values)) {
          args.push("--values", valuesFile);
        }
      }
      for (const flag of setFlags) {
        args.push("--set", flag);
      }
      const { stdout } = await run(args);
      return stdout;
    },
  };
}
