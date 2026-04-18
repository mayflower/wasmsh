{{- define "wasmsh.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "wasmsh.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := include "wasmsh.name" . -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "wasmsh.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" -}}
{{- end -}}

{{- define "wasmsh.labels" -}}
helm.sh/chart: {{ include "wasmsh.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/part-of: wasmsh
release: {{ .Release.Name }}
{{- with .Values.commonLabels }}
{{ toYaml . }}
{{- end }}
{{- end -}}

{{- define "wasmsh.selectorLabels" -}}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/part-of: wasmsh
{{- end -}}

{{- define "wasmsh.componentName" -}}
{{- printf "%s-%s" (include "wasmsh.fullname" .root) .component | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "wasmsh.componentLabels" -}}
{{ include "wasmsh.labels" .root }}
app.kubernetes.io/name: {{ include "wasmsh.componentName" . }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{- define "wasmsh.componentSelectorLabels" -}}
{{ include "wasmsh.selectorLabels" .root }}
app.kubernetes.io/name: {{ include "wasmsh.componentName" . }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{- define "wasmsh.image" -}}
{{- $repo := required "image.repository is required" .repository -}}
{{- if .digest -}}
{{- printf "%s@%s" $repo .digest -}}
{{- else -}}
{{- printf "%s:%s" $repo (default "latest" .tag) -}}
{{- end -}}
{{- end -}}

{{- define "wasmsh.runnerServiceUrl" -}}
{{- printf "http://%s:%v" (include "wasmsh.runnerHeadlessServiceName" .) .Values.runner.service.port -}}
{{- end -}}

{{- define "wasmsh.runnerHeadlessServiceName" -}}
{{- printf "%s-headless" (include "wasmsh.componentName" (dict "root" . "component" "runner")) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "wasmsh.dispatcherServiceName" -}}
{{- include "wasmsh.componentName" (dict "root" . "component" "dispatcher") -}}
{{- end -}}
