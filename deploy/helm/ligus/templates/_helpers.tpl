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
{{- required "workspace.existingSecret is required: create the workspace connection Secret first (values.yaml shows how)" .Values.workspace.existingSecret -}}
{{- end -}}

{{/*
The in-release relay's Services, mirroring ligus-relay.fullname (the
subchart's name is its alias, "relay", unless nameOverride is set) and its
topology rule.
*/}}
{{- define "ligus.relayBase" -}}
{{- $r := .Values.relay -}}
{{- $name := "" -}}
{{- if $r.fullnameOverride -}}
{{- $name = $r.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $chart := default "relay" $r.nameOverride -}}
{{- if contains $chart .Release.Name -}}
{{- $name = .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name = printf "%s-%s" .Release.Name $chart | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
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

{{/*
Cloud metadata endpoints, excluded from every internet egress rule whatever
networkPolicy.internetExcept says (a list value replaces the default, so an
override that only meant to change the private ranges would drop them).
*/}}
{{- define "ligus.metadataExcept" -}}
{{- toJson (list "169.254.0.0/16" "100.100.100.200/32") -}}
{{- end -}}

{{/* HTTPS to the internet, never cloud metadata or private ranges. */}}
{{- define "ligus.internetHttpsEgress" -}}
{{- $except := concat (include "ligus.metadataExcept" . | fromJsonArray) (.Values.networkPolicy.internetExcept | default list) | uniq -}}
- to:
    - ipBlock:
        cidr: 0.0.0.0/0
        except:
          {{- toYaml $except | nindent 10 }}
  ports:
    - { port: 443, protocol: TCP }
{{- end -}}

{{/*
Refuse free-form egress rules (extraEgress, mcp.servers[].egress) that open
every address without excluding cloud metadata: 169.254.169.254 (and
Alibaba's 100.100.100.200) for 0.0.0.0/0, AWS's fd00:ec2::254 for ::/0.
Args: dict rules what.
*/}}
{{- define "ligus.validateEgressRules" -}}
{{- $what := .what -}}
{{- range $rule := .rules -}}
{{- range $peer := (default list $rule.to) -}}
{{- with $peer.ipBlock -}}
{{- $exc := default list .except -}}
{{- if eq (toString .cidr) "0.0.0.0/0" -}}
{{- if not (and (or (has "169.254.0.0/16" $exc) (has "169.254.169.254/32" $exc)) (or (has "100.100.100.200/32" $exc) (has "100.64.0.0/10" $exc))) -}}
{{- fail (printf "%s: an ipBlock of 0.0.0.0/0 must list 169.254.0.0/16 (or 169.254.169.254/32) and 100.100.100.200/32 in except: cloud metadata hands out node credentials" $what) -}}
{{- end -}}
{{- else if eq (toString .cidr) "::/0" -}}
{{- if not (or (has "fd00:ec2::254/128" $exc) (has "fd00:ec2::/32" $exc) (has "fd00::/8" $exc) (has "fc00::/7" $exc)) -}}
{{- fail (printf "%s: an ipBlock of ::/0 must list fd00:ec2::254/128 (or fc00::/7) in except: AWS instance metadata over IPv6" $what) -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/* Egress to the relay's pods (in this release, or relayNamespace). */}}
{{- define "ligus.relayEgress" -}}
{{- $np := .Values.networkPolicy -}}
{{- if .Values.relay.enabled }}
- to:
    - podSelector:
        matchLabels:
          app.kubernetes.io/name: ligus-relay
  ports:
    - port: 8443
{{- else if $np.relayNamespace }}
- to:
    - namespaceSelector:
        matchLabels:
          kubernetes.io/metadata.name: {{ $np.relayNamespace }}
      podSelector:
        matchLabels:
          app.kubernetes.io/name: ligus-relay
  ports:
    - port: 8443
{{- end }}
{{- end -}}

{{- define "ligus.validate" -}}
{{- $np := .Values.networkPolicy -}}
{{- range $c := list "serve" "worker" -}}
{{- if not (has (index $.Values $c).persistence.accessMode (list "ReadWriteOncePod" "ReadWriteOnce")) -}}
{{- fail (printf "%s.persistence.accessMode must be ReadWriteOncePod or ReadWriteOnce (one identity, one writer; never RWX)" $c) -}}
{{- end -}}
{{- end -}}
{{- if and $np.enabled $np.restrictEgress (not .Values.relay.enabled) (not $np.relayNamespace) (not $np.allowInternetHttps) (not $np.extraEgress) -}}
{{- fail "networkPolicy.restrictEgress is on but nothing lets serve reach the relay: set networkPolicy.relayNamespace (relay in this cluster), allowInternetHttps (relay behind https outside), or extraEgress" -}}
{{- end -}}
{{- if not (include "ligus.relayUrl" .) -}}
{{- fail "set relayUrl (or relay.enabled=true for an in-release relay)" -}}
{{- end -}}
{{- if hasKey .Values.workspace "generate" -}}
{{- fail "workspace.generate was removed (it used lookup, which regenerates the key under helm template and GitOps): create the Secret with kubectl first; values.yaml shows the command" -}}
{{- end -}}
{{- if not .Values.workspace.existingSecret -}}
{{- fail "set workspace.existingSecret (the workspace connection Secret: room, key; values.yaml shows how to make a throwaway one)" -}}
{{- end -}}
{{- if and .Values.remote.enabled .Values.relay.enabled (not .Values.remote.relayUrl) (eq (dig "tunnel" "mode" "off" .Values.relay) "off") -}}
{{- fail "remote.enabled with the in-release relay needs relay.tunnel.mode (allowlist, or open behind a token gate): the relay chart's tunnel is off by default" -}}
{{- end -}}
{{- if and .Values.remote.enabled (not (include "ligus.remoteRelayUrl" .)) -}}
{{- fail "remote.enabled needs remote.relayUrl (or relay.enabled=true)" -}}
{{- end -}}
{{- if hasKey .Values "sandbox" -}}
{{- fail "sandbox.* was removed: Kubernetes per-turn executors and PR-test sandboxes are not supported in 0.1 (agent turns run in the serve/worker pod under Ligus's own sandbox; see deploy/kubernetes.md, Explicitly unsupported)" -}}
{{- end -}}
{{- if or (hasKey .Values.serve "secretEnv") (hasKey .Values.worker "secretEnv") -}}
{{- fail "serve.secretEnv / worker.secretEnv (envFrom of a whole Secret) was removed: put workspace runtime credentials in <host>.workspaceSecrets.existingSecret (mounted as files, LIGUS_WS_SECRET_DIR) and provider keys CLI agents need in <host>.agentEnv (allow-listed)" -}}
{{- end -}}
{{- include "ligus.validateExtraEnv" (dict "v" .Values.serve "component" "serve") -}}
{{- include "ligus.validateExtraEnv" (dict "v" .Values.worker "component" "worker") -}}
{{- include "ligus.validateAgentEnv" (dict "root" . "v" .Values.serve "component" "serve") -}}
{{- include "ligus.validateAgentEnv" (dict "root" . "v" .Values.worker "component" "worker") -}}
{{- if hasKey .Values.worker "replicas" -}}
{{- fail "worker.replicas was replaced by worker.instances: one named entry per worker host (each is its own identity, never a scaled replica)" -}}
{{- end -}}
{{- if and .Values.worker.enabled (not .Values.worker.instances) -}}
{{- fail "worker.enabled needs at least one worker.instances entry ({name: ...})" -}}
{{- end -}}
{{- $seen := dict -}}
{{- range .Values.worker.instances -}}
{{- if not (regexMatch "^[a-z0-9]([-a-z0-9]{0,30}[a-z0-9])?$" (toString .name)) -}}
{{- fail (printf "worker.instances[].name %q must be a DNS label of at most 32 characters" (toString .name)) -}}
{{- end -}}
{{- if hasKey $seen .name -}}
{{- fail (printf "worker.instances: %s is listed twice" .name) -}}
{{- end -}}
{{- $_ := set $seen .name true -}}
{{- end -}}
{{- include "ligus.validateEgressRules" (dict "rules" $np.extraEgress "what" "networkPolicy.extraEgress") -}}
{{- range .Values.mcp.servers -}}
{{- include "ligus.validateEgressRules" (dict "rules" .egress "what" (printf "mcp.servers[%s].egress" .name)) -}}
{{- end -}}
{{- if hasKey .Values "otel" -}}
{{- fail "otel.* was removed: Ligus does not export OpenTelemetry in 0.1" -}}
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
# Ligus denies the files to agent processes. The workspace key and relay
# token are read through Ligus's k8s secret store (cli.key, cli.token in
# LIGUS_SECRETS_DIR), the same place `ligus enroll` would keep them, so no
# environment variable overrides them on restart.
- name: LIGUS_SECRET_BACKEND
  value: k8s
- name: LIGUS_SECRETS_DIR
  value: /etc/ligus/secrets/store
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
  mountPath: /etc/ligus/secrets/store
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
      - { key: {{ .root.Values.workspace.secretKeys.key }}, path: cli.key }
      {{- if .root.Values.workspace.relayAccessToken }}
      - { key: {{ .root.Values.workspace.secretKeys.relayAccessToken }}, path: cli.token }
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

{{/*
extraEnv may not set what the chart decides: Kubernetes keeps the last of
two entries with one name, and Ligus prefers LIGUS_WORKSPACE_KEY from the
environment over the mounted cli.key, so extraEnv would silently undo the
secret files and the fail-closed sandbox settings. Nor may it read a
Secret: secrets are files. Args: dict v component.
*/}}
{{- define "ligus.validateExtraEnv" -}}
{{- $chart := list "LIGUS_DATA_DIR" "HOME" "LIGUS_RELAY_URL" "LIGUS_WORKSPACE" "LIGUS_REQUIRE_SANDBOX" "LIGUS_SANDBOX" "LIGUS_SECRET_BACKEND" "LIGUS_SECRETS_DIR" "LIGUS_WS_SECRET_DIR" -}}
{{- range .v.extraEnv -}}
{{- $n := toString .name -}}
{{- $base := regexReplaceAll "^HIVE_" $n "LIGUS_" -}}
{{- if or (has $base $chart) (regexMatch "^LIGUS_(WORKSPACE_KEY|RELAY_ACCESS_TOKEN|REMOTE_REG_TOKEN|NTFY_TOKEN)(_FILE)?$" $base) (hasPrefix "LIGUS_WS_SECRET_" $base) -}}
{{- fail (printf "%s.extraEnv: %s is set by the chart (secrets are mounted files, the sandbox settings come from agentSandbox.*); remove it" $.component $n) -}}
{{- end -}}
{{- if and .valueFrom (or .valueFrom.secretKeyRef) -}}
{{- fail (printf "%s.extraEnv: %s reads a Secret; secrets are files (workspaceSecrets.existingSecret) or allow-listed agentEnv entries, never extraEnv" $.component $n) -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/* Agent env names must be on the allow-list (no Ligus secrets in env). */}}
{{- define "ligus.validateAgentEnv" -}}
{{- $allow := .root.Values.agentEnvAllowList -}}
{{- range $allow -}}
{{- if regexMatch "^(LIGUS|HIVE)_" (toString .) -}}
{{- fail (printf "agentEnvAllowList: %s is one of Ligus's own variables; only model provider and forge keys belong there" (toString .)) -}}
{{- end -}}
{{- end -}}
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


{{/* serve's volume mounts (also the preflight's). */}}
{{- define "ligus.serveMounts" -}}
- name: data
  mountPath: /var/lib/ligus
# Its own volume: CLI logins persist, outside the data dir agents can't read.
- name: home
  mountPath: /home/ligus
- name: tmp
  mountPath: /tmp
{{- if .Values.serve.runtimesToml }}
- name: runtimes
  mountPath: /etc/ligus/runtimes
  readOnly: true
{{- end }}
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
- name: home
  mountPath: /home/ligus
- name: tmp
  mountPath: /tmp
{{- if .Values.worker.runtimesToml }}
- name: runtimes
  mountPath: /etc/ligus/runtimes
  readOnly: true
{{- end }}
{{ include "ligus.secretMounts" (dict "root" . "v" .Values.worker "component" "worker") }}
{{- end -}}

{{/* volumeClaimTemplates for a host: data (the identity) and home. Args: dict root v component. */}}
{{- define "ligus.claimTemplates" -}}
- metadata:
    name: data
    labels:
      {{- include "ligus.selectorLabels" (dict "root" .root "component" .component) | nindent 6 }}
  spec:
    accessModes: [{{ .v.persistence.accessMode | quote }}]
    {{- with .v.persistence.storageClass }}
    storageClassName: {{ . }}
    {{- end }}
    resources:
      requests:
        storage: {{ .v.persistence.size }}
- metadata:
    name: home
    labels:
      {{- include "ligus.selectorLabels" (dict "root" .root "component" .component) | nindent 6 }}
  spec:
    accessModes: [{{ .v.persistence.accessMode | quote }}]
    {{- with .v.home.storageClass }}
    storageClassName: {{ . }}
    {{- end }}
    resources:
      requests:
        storage: {{ .v.home.size }}
{{- end -}}
