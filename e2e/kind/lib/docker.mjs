import { runCommand } from "./process.mjs";

export function createDocker() {
  async function run(args, options = {}) {
    return runCommand("docker", args, options);
  }

  return {
    run,

    async buildImage({ dockerfile, contextPath, tag, buildArgs = {} }) {
      const args = ["build", "-f", dockerfile, "-t", tag];
      for (const [key, value] of Object.entries(buildArgs)) {
        args.push("--build-arg", `${key}=${value}`);
      }
      args.push(contextPath);
      // Force BuildKit so <Dockerfile>.dockerignore next to the Dockerfile is
      // honoured instead of the context-root .dockerignore.
      return run(args, {
        env: { DOCKER_BUILDKIT: "1" },
        inherit: true,
        timeoutMs: 30 * 60 * 1000,
      });
    },

    async imageExists(tag) {
      const result = await run(["image", "inspect", tag], { allowedExitCodes: [0, 1] });
      return result.exitCode === 0;
    },
  };
}
