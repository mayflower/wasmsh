export function assertWorkspaceEmpty(module) {
  const entries = module.FS.readdir("/workspace").filter((entry) => entry !== "." && entry !== "..");
  if (entries.length > 0) {
    throw new Error(`/workspace must be empty before snapshotting (found: ${entries.join(", ")})`);
  }
}

export function buildEntropyContract() {
  return {
    deterministic_entropy: true,
    reseed_required_on_restore: true,
    baseline_entropy_source: "snapshot-build",
  };
}
