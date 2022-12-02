{{/*
Expand the name of the chart.
*/}}
{{- define "sdp-k8s-client.name" -}}
{{- .Chart.Name | trunc 63 }}
{{- end }}

{{/*
Create a default fully qualified app name.
Truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec)
*/}}
{{- define "sdp-k8s-client.fullname" -}}
{{- printf "%s" .Release.Name | trunc 63 }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "sdp-k8s-client.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "sdp-k8s-client.labels" -}}
helm.sh/chart: {{ include "sdp-k8s-client.chart" . }}
{{ include "sdp-k8s-client.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "sdp-k8s-client.selectorLabels" -}}
app.kubernetes.io/name: {{ include "sdp-k8s-client.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
ServiceAccount
*/}}
{{- define "sdp-k8s-client.serviceAccountName" -}}
{{- include "sdp-k8s-client.fullname" .}}
{{- end }}

{{/*
Secret
*/}}
{{- define "sdp-k8s-client.injector-ca-crt" -}}
{{- printf "sdp-client-ca-crt-%s" .Release.Name }}
{{- end }}

{{- define "sdp-k8s-client.injector-secret" -}}
{{- printf "sdp-client-secret-%s" .Release.Name }}
{{- end }}

{{- define "sdp-k8s-client.injector-certificate" -}}
{{- printf "sdp-client-certificate-%s" .Release.Name }}
{{- end }}

{{- define "sdp-k8s-client.injector-issuer" -}}
{{- printf "sdp-client-issuer-%s" .Release.Name }}
{{- end }}

{{/*
Sidecar Config
*/}}
{{- define "sdp-k8s-client.sidecar-config" -}}
{{- printf "sdp-sidecar-config-%s" .Release.Name }}
{{- end }}

{{/*
Default app version
*/}}
{{- define "sdp-k8s-client.defaultTag" -}}
  {{- default .Chart.AppVersion .Values.global.image.tag }}
{{- end -}}

{{/*
Namespace
*/}}
{{- define "sdp-k8s-client.namespace" -}}
{{- if eq .Release.Namespace "default" }}
{{- print "sdp-system" }}
{{- else}}
{{- .Release.Namespace }}
{{- end }}
{{- end -}}
