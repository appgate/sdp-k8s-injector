{{/*
Expand the name of the chart.
*/}}
{{- define "sdp-injector.name" -}}
{{- .Chart.Name | trunc 63 }}
{{- end }}

{{/*
Create a default fully qualified app name.
Truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec)
*/}}
{{- define "sdp-injector.fullname" -}}
{{- printf "%s" .Release.Name | trunc 63 }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "sdp-injector.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "sdp-injector.labels" -}}
helm.sh/chart: {{ include "sdp-injector.chart" . }}
{{ include "sdp-injector.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "sdp-injector.selectorLabels" -}}
app.kubernetes.io/name: {{ include "sdp-injector.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
ServiceAccount
*/}}
{{- define "sdp-injector.serviceAccountName" -}}
{{- include "sdp-injector.fullname" .}}
{{- end }}

{{/*
Secret
*/}}
{{- define "sdp-injector.injector-ca-crt" -}}
{{- printf "sdp-injector-ca-crt-%s" .Release.Name }}
{{- end }}

{{- define "sdp-injector.injector-secret" -}}
{{- printf "sdp-injector-secret-%s" .Release.Name }}
{{- end }}

{{- define "sdp-injector.injector-certificate" -}}
{{- printf "sdp-injector-certificate-%s" .Release.Name }}
{{- end }}

{{- define "sdp-injector.injector-issuer" -}}
{{- printf "sdp-injector-issuer-%s" .Release.Name }}
{{- end }}

{{/*
Sidecar Config
*/}}
{{- define "sdp-injector.sidecar-config" -}}
{{- printf "sdp-sidecar-config-%s" .Release.Name }}
{{- end }}

{{/*
Default app version
*/}}
{{- define "sdp-injector.defaultTag" -}}
  {{- default .Chart.AppVersion .Values.global.image.tag }}
{{- end -}}

{{/*
Namespace
*/}}
{{- define "sdp-injector.namespace" -}}
{{- if eq .Release.Namespace "default" }}
{{- print "sdp-system" }}
{{- else}}
{{- .Release.Namespace }}
{{- end }}
{{- end -}}
