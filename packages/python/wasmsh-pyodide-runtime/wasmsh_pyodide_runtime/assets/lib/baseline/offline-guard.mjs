export function assertOfflineBaselineBootPlan(plan) {
  if (!plan || typeof plan !== "object") {
    throw new Error("baseline boot plan is required");
  }
  if (plan.network_required !== false) {
    throw new Error("baseline boot must not require network access");
  }
  if ((plan.optional_python_packages?.length ?? 0) > 0) {
    throw new Error("baseline boot must not preload optional Python packages");
  }
  if ((plan.dynamic_install_steps?.length ?? 0) > 0) {
    throw new Error("baseline boot must not perform dynamic install steps");
  }
  return plan;
}
