import { mkdir, rm } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { createDocker } from "./docker.mjs";
import { createHelm } from "./helm.mjs";
import { createKind } from "./kind.mjs";
import { createKubectl, waitUntil } from "./kubectl.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = resolve(HERE, "..", "..", "..");
export const E2E_ROOT = resolve(HERE, "..");

export const CLUSTER_NAME = "wasmsh-e2e";
export const NAMESPACE = "wasmsh";
export const HELM_RELEASE = "wasmsh";
export const DISPATCHER_IMAGE = "wasmsh-dispatcher:e2e";
export const RUNNER_IMAGE = "wasmsh-runner:e2e";

export function createClusterTooling() {
  const kubeconfig = resolve(E2E_ROOT, ".artifacts", "kubeconfig");
  const context = `kind-${CLUSTER_NAME}`;
  const docker = createDocker();
  const kind = createKind({ kubeconfig });
  const kubectl = createKubectl({ kubeconfig, context, namespace: NAMESPACE });
  const helm = createHelm({ kubeconfig, context, namespace: NAMESPACE });
  return { kubeconfig, context, docker, kind, kubectl, helm };
}

export async function buildImages({ docker, skipExisting = false } = {}) {
  const dock = docker ?? createDocker();
  const steps = [
    {
      tag: DISPATCHER_IMAGE,
      dockerfile: resolve(REPO_ROOT, "deploy/docker/Dockerfile.dispatcher"),
    },
    {
      tag: RUNNER_IMAGE,
      dockerfile: resolve(REPO_ROOT, "deploy/docker/Dockerfile.runner"),
    },
  ];
  for (const step of steps) {
    if (skipExisting && (await dock.imageExists(step.tag))) {
      continue;
    }
    await dock.buildImage({
      dockerfile: step.dockerfile,
      contextPath: REPO_ROOT,
      tag: step.tag,
    });
  }
}

export async function setupCluster({ reuseExisting = false, buildImagesIfMissing = true } = {}) {
  const tooling = createClusterTooling();
  await mkdir(dirname(tooling.kubeconfig), { recursive: true });

  const clusters = await tooling.kind.listClusters();
  const exists = clusters.includes(CLUSTER_NAME);
  if (exists && !reuseExisting) {
    await tooling.kind.deleteCluster(CLUSTER_NAME);
  }
  if (!exists || !reuseExisting) {
    await tooling.kind.createCluster({
      name: CLUSTER_NAME,
      configPath: resolve(E2E_ROOT, "kind-config.yaml"),
    });
  }
  await tooling.kind.exportKubeconfig(CLUSTER_NAME, tooling.kubeconfig);

  await buildImages({ docker: tooling.docker, skipExisting: buildImagesIfMissing });
  await tooling.kind.loadImage(CLUSTER_NAME, DISPATCHER_IMAGE);
  await tooling.kind.loadImage(CLUSTER_NAME, RUNNER_IMAGE);

  await tooling.kubectl.createNamespace(NAMESPACE);

  await tooling.helm.install(HELM_RELEASE, resolve(REPO_ROOT, "deploy/helm/wasmsh"), {
    values: resolve(E2E_ROOT, "values-e2e.yaml"),
    wait: true,
    timeoutMs: 10 * 60 * 1000,
  });

  await waitUntil(
    async () => {
      const pods = await tooling.kubectl.getPods("app.kubernetes.io/component=runner");
      if (pods.items.length === 0) return false;
      return pods.items.every((pod) => {
        const ready = pod.status?.conditions?.find((c) => c.type === "Ready");
        return ready?.status === "True";
      });
    },
    { intervalMs: 2000, timeoutMs: 10 * 60 * 1000, description: "runner pods Ready" },
  );

  return tooling;
}

export async function teardownCluster({ keep = false } = {}) {
  if (keep) return;
  const tooling = createClusterTooling();
  try {
    await tooling.kind.deleteCluster(CLUSTER_NAME);
  } finally {
    await rm(dirname(tooling.kubeconfig), { recursive: true, force: true });
  }
}
