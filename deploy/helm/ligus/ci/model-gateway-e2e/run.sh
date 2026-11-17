#!/bin/bash
# End-to-end check of Ligus model gateways on a local Kubernetes cluster
# (docs/design/v2/model-gateways.md §12). Throwaway namespaces only; every
# object it creates is deleted at the end, including the Gateway API and
# kgateway CRDs (do not run it on a cluster that already uses them).
#
#   KCTX=orbstack TAG=mgw-e2e ./run.sh
#
# Needs: the ligus image ligus:$TAG and the relay image ligus-relay:$RELAY_TAG
# in the cluster's image store, helm, openssl, curl, and a node with Landlock
# (the chart's sandbox preflight). A stub model sits behind the gateways; no
# real provider is called and no real key is used.
set -euo pipefail
cd "$(dirname "$0")"
KCTX=${KCTX:-orbstack}; TAG=${TAG:-dev}; RELAY_TAG=${RELAY_TAG:-dev}
K="kubectl --context $KCTX"; HELM="helm --kube-context $KCTX"
CHART=../..
W=$(mktemp -d)
cleanup() {
  $HELM uninstall gwe2e -n ligus-gw-e2e >/dev/null 2>&1 || true
  $HELM uninstall ligus-relay -n ligus-relay-gwe2e >/dev/null 2>&1 || true
  $K delete ns ligus-gw-e2e ligus-relay-gwe2e gw-test gw-other --wait=true >/dev/null 2>&1 || true
  $HELM uninstall kgateway -n kgateway-system >/dev/null 2>&1 || true
  $HELM uninstall kgateway-crds -n kgateway-system >/dev/null 2>&1 || true
  $K delete ns kgateway-system >/dev/null 2>&1 || true
  [ -f "$W/gwapi.yaml" ] && $K delete -f "$W/gwapi.yaml" >/dev/null 2>&1 || true
  rm -rf "$W"
}
trap cleanup EXIT

# Gateway API CRDs v1.7.0 and kgateway v2.5.1 (pinned).
curl -sfL -o "$W/gwapi.yaml" https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.7.0/standard-install.yaml
echo "4e2602c4cbe1afbc394d86f4ac6522e1a5263b01133bba1e40a6d1a549e753a1  $W/gwapi.yaml" | shasum -a 256 -c -
$K apply --server-side -f "$W/gwapi.yaml" >/dev/null
$HELM install kgateway-crds oci://cr.kgateway.dev/kgateway-dev/charts/kgateway-crds --version v2.5.1 -n kgateway-system --create-namespace >/dev/null
$HELM install kgateway oci://cr.kgateway.dev/kgateway-dev/charts/kgateway --version v2.5.1 -n kgateway-system --wait --timeout 5m >/dev/null

# The gateway side: a stub model, kgateway with SA-token JWT auth, LiteLLM.
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout "$W/ca.key" -out "$W/ca.crt" -days 1 -subj /CN=gwe2e-ca 2>/dev/null
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout "$W/gw.key" -out "$W/gw.csr" -subj /CN=llm-gw 2>/dev/null
printf "subjectAltName=DNS:llm-gw.gw-test.svc.cluster.local\nextendedKeyUsage=serverAuth\n" > "$W/ext.cnf"
openssl x509 -req -in "$W/gw.csr" -CA "$W/ca.crt" -CAkey "$W/ca.key" -CAcreateserial -out "$W/gw.crt" -days 1 -extfile "$W/ext.cnf" 2>/dev/null
$K create ns gw-test >/dev/null; $K create ns gw-other >/dev/null
$K -n gw-test create configmap stub --from-file=stub.py >/dev/null
$K -n gw-test create secret tls llm-gw-tls --cert="$W/gw.crt" --key="$W/gw.key" >/dev/null
$K apply -f stub.yaml >/dev/null
ISS=$($K get --raw /.well-known/openid-configuration | python3 -c 'import json,sys;print(json.load(sys.stdin)["issuer"])')
JWKS=$($K get --raw /openid/v1/jwks)
python3 - "$ISS" "$JWKS" "$W/kgw.yaml" <<'PY'
import sys
t=open('kgw.yaml.tmpl').read()
t=t.replace('__ISSUER__',sys.argv[1]).replace('__JWKS__',sys.argv[2]).replace('__LIGUS_NS__','ligus-gw-e2e').replace('__LIGUS_SA__','ligus-serve')
open(sys.argv[3],'w').write(t)
PY
$K apply -f "$W/kgw.yaml" >/dev/null
LKEY="sk-litellm-e2e-$(openssl rand -hex 12)"
$K -n gw-test create secret generic litellm-master --from-literal=LITELLM_MASTER_KEY="$LKEY" >/dev/null
$K apply -f litellm.yaml >/dev/null
$K -n gw-test rollout status deploy/stub deploy/litellm --timeout=5m >/dev/null

# The Ligus side: a throwaway relay and workspace, serve with modelGateways.
$K create ns ligus-relay-gwe2e >/dev/null; $K label ns ligus-relay-gwe2e pod-security.kubernetes.io/enforce=restricted >/dev/null
$HELM install ligus-relay "$CHART"/charts/ligus-relay-*.tgz -n ligus-relay-gwe2e --set image.repository=ligus-relay \
  --set image.tag="$RELAY_TAG" --set image.pullPolicy=Never --set database.mode=none --set topology=single --set tunnel.mode=open --wait --timeout 4m >/dev/null
$K create ns ligus-gw-e2e >/dev/null; $K label ns ligus-gw-e2e pod-security.kubernetes.io/enforce=restricted >/dev/null
$K -n ligus-gw-e2e create secret generic ws-gwe2e --from-literal=room="k8s-test-$(uuidgen | tr A-Z a-z)" --from-literal=key="$(openssl rand -base64 36)" >/dev/null
$K -n ligus-gw-e2e create secret generic llm-gw-ca --from-file=ca.crt="$W/ca.crt" >/dev/null
$K -n ligus-gw-e2e create secret generic gw-attr --from-literal=key="$(openssl rand -hex 32)" >/dev/null
$K -n ligus-gw-e2e create secret generic litellm-key --from-literal=key="$LKEY" >/dev/null
sed "s/__TAG__/$TAG/" ligus-values.yaml.tmpl > "$W/values.yaml"
$HELM install gwe2e "$CHART" -n ligus-gw-e2e -f "$W/values.yaml" --wait --timeout 6m >/dev/null

echo "== ligus gateway-check (hosted turn, sandboxed CLI child, unlisted endpoint)"
STATUS=0
$K -n ligus-gw-e2e exec gwe2e-ligus-serve-0 -c serve -- ligus gateway-check || STATUS=$?

echo "== headers the stub model received (credentials as claims or hashes)"
$K -n gw-test logs deploy/stub | python3 -c '
import json,sys
for l in sys.stdin:
    r=json.loads(l); h=r["headers"]
    print(r["path"], {k:v for k,v in h.items() if k in ("authorization","x-api-key","x-k8s-sub","x-ligus-workspace","x-ligus-agent")})'

echo "== kgateway: tokens"
GWIP=$($K -n gw-test get svc llm-gw -o jsonpath='{.spec.clusterIP}')
TOK_OK=$($K create token ligus-serve -n ligus-gw-e2e --audience ligus-llm-gw --duration 10m)
TOK_NS=$($K create token default -n gw-other --audience ligus-llm-gw --duration 10m)
TOK_AUD=$($K create token ligus-serve -n ligus-gw-e2e --audience other --duration 10m)
$K -n gw-other run tokcheck --restart=Never \
  --image=curlimages/curl@sha256:c1fe1679c34d9784c1b0d1e5f62ac0a79fca01fb6377cdd33e90473c6f9f9a69 --command -- sleep 600 >/dev/null
$K -n gw-other wait --for=condition=Ready pod/tokcheck --timeout=120s >/dev/null
r() { $K -n gw-other exec tokcheck -- curl -sk -o /dev/null -w '%{http_code}' "https://$GWIP:8443/v1/chat/completions" \
  -H 'content-type: application/json' -d '{"model":"m","messages":[]}' "$@"; }
echo "ligus serve SA token: $(r -H "authorization: Bearer $TOK_OK")"
echo "token from namespace gw-other: $(r -H "authorization: Bearer $TOK_NS")"
echo "serve SA, wrong audience: $(r -H "authorization: Bearer $TOK_AUD")"
echo "no token: $(r)"
echo "tampered signature: $(r -H "authorization: Bearer ${TOK_OK%.*}.AAAA")"
codes=""; for _ in $(seq 1 45); do codes="$codes $(r -H "authorization: Bearer $TOK_OK")"; done
echo "45 rapid valid requests (local rate limit 40/min): $(echo $codes | tr ' ' '\n' | sort | uniq -c | tr '\n' ' ')"

echo "== NetworkPolicy from the serve pod's network"
STUB=$($K -n gw-test get svc stub -o jsonpath='{.spec.clusterIP}')
LLM=$($K -n gw-test get svc litellm -o jsonpath='{.spec.clusterIP}')
KAPI=$($K get svc kubernetes -o jsonpath='{.spec.clusterIP}')
$K -n ligus-gw-e2e debug gwe2e-ligus-serve-0 --profile=restricted --container=npprobe \
  --image=curlimages/curl@sha256:c1fe1679c34d9784c1b0d1e5f62ac0a79fca01fb6377cdd33e90473c6f9f9a69 -- sh -c "
  for t in kgateway=https://$GWIP:8443/v1/models litellm=http://$LLM:4000/health/liveliness stub-unlisted=http://$STUB:8080/v1/models kube-apiserver=https://$KAPI:443/ metadata=http://169.254.169.254/ internet=https://1.1.1.1/; do
    echo \"\${t%%=*} => \$(curl -sk -m 6 -o /dev/null -w '%{http_code}' \${t#*=})\"; done" >/dev/null
sleep 45; $K -n ligus-gw-e2e logs gwe2e-ligus-serve-0 -c npprobe
exit $STATUS
