{{/*
Expand the name of the chart.
*/}}
{{- define "sdp-k8s-client.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
We truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec).
If release name contains chart name it will be used as a full name.
*/}}
{{- define "sdp-k8s-client.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
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
{{- if .Values.serviceAccount.create }}
{{- default (include "sdp-k8s-client.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
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
