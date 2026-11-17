{{/*
Model gateways (LLM gateways) for serve and workers. See
docs/design/v2/model-gateways.md and the README's "Model gateways" section.

Every gateway's credential material is a file under
/etc/ligus/model-gateways/<name>/ (one projected volume per gateway); the
binary reads the allow-list from LIGUS_MODEL_GATEWAYS_FILE and keeps those
files away from agent processes.
*/}}

{{- define "ligus.mg.dir" -}}
/etc/ligus/model-gateways/{{ . }}
{{- end -}}

{{/* 2^n for 0 <= n <= 32. */}}
{{- define "ligus.mg.pow2" -}}
{{- $r := 1 -}}
{{- range until (int .) -}}{{- $r = mul $r 2 -}}{{- end -}}
{{- $r -}}
{{- end -}}

{{/*
An address as {"w": unit width in bits, "u": [unit values]}: four octets for
IPv4, eight hextets for IPv6 ("::" expanded). Fails on anything else.
*/}}
{{- define "ligus.mg.addr" -}}
{{- $a := lower (toString .) -}}
{{- if contains ":" $a -}}
{{- $hex := dict "0" 0 "1" 1 "2" 2 "3" 3 "4" 4 "5" 5 "6" 6 "7" 7 "8" 8 "9" 9 "a" 10 "b" 11 "c" 12 "d" 13 "e" 14 "f" 15 -}}
{{- $parts := splitList "::" $a -}}
{{- if gt (len $parts) 2 -}}{{- fail (printf "modelGateways: %q is not an IPv6 address" $a) -}}{{- end -}}
{{- $left := list -}}
{{- $right := list -}}
{{- if ne (index $parts 0) "" -}}{{- $left = splitList ":" (index $parts 0) -}}{{- end -}}
{{- if and (eq (len $parts) 2) (ne (index $parts 1) "") -}}{{- $right = splitList ":" (index $parts 1) -}}{{- end -}}
{{- $groups := $left -}}
{{- if eq (len $parts) 2 -}}
{{- range until (int (sub 8 (add (len $left) (len $right)))) -}}{{- $groups = append $groups "0" -}}{{- end -}}
{{- $groups = concat $groups $right -}}
{{- end -}}
{{- if ne (len $groups) 8 -}}{{- fail (printf "modelGateways: %q is not an IPv6 address (embedded IPv4 is not supported)" $a) -}}{{- end -}}
{{- $units := list -}}
{{- range $g := $groups -}}
{{- if or (eq $g "") (gt (len $g) 4) -}}{{- fail (printf "modelGateways: %q is not an IPv6 address" $a) -}}{{- end -}}
{{- $v := 0 -}}
{{- range $c := splitList "" $g -}}
{{- if not (hasKey $hex $c) -}}{{- fail (printf "modelGateways: %q is not an IPv6 address" $a) -}}{{- end -}}
{{- $v = add (mul $v 16) (get $hex $c) -}}
{{- end -}}
{{- $units = append $units $v -}}
{{- end -}}
{{- toJson (dict "w" 16 "u" $units) -}}
{{- else -}}
{{- $oct := splitList "." $a -}}
{{- if ne (len $oct) 4 -}}{{- fail (printf "modelGateways: %q is not an IPv4 address" $a) -}}{{- end -}}
{{- $units := list -}}
{{- range $o := $oct -}}
{{- if not (regexMatch "^[0-9]{1,3}$" $o) -}}{{- fail (printf "modelGateways: %q is not an IPv4 address" $a) -}}{{- end -}}
{{- if gt (atoi $o) 255 -}}{{- fail (printf "modelGateways: %q is not an IPv4 address" $a) -}}{{- end -}}
{{- $units = append $units (atoi $o) -}}
{{- end -}}
{{- toJson (dict "w" 8 "u" $units) -}}
{{- end -}}
{{- end -}}

{{/*
Whether two CIDRs of the same family overlap ("true" or ""): they do iff
their first min(prefix) bits are equal. Args: dict a b (strings "addr/len").
*/}}
{{- define "ligus.mg.overlap" -}}
{{- $pa := splitList "/" .a -}}
{{- $pb := splitList "/" .b -}}
{{- $A := include "ligus.mg.addr" (index $pa 0) | fromJson -}}
{{- $B := include "ligus.mg.addr" (index $pb 0) | fromJson -}}
{{- if eq (int $A.w) (int $B.w) -}}
{{- $p := min (atoi (index $pa 1)) (atoi (index $pb 1)) -}}
{{- $w := int $A.w -}}
{{- $same := true -}}
{{- range $i := until (len $A.u) -}}
{{- $r := sub $p (mul $i $w) -}}
{{- if gt $r 0 -}}
{{- $x := int (index $A.u $i) -}}
{{- $y := int (index $B.u $i) -}}
{{- if lt $r $w -}}
{{- $d := include "ligus.mg.pow2" (sub $w $r) | atoi -}}
{{- $x = div $x $d -}}
{{- $y = div $y $d -}}
{{- end -}}
{{- if ne $x $y -}}{{- $same = false -}}{{- end -}}
{{- end -}}
{{- end -}}
{{- if $same -}}true{{- end -}}
{{- end -}}
{{- end -}}

{{/* Refuse anything unsafe or ambiguous in .Values.modelGateways. */}}
{{- define "ligus.modelGatewaysValidate" -}}
{{- $seen := dict -}}
{{- $seenAud := dict -}}
{{- $refused := list "169.254.0.0/16" "100.100.100.200/32" "fd00:ec2::254/128" "fe80::/10" -}}
{{- range $i, $g := .Values.modelGateways -}}
{{- $n := toString $g.name -}}
{{- $where := printf "modelGateways[%d] (%s)" $i $n -}}
{{- if not (regexMatch "^[a-z0-9]([a-z0-9-]{0,58}[a-z0-9])?$" $n) -}}
{{- fail (printf "%s: name must be 1-60 characters of a-z, 0-9 and '-', not starting or ending with '-' (it names a volume)" $where) -}}
{{- end -}}
{{- if hasKey $seen $n -}}{{- fail (printf "%s: duplicate name" $where) -}}{{- end -}}
{{- $_ := set $seen $n true -}}
{{- $api := default "openai" $g.api -}}
{{- if not (has $api (list "openai" "anthropic")) -}}{{- fail (printf "%s: api must be openai or anthropic" $where) -}}{{- end -}}
{{- if not $g.url -}}{{- fail (printf "%s: url is required" $where) -}}{{- end -}}
{{- $u := urlParse (toString $g.url) -}}
{{- if not (has $u.scheme (list "https" "http")) -}}{{- fail (printf "%s: url must be https:// (or http:// with allowPlaintext)" $where) -}}{{- end -}}
{{- if and (eq $u.scheme "http") (not $g.allowPlaintext) -}}
{{- fail (printf "%s: url is http://; a bearer token or key would cross the network in clear. Use https://, or set allowPlaintext only inside a mesh with mTLS" $where) -}}
{{- end -}}
{{- if or $u.userinfo (contains "@" (toString $u.host)) -}}{{- fail (printf "%s: url must not carry credentials (user:pass@); use auth" $where) -}}{{- end -}}
{{- if not $u.host -}}{{- fail (printf "%s: url has no host" $where) -}}{{- end -}}
{{- if or $u.query $u.fragment -}}{{- fail (printf "%s: url must not have a query or fragment" $where) -}}{{- end -}}
{{- $auth := default dict $g.auth -}}
{{- $mode := toString (default "" $auth.mode) -}}
{{- if not (has $mode (list "serviceAccountToken" "apiKey" "mtls" "none")) -}}
{{- fail (printf "%s: auth.mode must be serviceAccountToken, apiKey, mtls or none" $where) -}}
{{- end -}}
{{- if eq $mode "serviceAccountToken" -}}
{{- $exp := int (default 3600 $auth.expirationSeconds) -}}
{{- if lt $exp 600 -}}{{- fail (printf "%s: auth.expirationSeconds must be at least 600 (the kubelet's minimum)" $where) -}}{{- end -}}
{{- if gt $exp 86400 -}}{{- fail (printf "%s: auth.expirationSeconds must be at most 86400 (a day): the gateway holds a replayable token that long" $where) -}}{{- end -}}
{{- /* One audience per gateway, never the API server's: a gateway that
       receives the token can replay it wherever its audience is accepted. */ -}}
{{- $aud := toString (default $n $auth.audience) -}}
{{- if or (contains "kubernetes.default" (lower $aud)) (eq (lower $aud) "api") -}}
{{- fail (printf "%s: auth.audience %q is the Kubernetes API server's; the gateway could use the token against the cluster. Use the gateway's own audience" $where $aud) -}}
{{- end -}}
{{- if hasKey $seenAud $aud -}}{{- fail (printf "%s: auth.audience %q is also another gateway's; each gateway needs its own, so one can't replay the token it was sent to the other" $where $aud) -}}{{- end -}}
{{- $_ := set $seenAud $aud true -}}
{{- end -}}
{{- if eq $mode "apiKey" -}}
{{- if or (not $auth.secretName) (not $auth.secretKey) -}}{{- fail (printf "%s: auth.mode apiKey needs auth.secretName and auth.secretKey" $where) -}}{{- end -}}
{{- if not (has (lower (default "authorization" $auth.header)) (list "authorization" "x-api-key" "api-key")) -}}
{{- fail (printf "%s: auth.header must be authorization, x-api-key or api-key" $where) -}}
{{- end -}}
{{- end -}}
{{- if eq $mode "mtls" -}}
{{- if not $auth.secretName -}}{{- fail (printf "%s: auth.mode mtls needs auth.secretName (a Secret with tls.crt, tls.key and ca.crt)" $where) -}}{{- end -}}
{{- if ne $u.scheme "https" -}}{{- fail (printf "%s: auth.mode mtls needs an https:// url" $where) -}}{{- end -}}
{{- end -}}
{{- with $g.caSecret -}}
{{- if and .name (eq $u.scheme "http") -}}{{- fail (printf "%s: caSecret pins a CA for https://; the url is http://" $where) -}}{{- end -}}
{{- end -}}
{{- $np := default dict $g.networkPolicy -}}
{{- $cidrs := list -}}
{{- with $np.ipBlock -}}{{- $cidrs = default list .cidrs -}}{{- end -}}
{{- if not (or $np.namespaceSelector $np.podSelector $cidrs) -}}
{{- fail (printf "%s: networkPolicy needs namespaceSelector/podSelector or ipBlock.cidrs (a rule with no peers would open every address)" $where) -}}
{{- end -}}
{{- if not $np.ports -}}{{- fail (printf "%s: networkPolicy.ports is required" $where) -}}{{- end -}}
{{- range $p := $np.ports -}}
{{- if not (regexMatch "^[0-9]+$" (toString $p)) -}}{{- fail (printf "%s: networkPolicy.ports must be port numbers" $where) -}}{{- end -}}
{{- if or (lt (int $p) 1) (gt (int $p) 65535) -}}{{- fail (printf "%s: port %v out of range" $where $p) -}}{{- end -}}
{{- end -}}
{{- range $c := $cidrs -}}
{{- $c = toString $c -}}
{{- if not (regexMatch "^[0-9a-fA-F:.]+/[0-9]{1,3}$" $c) -}}{{- fail (printf "%s: %q is not a CIDR" $where $c) -}}{{- end -}}
{{- $pl := atoi (index (splitList "/" $c) 1) -}}
{{- $v6 := contains ":" $c -}}
{{- if or (and $v6 (or (lt $pl 16) (gt $pl 128))) (and (not $v6) (or (lt $pl 8) (gt $pl 32))) -}}
{{- fail (printf "%s: %s is too broad (IPv4 needs /8 or longer, IPv6 /16 or longer): list the gateway's own addresses" $where $c) -}}
{{- end -}}
{{- range $r := $refused -}}
{{- if eq (contains ":" $r) $v6 -}}
{{- if include "ligus.mg.overlap" (dict "a" $c "b" $r) -}}
{{- fail (printf "%s: %s overlaps %s (link-local or cloud metadata), which stays unreachable" $where $c $r) -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- range $k := list "serve" "worker" -}}
{{- range (index $.Values $k).extraEnv -}}
{{- if regexMatch "^(LIGUS|HIVE)_MODEL_GATEWAY" (toString .name) -}}
{{- fail (printf "%s.extraEnv: %s is set by the chart from modelGateways; remove it" $k .name) -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/* The binary's allow-list (LIGUS_MODEL_GATEWAYS_FILE). */}}
{{- define "ligus.modelGatewaysJson" -}}
{{- $out := list -}}
{{- range $g := .Values.modelGateways -}}
{{- $dir := include "ligus.mg.dir" $g.name -}}
{{- $a := default dict $g.auth -}}
{{- $auth := dict "mode" $a.mode -}}
{{- if eq $a.mode "serviceAccountToken" -}}{{- $_ := set $auth "tokenFile" (printf "%s/token" $dir) -}}{{- end -}}
{{- if eq $a.mode "apiKey" -}}
{{- $_ := set $auth "keyFile" (printf "%s/api-key" $dir) -}}
{{- $_ := set $auth "header" (lower (default "authorization" $a.header)) -}}
{{- end -}}
{{- if eq $a.mode "mtls" -}}
{{- $_ := set $auth "certFile" (printf "%s/tls.crt" $dir) -}}
{{- $_ := set $auth "clientKeyFile" (printf "%s/tls.key" $dir) -}}
{{- end -}}
{{- $e := dict "name" $g.name "url" $g.url "api" (default "openai" $g.api) "allowPlaintext" (default false $g.allowPlaintext) "attribution" (default false $g.attribution) "auth" $auth -}}
{{- if or (eq $a.mode "mtls") (and $g.caSecret $g.caSecret.name) -}}{{- $_ := set $e "caFile" (printf "%s/ca.crt" $dir) -}}{{- end -}}
{{- $out = append $out $e -}}
{{- end -}}
{{- toPrettyJson (dict "gateways" $out) -}}
{{- end -}}

{{/* Env for serve and workers. */}}
{{- define "ligus.modelGatewayEnv" -}}
{{- if .Values.modelGateways }}
- name: LIGUS_MODEL_GATEWAYS_FILE
  value: /etc/ligus/model-gateways-config/model-gateways.json
{{- if not .Values.modelGatewaysExclusive }}
- name: LIGUS_MODEL_GATEWAYS_EXCLUSIVE
  value: "0"
{{- end }}
{{- end }}
{{- if .Values.modelGatewayAttribution.secretName }}
- name: LIGUS_MODEL_GATEWAY_ATTRIBUTION_KEY_FILE
  value: /etc/ligus/model-gateway-attribution/key
{{- end }}
{{- end -}}

{{/* Mounts for serve and workers. */}}
{{- define "ligus.modelGatewayMounts" -}}
{{- if .Values.modelGateways }}
- name: model-gateways-config
  mountPath: /etc/ligus/model-gateways-config
  readOnly: true
{{- range $g := .Values.modelGateways }}
{{- $a := default dict $g.auth }}
{{- if or (ne (toString $a.mode) "none") (and $g.caSecret $g.caSecret.name) }}
- name: mg-{{ $g.name }}
  mountPath: {{ include "ligus.mg.dir" $g.name }}
  readOnly: true
{{- end }}
{{- end }}
{{- end }}
{{- if .Values.modelGatewayAttribution.secretName }}
- name: model-gateway-attribution
  mountPath: /etc/ligus/model-gateway-attribution
  readOnly: true
{{- end }}
{{- end -}}

{{/* Volumes for serve and workers. */}}
{{- define "ligus.modelGatewayVolumes" -}}
{{- if .Values.modelGateways }}
- name: model-gateways-config
  configMap:
    name: {{ include "ligus.fullname" . }}-model-gateways
{{- range $g := .Values.modelGateways }}
{{- $a := default dict $g.auth }}
{{- $ca := and $g.caSecret $g.caSecret.name }}
{{- if or (ne (toString $a.mode) "none") $ca }}
- name: mg-{{ $g.name }}
  projected:
    defaultMode: 0400
    sources:
      {{- if eq $a.mode "serviceAccountToken" }}
      # Audience-bound and short-lived; the kubelet rotates it at 80 % of
      # its lifetime and Ligus re-reads it on every request.
      - serviceAccountToken:
          audience: {{ default $g.name $a.audience | quote }}
          expirationSeconds: {{ int (default 3600 $a.expirationSeconds) }}
          path: token
      {{- end }}
      {{- if eq $a.mode "apiKey" }}
      - secret:
          name: {{ $a.secretName }}
          items:
            - { key: {{ $a.secretKey }}, path: api-key }
      {{- end }}
      {{- if eq $a.mode "mtls" }}
      - secret:
          name: {{ $a.secretName }}
          items:
            - { key: tls.crt, path: tls.crt }
            - { key: tls.key, path: tls.key }
            {{- if not $ca }}
            - { key: ca.crt, path: ca.crt }
            {{- end }}
      {{- end }}
      {{- if $ca }}
      - secret:
          name: {{ $g.caSecret.name }}
          items:
            - { key: {{ default "ca.crt" $g.caSecret.key }}, path: ca.crt }
      {{- end }}
{{- end }}
{{- end }}
{{- end }}
{{- if .Values.modelGatewayAttribution.secretName }}
- name: model-gateway-attribution
  secret:
    secretName: {{ .Values.modelGatewayAttribution.secretName }}
    defaultMode: 0400
    items:
      - { key: {{ default "key" .Values.modelGatewayAttribution.secretKey }}, path: key }
{{- end }}
{{- end -}}

{{/* Pod annotation so a changed allow-list rolls the pods. */}}
{{- define "ligus.modelGatewayAnnotations" -}}
{{- if .Values.modelGateways }}
checksum/model-gateways: {{ include "ligus.modelGatewaysJson" . | sha256sum }}
{{- end }}
{{- end -}}
