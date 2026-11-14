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

{{- define "ligus.sandboxImage" -}}
{{- $s := .Values.sandbox.image -}}
{{- $repo := default .Values.image.repository $s.repository -}}
{{- if $s.digest -}}
{{- printf "%s@%s" $repo $s.digest -}}
{{- else if $s.tag -}}
{{- printf "%s:%s" $repo $s.tag -}}
{{- else -}}
{{- include "ligus.image" . -}}
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
{{- if and .Values.sandbox.enabled (eq .Values.sandbox.profile "pr-test") (not .Values.sandbox.runtimeClassName) (not .Values.sandbox.allowDefaultRuntime) -}}
{{- fail "sandbox.profile=pr-test runs untrusted code: set sandbox.runtimeClassName (gvisor, kata, ...) or, for kind/CI only, sandbox.allowDefaultRuntime=true" -}}
{{- end -}}
{{- if not (has .Values.sandbox.profile (list "agent-turn" "pr-test")) -}}
{{- fail "sandbox.profile must be agent-turn or pr-test" -}}
{{- end -}}
{{- end -}}

{{/* Environment shared by serve and worker. */}}
{{- define "ligus.commonEnv" -}}
- name: LIGUS_DATA_DIR
  value: /var/lib/ligus
- name: HOME
  value: /var/lib/ligus
- name: POD_NAME
  valueFrom:
    fieldRef:
      fieldPath: metadata.name
- name: LIGUS_RELAY_URL
  value: {{ include "ligus.relayUrl" . | quote }}
- name: LIGUS_WORKSPACE
  valueFrom:
    secretKeyRef:
      name: {{ include "ligus.workspaceSecretName" . }}
      key: {{ .Values.workspace.secretKeys.room }}
- name: LIGUS_WORKSPACE_KEY
  valueFrom:
    secretKeyRef:
      name: {{ include "ligus.workspaceSecretName" . }}
      key: {{ .Values.workspace.secretKeys.key }}
- name: LIGUS_RELAY_ACCESS_TOKEN
  valueFrom:
    secretKeyRef:
      name: {{ include "ligus.workspaceSecretName" . }}
      key: {{ .Values.workspace.secretKeys.relayAccessToken }}
      optional: true
{{- with .Values.otel.endpoint }}
- name: OTEL_EXPORTER_OTLP_ENDPOINT
  value: {{ . | quote }}
{{- end }}
{{- with .Values.otel.resourceAttributes }}
- name: OTEL_RESOURCE_ATTRIBUTES
  value: {{ . | quote }}
{{- end }}
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

{{/*
Hardened executor Job. Args: dict root name command [entrypoint]
entrypoint=true replaces the image entrypoint with command[0].
*/}}
{{- define "ligus.sandboxJob" -}}
{{- $root := .root -}}
{{- $s := $root.Values.sandbox -}}
apiVersion: batch/v1
kind: Job
metadata:
  name: {{ .name }}
  labels:
    {{- include "ligus.labels" $root | nindent 4 }}
    app.kubernetes.io/component: sandbox
    ligus.ai/role: executor
    ligus.ai/sandbox-profile: {{ $s.profile }}
spec:
  backoffLimit: 0
  activeDeadlineSeconds: {{ $s.activeDeadlineSeconds }}
  ttlSecondsAfterFinished: {{ $s.ttlSecondsAfterFinished }}
  template:
    metadata:
      labels:
        app.kubernetes.io/instance: {{ $root.Release.Name }}
        app.kubernetes.io/component: sandbox
        ligus.ai/role: executor
        ligus.ai/sandbox-profile: {{ $s.profile }}
    spec:
      restartPolicy: Never
      automountServiceAccountToken: false
      enableServiceLinks: false
      hostNetwork: false
      hostPID: false
      hostIPC: false
      {{- with $s.runtimeClassName }}
      runtimeClassName: {{ . }}
      {{- end }}
      securityContext:
        runAsNonRoot: true
        runAsUser: 65532
        runAsGroup: 65532
        fsGroup: 65532
        seccompProfile: { type: RuntimeDefault }
      containers:
        - name: turn
          image: {{ include "ligus.sandboxImage" $root }}
          imagePullPolicy: {{ $root.Values.image.pullPolicy }}
          {{- if .entrypoint }}
          command: [{{ first .command | quote }}]
          args:
            {{- range rest .command }}
            - {{ . | quote }}
            {{- end }}
          {{- else }}
          args:
            {{- range .command }}
            - {{ . | quote }}
            {{- end }}
          {{- end }}
          env:
            - { name: HOME, value: /work }
            - { name: TMPDIR, value: /tmp }
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities: { drop: ["ALL"] }
          resources:
            {{- toYaml $s.resources | nindent 12 }}
          volumeMounts:
            - { name: work, mountPath: /work }
            - { name: tmp, mountPath: /tmp }
      volumes:
        - name: work
          emptyDir: { sizeLimit: {{ $s.workSizeLimit }} }
        - name: tmp
          emptyDir: { sizeLimit: 1Gi }
{{- end -}}
