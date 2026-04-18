import { runCommand } from "./process.mjs";

export function createKubectl({ kubeconfig, context, namespace }) {
  const baseArgs = [];
  if (kubeconfig) baseArgs.push("--kubeconfig", kubeconfig);
  if (context) baseArgs.push("--context", context);
  if (namespace) baseArgs.push("--namespace", namespace);

  async function run(args, options = {}) {
    return runCommand("kubectl", [...baseArgs, ...args], options);
  }

  async function runJson(args, options = {}) {
    const { stdout } = await run([...args, "-o", "json"], options);
    return JSON.parse(stdout);
  }

  return {
    run,
    runJson,

    async applyInline(manifest) {
      return run(["apply", "-f", "-"], { input: manifest });
    },

    async getPods(selector) {
      return runJson(["get", "pods", "-l", selector]);
    },

    async getPodPhase(podName) {
      const pod = await runJson(["get", "pod", podName]);
      return pod.status?.phase;
    },

    async getPodReady(podName) {
      const pod = await runJson(["get", "pod", podName]);
      const conditions = pod.status?.conditions ?? [];
      const ready = conditions.find((c) => c.type === "Ready");
      return ready?.status === "True";
    },

    async getDeployment(name) {
      return runJson(["get", "deployment", name]);
    },

    async scaleDeployment(name, replicas) {
      return run(["scale", "deployment", name, `--replicas=${replicas}`]);
    },

    async rolloutStatus(kind, name, { timeoutMs = 180_000 } = {}) {
      return run(["rollout", "status", `${kind}/${name}`, `--timeout=${Math.floor(timeoutMs / 1000)}s`], {
        timeoutMs: timeoutMs + 10_000,
      });
    },

    async deletePod(podName, { grace } = {}) {
      const args = ["delete", "pod", podName];
      if (grace !== undefined) args.push(`--grace-period=${grace}`);
      return run(args);
    },

    async createNamespace(name) {
      try {
        await run(["create", "namespace", name]);
      } catch (error) {
        if (error.stderr?.includes("AlreadyExists")) {
          return { already: true };
        }
        throw error;
      }
      return { already: false };
    },

    async deleteNamespace(name) {
      try {
        await run(["delete", "namespace", name, "--ignore-not-found=true", "--timeout=60s"]);
      } catch (error) {
        if (!error.stderr?.includes("not found")) throw error;
      }
    },

    async waitForCondition(kind, name, condition, { timeoutMs = 120_000 } = {}) {
      return run(
        ["wait", `${kind}/${name}`, `--for=condition=${condition}`, `--timeout=${Math.floor(timeoutMs / 1000)}s`],
        { timeoutMs: timeoutMs + 10_000 },
      );
    },
  };
}

export async function waitUntil(predicate, { intervalMs = 1000, timeoutMs = 120_000, description = "condition" } = {}) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const result = await predicate();
      if (result) return result;
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }
  const hint = lastError ? ` (last error: ${lastError.message})` : "";
  throw new Error(`timed out after ${timeoutMs}ms waiting for ${description}${hint}`);
}
