{{- define "ligus.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "ligus.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "ligus.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
app.kubernetes.io/name: {{ include "ligus.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: ligus
{{- end -}}

{{- define "ligus.selectorLabels" -}}
app.kubernetes.io/name: {{ include "ligus.name" .root }}
app.kubernetes.io/instance: {{ .root.Release.Name }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{- define "ligus.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "ligus.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{- define "ligus.image" -}}
{{- if .Values.image.digest -}}
{{- printf "%s@%s" .Values.image.repository .Values.image.digest -}}
{{- else -}}
{{- printf "%s:%s" .Values.image.repository (default .Chart.AppVersion .Values.image.tag) -}}
{{- end -}}
{{- end -}}

{{- define "ligus.workspaceSecretName" -}}
{{- if .Values.workspace.existingSecret -}}
{{- .Values.workspace.existingSecret -}}
{{- else -}}
{{- printf "%s-workspace" (include "ligus.fullname" .) -}}
{{- end -}}
{{- end -}}

{{/* The in-release relay's Services, mirroring ligus-relay's topology rule. */}}
{{- define "ligus.relayBase" -}}
{{- $r := .Values.relay -}}
{{- $name := default (printf "%s-ligus-relay" .Release.Name) $r.fullnameOverride -}}
{{- $hasDb := or (dig "postgres" "enabled" false $r) (dig "externalDatabase" "url" "" $r) (dig "externalDatabase" "existingSecret" "" $r) -}}
{{- $topology := default "auto" $r.topology -}}
{{- if eq $topology "auto" }}{{ $topology = ternary "split" "single" (not (empty $hasDb)) }}{{ end -}}
{{- $port := dig "service" "port" 8443 $r -}}
{{- if eq $topology "split" -}}
{{- printf "%s-sync|%s-tunnel|%v" $name $name $port -}}
{{- else -}}
{{- printf "%s|%s|%v" $name $name $port -}}
{{- end -}}
{{- end -}}

{{- define "ligus.relayUrl" -}}
{{- if .Values.relayUrl -}}
{{- .Values.relayUrl -}}
{{- else if .Values.relay.enabled -}}
{{- $p := split "|" (include "ligus.relayBase" .) -}}
{{- printf "http://%s.%s.svc.cluster.local:%s" $p._0 .Release.Namespace $p._2 -}}
{{- end -}}
{{- end -}}

{{- define "ligus.remoteRelayUrl" -}}
{{- if .Values.remote.relayUrl -}}
{{- .Values.remote.relayUrl -}}
{{- else if .Values.relay.enabled -}}
{{- $p := split "|" (include "ligus.relayBase" .) -}}
{{- printf "http://%s.%s.svc.cluster.local:%s" $p._1 .Release.Namespace $p._2 -}}
{{- end -}}
{{- end -}}

{{/* Egress rule for the cluster DNS resolver only. */}}
{{- define "ligus.dnsEgress" -}}
- to:
    - namespaceSelector:
        {{- toYaml .Values.networkPolicy.dns.namespaceSelector | nindent 8 }}
      podSelector:
        {{- toYaml .Values.networkPolicy.dns.podSelector | nindent 8 }}
  ports:
    - { port: 53, protocol: UDP }
    - { port: 53, protocol: TCP }
{{- end -}}

{{- define "ligus.validate" -}}
{{- if not (include "ligus.relayUrl" .) -}}
{{- fail "set relayUrl (or relay.enabled=true for an in-release relay)" -}}
{{- end -}}
{{- if and (not .Values.workspace.existingSecret) (not .Values.workspace.generate) -}}
{{- fail "set workspace.existingSecret (the workspace connection), or workspace.generate=true for a throwaway test workspace" -}}
{{- end -}}
{{- if and .Values.remote.enabled (not (include "ligus.remoteRelayUrl" .)) -}}
{{- fail "remote.enabled needs remote.relayUrl (or relay.enabled=true)" -}}
{{- end -}}
{{- if hasKey .Values "sandbox" -}}
{{- fail "sandbox.* was removed: Kubernetes per-turn executors and PR-test sandboxes are not supported in 2.0 (agent turns run in the serve/worker pod under Ligus's own sandbox; see deploy/kubernetes.md, Explicitly unsupported)" -}}
{{- end -}}
{{- if or (hasKey .Values.serve "secretEnv") (hasKey .Values.worker "secretEnv") -}}
{{- fail "serve.secretEnv / worker.secretEnv (envFrom of a whole Secret) was removed: put workspace runtime credentials in <host>.workspaceSecrets.existingSecret (mounted as files, LIGUS_WS_SECRET_DIR) and provider keys CLI agents need in <host>.agentEnv (allow-listed)" -}}
{{- end -}}
{{- include "ligus.validateAgentEnv" (dict "root" . "v" .Values.serve "component" "serve") -}}
{{- include "ligus.validateAgentEnv" (dict "root" . "v" .Values.worker "component" "worker") -}}
{{- if hasKey .Values "otel" -}}
{{- fail "otel.* was removed: Ligus does not export OpenTelemetry in 2.0" -}}
{{- end -}}
{{- end -}}

{{/* Environment shared by serve and worker. */}}
{{- define "ligus.commonEnv" -}}
- name: LIGUS_DATA_DIR
  value: /var/lib/ligus
# HOME (agent CLIs' logins and caches) is outside the data dir: agent
# processes never read the data dir (keys, hive.db, serve.token).
- name: HOME
  value: /home/ligus
- name: POD_NAME
  valueFrom:
    fieldRef:
      fieldPath: metadata.name
- name: LIGUS_RELAY_URL
  value: {{ include "ligus.relayUrl" . | quote }}
{{- if .Values.agentSandbox.require }}
- name: LIGUS_REQUIRE_SANDBOX
  value: "1"
{{- end }}
{{- with .Values.agentSandbox.tier }}
- name: LIGUS_SANDBOX
  value: {{ . | quote }}
{{- end }}
- name: LIGUS_WORKSPACE
  valueFrom:
    secretKeyRef:
      name: {{ include "ligus.workspaceSecretName" . }}
      key: {{ .Values.workspace.secretKeys.room }}
# Secrets are files (mounted 0400), never environment variables: they stay
# out of the pod spec, `kubectl describe` and /proc/<pid>/environ, and
# Ligus denies the files to agent processes.
- name: LIGUS_WORKSPACE_KEY_FILE
  value: /etc/ligus/secrets/workspace/workspace-key
{{- if .Values.workspace.relayAccessToken }}
- name: LIGUS_RELAY_ACCESS_TOKEN_FILE
  value: /etc/ligus/secrets/workspace/relay-access-token
{{- end }}
{{- end -}}

{{/*
Per-host secrets (serve or worker). Args: dict root v (the serve or worker
values) component.
*/}}
{{- define "ligus.hostSecretEnv" -}}
{{- $root := .root -}}
{{- if .v.workspaceSecrets.existingSecret }}
# Workspace runtime credentials: each key of the Secret is a secret_ref
# (LIGUS_WS_SECRET_<key>).
- name: LIGUS_WS_SECRET_DIR
  value: /etc/ligus/secrets/ws
{{- end }}
{{- if eq .component "serve" }}
{{- if and .v.ntfy.url .v.ntfy.topic .v.ntfy.tokenSecret.name }}
- name: LIGUS_NTFY_TOKEN_FILE
  value: /etc/ligus/secrets/ntfy/token
{{- end }}
{{- if and $root.Values.remote.enabled $root.Values.remote.regTokenSecret.name }}
- name: LIGUS_REMOTE_REG_TOKEN_FILE
  value: /etc/ligus/secrets/remote/reg-token
{{- end }}
{{- end }}
{{- range .v.agentEnv }}
# Passed to agent processes (CLI agents need their provider key in env).
- name: {{ .name }}
  valueFrom:
    secretKeyRef:
      name: {{ required "agentEnv[].secretName is required" .secretName }}
      key: {{ required "agentEnv[].secretKey is required" .secretKey }}
{{- end }}
{{- end -}}

{{/* Secret volume mounts for a host. Args: dict root v component. */}}
{{- define "ligus.secretMounts" -}}
- name: workspace-secret
  mountPath: /etc/ligus/secrets/workspace
  readOnly: true
{{- if .v.workspaceSecrets.existingSecret }}
- name: ws-secrets
  mountPath: /etc/ligus/secrets/ws
  readOnly: true
{{- end }}
{{- if eq .component "serve" }}
{{- if and .v.ntfy.url .v.ntfy.topic .v.ntfy.tokenSecret.name }}
- name: ntfy-token
  mountPath: /etc/ligus/secrets/ntfy
  readOnly: true
{{- end }}
{{- if and .root.Values.remote.enabled .root.Values.remote.regTokenSecret.name }}
- name: remote-token
  mountPath: /etc/ligus/secrets/remote
  readOnly: true
{{- end }}
{{- end }}
{{- end -}}

{{/*
Secret volumes for a host. 0400: with fsGroup the kubelet adds group read,
so the files end up 0440 root:65532, readable by the Ligus user only.
Args: dict root v component.
*/}}
{{- define "ligus.secretVolumes" -}}
- name: workspace-secret
  secret:
    secretName: {{ include "ligus.workspaceSecretName" .root }}
    defaultMode: 0400
    items:
      - { key: {{ .root.Values.workspace.secretKeys.key }}, path: workspace-key }
      {{- if .root.Values.workspace.relayAccessToken }}
      - { key: {{ .root.Values.workspace.secretKeys.relayAccessToken }}, path: relay-access-token }
      {{- end }}
{{- if .v.workspaceSecrets.existingSecret }}
- name: ws-secrets
  secret:
    secretName: {{ .v.workspaceSecrets.existingSecret }}
    defaultMode: 0400
{{- end }}
{{- if eq .component "serve" }}
{{- if and .v.ntfy.url .v.ntfy.topic .v.ntfy.tokenSecret.name }}
- name: ntfy-token
  secret:
    secretName: {{ .v.ntfy.tokenSecret.name }}
    defaultMode: 0400
    items:
      - { key: {{ .v.ntfy.tokenSecret.key }}, path: token }
{{- end }}
{{- if and .root.Values.remote.enabled .root.Values.remote.regTokenSecret.name }}
- name: remote-token
  secret:
    secretName: {{ .root.Values.remote.regTokenSecret.name }}
    defaultMode: 0400
    items:
      - { key: {{ .root.Values.remote.regTokenSecret.key }}, path: reg-token }
{{- end }}
{{- end }}
{{- end -}}

{{/*
checksum/secrets: rolls the pods when a referenced Secret changes. Uses
lookup, so under `helm template` (Argo CD and other GitOps tools) it is a
constant; there, use Reloader or bump podAnnotations. Args: dict root v
component.
*/}}
{{- define "ligus.secretsChecksum" -}}
{{- $root := .root -}}
{{- $names := list (include "ligus.workspaceSecretName" $root) -}}
{{- with .v.workspaceSecrets.existingSecret }}{{ $names = append $names . }}{{ end -}}
{{- range .v.agentEnv }}{{ $names = append $names .secretName }}{{ end -}}
{{- if eq .component "serve" -}}
{{- with .v.ntfy.tokenSecret.name }}{{ $names = append $names . }}{{ end -}}
{{- with $root.Values.remote.regTokenSecret.name }}{{ $names = append $names . }}{{ end -}}
{{- end -}}
{{- $data := dict -}}
{{- range $n := ($names | uniq | sortAlpha) -}}
{{- $s := lookup "v1" "Secret" $root.Release.Namespace $n -}}
{{- $_ := set $data $n (dig "data" (dict) $s) -}}
{{- end -}}
{{- toJson $data | sha256sum -}}
{{- end -}}

{{/* Agent env names must be on the allow-list (no Ligus secrets in env). */}}
{{- define "ligus.validateAgentEnv" -}}
{{- $allow := .root.Values.agentEnvAllowList -}}
{{- range .v.agentEnv -}}
{{- if not (has .name $allow) -}}
{{- fail (printf "%s.agentEnv: %s is not in agentEnvAllowList; model provider keys only (Ligus's own secrets go in files)" $.component .name) -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "ligus.podSpecCommon" -}}
serviceAccountName: {{ include "ligus.serviceAccountName" . }}
automountServiceAccountToken: false
enableServiceLinks: false
hostNetwork: false
hostPID: false
hostIPC: false
{{- with .Values.imagePullSecrets }}
imagePullSecrets:
  {{- toYaml . | nindent 2 }}
{{- end }}
securityContext:
  {{- toYaml .Values.podSecurityContext | nindent 2 }}
{{- with .Values.nodeSelector }}
nodeSelector:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with .Values.tolerations }}
tolerations:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with .Values.affinity }}
affinity:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- end -}}

{{/*
Sandbox preflight init container: the pod is never Ready on a node where
agent turns can't be confined. Args: dict root env mounts (YAML strings of
the main container's env and volumeMounts, so the check sees what serve or
the worker will).
*/}}
{{- define "ligus.preflightInit" -}}
{{- $p := .root.Values.agentSandbox.preflight -}}
- name: sandbox-preflight
  image: {{ include "ligus.image" .root }}
  imagePullPolicy: {{ .root.Values.image.pullPolicy }}
  args:
    - doctor
    - --require-confined
    {{- with .root.Values.agentSandbox.tier }}
    - --expect-tier
    - {{ . | quote }}
    {{- end }}
    {{- if $p.turnTest }}
    - --turn-test
    {{- end }}
  securityContext:
    {{- toYaml .root.Values.securityContext | nindent 4 }}
  resources:
    requests: { cpu: 10m, memory: 32Mi }
    limits: { cpu: 500m, memory: 256Mi }
  env:
    {{- .env | nindent 4 }}
  volumeMounts:
    {{- .mounts | nindent 4 }}
{{- end -}}

{{/* Init container that copies runtimes.toml from a ConfigMap into the data dir. */}}
{{- define "ligus.runtimesInit" -}}
- name: runtimes
  image: {{ include "ligus.image" .root }}
  imagePullPolicy: {{ .root.Values.image.pullPolicy }}
  command: ["/bin/sh", "-c", "install -m 0600 /etc/ligus/runtimes/runtimes.toml /var/lib/ligus/runtimes.toml"]
  securityContext:
    {{- toYaml .root.Values.securityContext | nindent 4 }}
  resources:
    requests: { cpu: 10m, memory: 16Mi }
    limits: { cpu: 100m, memory: 64Mi }
  volumeMounts:
    - name: data
      mountPath: /var/lib/ligus
    - name: runtimes
      mountPath: /etc/ligus/runtimes
      readOnly: true
{{- end -}}

{{/* serve's volume mounts (also the preflight's). */}}
{{- define "ligus.serveMounts" -}}
- name: data
  mountPath: /var/lib/ligus
# Same volume, its own subdirectory: CLI logins persist, outside the data
# dir agents can't read.
- name: data
  mountPath: /home/ligus
  subPath: home
- name: tmp
  mountPath: /tmp
{{ include "ligus.secretMounts" (dict "root" . "v" .Values.serve "component" "serve") }}
{{- if or .Values.serve.mcpToml .Values.serve.dashboardToml }}
- name: config
  mountPath: /etc/ligus/config
  readOnly: true
{{- end }}
{{- end -}}

{{/* A worker's volume mounts (also the preflight's). */}}
{{- define "ligus.workerMounts" -}}
- name: data
  mountPath: /var/lib/ligus
- name: data
  mountPath: /home/ligus
  subPath: home
- name: tmp
  mountPath: /tmp
{{ include "ligus.secretMounts" (dict "root" . "v" .Values.worker "component" "worker") }}
{{- end -}}
