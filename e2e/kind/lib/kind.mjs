import { runCommand } from "./process.mjs";

export function createKind({ kubeconfig } = {}) {
  async function run(args, options = {}) {
    return runCommand("kind", args, options);
  }

  return {
    run,

    async listClusters() {
      const { stdout } = await run(["get", "clusters"]);
      return stdout
        .split("\n")
        .map((entry) => entry.trim())
        .filter(Boolean);
    },

    async createCluster({ name, configPath, imageRef } = {}) {
      const args = ["create", "cluster", "--name", name, "--wait", "120s"];
      if (configPath) args.push("--config", configPath);
      if (imageRef) args.push("--image", imageRef);
      if (kubeconfig) args.push("--kubeconfig", kubeconfig);
      return run(args, { timeoutMs: 180_000 });
    },

    async deleteCluster(name) {
      const args = ["delete", "cluster", "--name", name];
      if (kubeconfig) args.push("--kubeconfig", kubeconfig);
      try {
        await run(args);
      } catch (error) {
        if (!error.stderr?.includes("no nodes found for cluster")) throw error;
      }
    },

    async loadImage(name, imageRef) {
      return run(["load", "docker-image", imageRef, "--name", name], { timeoutMs: 300_000 });
    },

    async exportKubeconfig(name, targetPath) {
      return run(["export", "kubeconfig", "--name", name, "--kubeconfig", targetPath]);
    },
  };
}
