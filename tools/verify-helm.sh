#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHART_DIR="${ROOT_DIR}/deploy/helm/wasmsh"

cd "${ROOT_DIR}"

helm lint "${CHART_DIR}"
helm template wasmsh "${CHART_DIR}" >/tmp/wasmsh-default.yaml
helm template wasmsh "${CHART_DIR}" \
  --values "${CHART_DIR}/values-prod.yaml" \
  >/tmp/wasmsh-prod.yaml
helm template wasmsh "${CHART_DIR}" \
  --set autoscaling.enabled=false \
  --set monitoring.serviceMonitor.enabled=false \
  --set monitoring.prometheusRule.enabled=false \
  >/tmp/wasmsh-static.yaml

rg -q 'clusterIP: None' /tmp/wasmsh-default.yaml
rg -q 'type: ClusterIP' /tmp/wasmsh-default.yaml
rg -q 'value: "http://wasmsh-runner-headless:8787"' /tmp/wasmsh-default.yaml
rg -q 'path: /metrics' /tmp/wasmsh-prod.yaml
rg -q 'kind: NetworkPolicy' /tmp/wasmsh-default.yaml
rg -q 'kind: HorizontalPodAutoscaler' /tmp/wasmsh-default.yaml
rg -q 'name: wasmsh_inflight_restores' /tmp/wasmsh-default.yaml
rg -q 'targetLabels:' /tmp/wasmsh-prod.yaml
rg -Fq 'release="{{ .Release.Name }}"' "${CHART_DIR}/templates/prometheusrule.yaml"
rg -q 'kind: ServiceMonitor' /tmp/wasmsh-prod.yaml
rg -q 'kind: PrometheusRule' /tmp/wasmsh-prod.yaml
if rg -q 'DISPATCHER_POLICY' /tmp/wasmsh-default.yaml; then
  echo "dispatcher policy env should not be rendered" >&2
  exit 1
fi
if rg -q 'kind: HorizontalPodAutoscaler|kind: ServiceMonitor|kind: PrometheusRule' /tmp/wasmsh-static.yaml; then
  echo "static chart render unexpectedly contains autoscaling or monitoring resources" >&2
  exit 1
fi

echo "Helm chart validation passed:"
echo "  - helm lint"
echo "  - helm template (default values)"
echo "  - helm template (production values)"
echo "  - helm template (autoscaling + monitoring disabled)"
echo "  - rendered contract assertions"
