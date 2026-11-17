#!/usr/bin/env bash
# Offline tests for the agent deploy tooling (deploy/kubernetes/ligus-k8s,
# deploy/local/ligus-local): shellcheck, exit codes, and the --json schema.
# Needs no cluster and runs no real ligus binary.
#   bash deploy/tests/agent-deploy-test.sh
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
K8S="$ROOT/deploy/kubernetes/ligus-k8s"
LOCAL="$ROOT/deploy/local/ligus-local"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
fails=0
pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; fails=$((fails+1)); }

# Validate one --json document: the shared schema, and the expected exit code.
check_json() { # name file want-exit got-exit [required-check-id]
  local name=$1 file=$2 want=$3 got=$4 id=${5:-}
  if [ "$got" != "$want" ]; then fail "$name: exit $got, want $want"; return; fi
  if python3 -I - "$file" "$want" "$id" <<'EOF'
import json, sys
path, want, cid = sys.argv[1], int(sys.argv[2]), sys.argv[3]
with open(path) as f:
    text = f.read()
lines = [l for l in text.splitlines() if l.strip()]
assert len(lines) == 1, "stdout must be exactly one JSON line, got %d" % len(lines)
d = json.loads(lines[0])
assert d["schema"] == "ligus.agent-deploy.v1", d.get("schema")
for k, t in (("tool", str), ("command", str), ("ok", bool), ("exitCode", int), ("checks", list), ("next", list)):
    assert isinstance(d.get(k), t), "%s missing or not %s" % (k, t.__name__)
assert d["exitCode"] == want, (d["exitCode"], want)
assert d["ok"] == (want == 0)
for c in d["checks"]:
    assert set(c) <= {"id", "status", "detail", "fix"}, c
    assert c["status"] in ("pass", "warn", "fail", "info", "skip"), c
    assert isinstance(c["id"], str) and isinstance(c["detail"], str)
    if c["status"] == "fail":
        assert c.get("fix"), "a failed check must say what to do next: %r" % c
if cid:
    assert any(c["id"] == cid for c in d["checks"]), "no check %s" % cid
EOF
  then pass "$name"; else fail "$name"; fi
}

run() { # out-file cmd...  (sets RC)
  local out=$1; shift
  set +e; "$@" >"$out" 2>"$out.err"; RC=$?; set -e
}

# Lint the scripts.
if command -v shellcheck >/dev/null 2>&1; then
  if shellcheck -x "$K8S" "$LOCAL" "$ROOT/deploy/lib/agent-common.sh" "${BASH_SOURCE[0]}"; then pass shellcheck; else fail shellcheck; fi
else
  printf 'skip shellcheck (not installed)\n'
fi

# --help exits 0 and prints usage
for s in "$K8S" "$LOCAL"; do
  run "$TMP/h" "$s" --help
  if [ "$RC" = 0 ] && grep -q preflight "$TMP/h"; then pass "$(basename "$s") --help"; else fail "$(basename "$s") --help"; fi
done

# Usage errors: exit 2 with a JSON document
run "$TMP/u1" "$K8S" nonsense --json; check_json "ligus-k8s unknown command" "$TMP/u1" 2 "$RC"
run "$TMP/u2" "$K8S" install --json --namespace x; check_json "ligus-k8s install without --scenario" "$TMP/u2" 2 "$RC"
run "$TMP/u3" "$K8S" install --json --namespace x --scenario team-external-relay; check_json "ligus-k8s external relay without --relay-url" "$TMP/u3" 2 "$RC"
run "$TMP/u4" "$LOCAL" start --json --data-dir "$TMP/d"; check_json "ligus-local start without --role" "$TMP/u4" 2 "$RC"

# An unreachable context: exit 3, says what to do
if command -v kubectl >/dev/null 2>&1 && command -v helm >/dev/null 2>&1; then
  run "$TMP/p1" env KUBECONFIG="$TMP/empty-kubeconfig" "$K8S" preflight --json --context agentdeploy-does-not-exist
  check_json "ligus-k8s preflight, unreachable context" "$TMP/p1" 3 "$RC" kube.reachable
  run "$TMP/p2" env KUBECONFIG="$TMP/empty-kubeconfig" "$K8S" secrets workspace --json --namespace x --context agentdeploy-does-not-exist
  check_json "ligus-k8s secrets, unreachable context" "$TMP/p2" 3 "$RC"
else
  printf 'skip ligus-k8s cluster-less checks (kubectl or helm missing)\n'
fi

# ligus-local: no binary -> 3; the desktop app's data dir -> 4; 0.0.0.0 -> 4
mkdir -p "$TMP/home"
run "$TMP/l1" env HOME="$TMP/home" LIGUS_BIN="$TMP/no-such-ligus" PATH=/usr/bin:/bin "$LOCAL" preflight --json
check_json "ligus-local preflight without a binary" "$TMP/l1" 3 "$RC" cli.binary
run "$TMP/l2" env HOME="$TMP/home" LIGUS_BIN=/usr/bin/true "$LOCAL" configure --json --role serve \
  --data-dir "$TMP/home/Library/Application Support/ai.ligus.desktop" --env-file "$TMP/e1"
check_json "ligus-local refuses the desktop app's data dir" "$TMP/l2" 4 "$RC" data.dir
run "$TMP/l3" env HOME="$TMP/home" LIGUS_BIN=/usr/bin/true "$LOCAL" configure --json --role serve \
  --data-dir "$TMP/d3" --env-file "$TMP/e3" --listen 0.0.0.0:8787
check_json "ligus-local refuses 0.0.0.0" "$TMP/l3" 4 "$RC" listen.exposure
if [ ! -e "$TMP/d3" ]; then pass "refused configure created nothing"; else fail "refused configure created $TMP/d3"; fi
run "$TMP/l4" env HOME="$TMP/home" LIGUS_BIN=/usr/bin/true "$LOCAL" configure --json --role worker \
  --data-dir "$TMP/d4" --env-file "$TMP/e4"
check_json "ligus-local configure without a workspace stops for the human" "$TMP/l4" 4 "$RC" workspace
if [ -f "$TMP/e4" ] && [ "$(stat -c %a "$TMP/e4" 2>/dev/null || stat -f %Lp "$TMP/e4")" = 600 ]; then pass "env file is 0600"; else fail "env file mode"; fi
mkdir -p "$TMP/notours" && : >"$TMP/notours/keep"
run "$TMP/l5" env HOME="$TMP/home" LIGUS_BIN=/usr/bin/true "$LOCAL" uninstall --json --role serve --data-dir "$TMP/notours" --env-file "$TMP/e5" --delete-data
check_json "ligus-local never deletes a data dir it did not create" "$TMP/l5" 4 "$RC" data
if [ -f "$TMP/notours/keep" ]; then pass "foreign data dir kept"; else fail "foreign data dir deleted"; fi

# Scenario values files parse and keep the fail-closed defaults on.
if python3 -I - "$ROOT/deploy/kubernetes/scenarios" <<'EOF'
import os, sys
try:
    import yaml
except ImportError:
    print("skip scenario parse (no PyYAML)"); sys.exit(0)
d = sys.argv[1]
for f in sorted(os.listdir(d)):
    v = yaml.safe_load(open(os.path.join(d, f)))
    np = v.get("networkPolicy", {})
    assert np.get("enabled", True) and np.get("restrictEgress", True), f
    assert "agentSandbox" not in v, "%s must not touch agentSandbox" % f
    for comp in ("serve", "worker"):
        assert v.get(comp, {}).get("service", {}).get("type", "ClusterIP") == "ClusterIP", f
EOF
then pass "scenario values keep the defaults"; else fail "scenario values"; fi

if [ $fails -gt 0 ]; then printf '%d failed\n' "$fails"; exit 1; fi
printf 'all passed\n'
