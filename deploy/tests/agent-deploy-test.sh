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

# ligus-local's systemd unit: ExecStart arguments are quoted, with % and $
# escaped (systemd specifiers and expansions), and a line break is refused.
mkdir -p "$TMP/u b%h\$X"
printf '#!/bin/sh\n' >"$TMP/u b%h\$X/ligus"; chmod +x "$TMP/u b%h\$X/ligus"
printf "LIGUS_WORKER_LABEL='my %%n \"box\"'\n" >"$TMP/eu"
set +e
LIGUS_BIN="$TMP/u b%h\$X/ligus" LIGUS_LOCAL_BWRAP=/nonexistent "$LOCAL" unit --role worker --data-dir "$TMP/du" --env-file "$TMP/eu" >"$TMP/unit1" 2>"$TMP/unit1.err"; RC=$?
set -e
want="ExecStart=\"$TMP/u b%%h\$\$X/ligus\" worker --label \"my %%n \\\"box\\\"\""
if [ "$RC" = 0 ] && grep -qxF "$want" "$TMP/unit1"; then pass "systemd unit quotes ExecStart and escapes % and \$"
else fail "systemd unit ExecStart: $(grep ExecStart "$TMP/unit1" 2>/dev/null) (want $want; rc $RC $(cat "$TMP/unit1.err"))"; fi
if grep -qxF "EnvironmentFile=$TMP/eu" "$TMP/unit1" && grep -qxF 'NoNewPrivileges=yes' "$TMP/unit1"; then pass "systemd unit env file and NoNewPrivileges=yes"; else fail "systemd unit env file or NoNewPrivileges"; fi
set +e
LIGUS_BIN="$TMP/x
ExecStartPre=/bin/sh -c id" LIGUS_LOCAL_BWRAP=/nonexistent "$LOCAL" unit --role serve --data-dir "$TMP/du" --env-file "$TMP/eu" >"$TMP/unit2" 2>/dev/null; RC=$?
set -e
if [ "$RC" = 2 ] && ! grep -q ExecStartPre "$TMP/unit2"; then pass "systemd unit refuses a line break"; else fail "systemd unit with a line break: rc $RC"; fi

# NoNewPrivileges against a setuid bwrap: relaxed (with the reason in the
# unit) only when bwrap is setuid and unprivileged user namespaces are off.
: >"$TMP/bwrap-suid"; chmod 4755 "$TMP/bwrap-suid"; : >"$TMP/bwrap-plain"; chmod 755 "$TMP/bwrap-plain"
nnp() { # bwrap userns
  LIGUS_BIN="$TMP/u b%h\$X/ligus" LIGUS_LOCAL_BWRAP=$1 LIGUS_LOCAL_USERNS=$2 "$LOCAL" unit --role serve --data-dir "$TMP/du" --env-file "$TMP/eu" | grep '^NoNewPrivileges='
}
if [ -u "$TMP/bwrap-suid" ]; then
  if [ "$(nnp "$TMP/bwrap-suid" no)" = NoNewPrivileges=no ] \
    && LIGUS_BIN="$TMP/u b%h\$X/ligus" LIGUS_LOCAL_BWRAP="$TMP/bwrap-suid" LIGUS_LOCAL_USERNS=no "$LOCAL" unit --role serve --data-dir "$TMP/du" --env-file "$TMP/eu" | grep '^# Relaxed by ligus-local: .*setuid root' >/dev/null; then
    pass "setuid bwrap without user namespaces relaxes NoNewPrivileges, with the reason"
  else fail "setuid bwrap without user namespaces"; fi
  if [ "$(nnp "$TMP/bwrap-suid" yes)" = NoNewPrivileges=yes ]; then pass "setuid bwrap with user namespaces keeps NoNewPrivileges"; else fail "setuid bwrap with user namespaces"; fi
else
  printf 'skip setuid bwrap checks (no setuid bit on %s)\n' "$TMP"
fi
if [ "$(nnp "$TMP/bwrap-plain" no)" = NoNewPrivileges=yes ]; then pass "a non-setuid bwrap keeps NoNewPrivileges"; else fail "non-setuid bwrap"; fi

# ligus-k8s never falls back to the current kube context.
run "$TMP/c1" "$K8S" verify --json --namespace x --release r
check_json "ligus-k8s without --context is a usage error" "$TMP/c1" 2 "$RC"

# Flag values reach a generated values file: a quote or newline is refused
# before anything is rendered (it would inject YAML past the fail-closed check).
run "$TMP/i1" "$K8S" install --json --context c --namespace x --scenario trial --image-tag 'dev"
agentSandbox: {preflight: {turnTest: false}}
x: "'
check_json "ligus-k8s refuses YAML in --image-tag" "$TMP/i1" 2 "$RC"
run "$TMP/i2" "$K8S" install --json --context c --namespace x --scenario team-external-relay --relay-url 'https://r.example"
networkPolicy: {restrictEgress: false}
y: "'
check_json "ligus-k8s refuses YAML in --relay-url" "$TMP/i2" 2 "$RC"

# The rendered-manifest check catches every fail-closed default it promises.
cat >"$TMP/good.yaml" <<'EOF'
---
kind: NetworkPolicy
metadata:
  name: t-ligus-controller
spec:
  policyTypes:
    - Ingress
    - Egress
---
kind: StatefulSet
spec:
  template:
    spec:
      initContainers:
        - name: sandbox-preflight
          args:
            - doctor
            - --turn-test
          env:
            - name: LIGUS_REQUIRE_SANDBOX
              value: "1"
      containers:
        - name: serve
          env:
            - name: LIGUS_REQUIRE_SANDBOX
              value: "1"
EOF
run "$TMP/r0" "$K8S" check-rendered "$TMP/good.yaml" --json
check_json "check-rendered passes the defaults" "$TMP/r0" 0 "$RC" fail-closed
sed '/^    - Egress$/d' "$TMP/good.yaml" >"$TMP/r1.yaml"
run "$TMP/r1" "$K8S" check-rendered "$TMP/r1.yaml" --json
check_json "check-rendered: restrictEgress off stops for the human" "$TMP/r1" 4 "$RC" fail-closed
sed '/- --turn-test/d' "$TMP/good.yaml" >"$TMP/r2.yaml"
run "$TMP/r2" "$K8S" check-rendered "$TMP/r2.yaml" --json
check_json "check-rendered: turn test off stops for the human" "$TMP/r2" 4 "$RC" fail-closed
awk '/value: "1"/ && ++n == 2 { sub(/"1"/, "\"0\"") } { print }' "$TMP/good.yaml" >"$TMP/r3.yaml"
run "$TMP/r3" "$K8S" check-rendered "$TMP/r3.yaml" --json
check_json "check-rendered: one workload without LIGUS_REQUIRE_SANDBOX=1 stops for the human" "$TMP/r3" 4 "$RC" fail-closed

# ligus-local: the env file is sourced, so its values must never run as code.
run "$TMP/l6" env HOME="$TMP/home" LIGUS_BIN=/usr/bin/true "$LOCAL" configure --json --role serve \
  --data-dir "$TMP/d6" --env-file "$TMP/e6" --label 'box;touch '"$TMP"'/pwned'
check_json "ligus-local refuses shell in --label" "$TMP/l6" 2 "$RC"
if [ ! -e "$TMP/d6" ] && [ ! -e "$TMP/pwned" ]; then pass "refused label created nothing"; else fail "refused label left files"; fi
run "$TMP/l7" env HOME="$TMP/home" LIGUS_BIN=/usr/bin/true "$LOCAL" configure --json --role serve \
  --data-dir "$TMP/d 7" --env-file "$TMP/e7" --label 'My Box'
check_json "ligus-local configure with spaces" "$TMP/l7" 4 "$RC" workspace
# shellcheck disable=SC2016  # $1 and the variables expand in the child shell
if [ "$(env -i bash -c 'set -a; . "$1"; set +a; printf "%s|%s" "$LIGUS_SERVE_LABEL" "$LIGUS_DATA_DIR"' _ "$TMP/e7" 2>&1)" = "My Box|$TMP/d 7" ]; then
  pass "env file with spaces sources cleanly"
else
  fail "env file with spaces does not source cleanly"
fi

# ligus-local uninstall deletes only an env file it wrote.
printf 'precious\n' >"$TMP/foreign.env"
run "$TMP/l8" env HOME="$TMP/home" LIGUS_BIN=/usr/bin/true "$LOCAL" uninstall --json --role serve \
  --data-dir "$TMP/d 7" --env-file "$TMP/foreign.env" --delete-data
check_json "ligus-local uninstall with a foreign env file" "$TMP/l8" 0 "$RC" data
if [ -f "$TMP/foreign.env" ] && [ ! -e "$TMP/d 7" ]; then pass "foreign env file kept, own data dir deleted"; else fail "uninstall removed a file it did not write"; fi

# Only the deploy skills' SKILL.md files under .claude/ can be committed.
if command -v git >/dev/null 2>&1 && git -C "$ROOT" rev-parse >/dev/null 2>&1; then
  gi_ok=1
  for f in .claude/skills/ligus-deploy-k8s/SKILL.md .claude/skills/ligus-deploy-local/SKILL.md; do
    git -C "$ROOT" check-ignore -q --no-index "$f" && gi_ok=0
  done
  for f in .claude/settings.local.json .claude/skills/private/SKILL.md .claude/skills/ligus-deploy-k8s/notes.md .claude/worktrees/x/y; do
    git -C "$ROOT" check-ignore -q --no-index "$f" || gi_ok=0
  done
  if [ $gi_ok = 1 ]; then pass "gitignore: only the deploy skills under .claude/ are tracked"; else fail "gitignore under .claude/"; fi
fi

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
