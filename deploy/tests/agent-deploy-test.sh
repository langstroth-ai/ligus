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

# The JSON is a conforming profile of ligus.checks.v1, and the help says so.
for s in "$K8S" "$LOCAL"; do
  run "$TMP/h" "$s" --help
  if grep -q 'ligus.checks.v1' "$TMP/h"; then pass "$(basename "$s") --help names ligus.checks.v1"; else fail "$(basename "$s") --help names ligus.checks.v1"; fi
done
if grep -q 'ligus.checks.v1' "$ROOT/AGENTS.md" && grep -q 'ligus.agent-deploy.v1' "$ROOT/docs-site/docs/operate/reference/cli-conventions.md"; then
  pass "AGENTS.md and the CLI conventions page declare the profile"
else
  fail "AGENTS.md and the CLI conventions page declare the profile"
fi

# Help is complete and cannot drift: the command list comes from each
# script's dispatcher (the last `case $CMD in`) and the flag list from its
# argument loop's case arms, so a new command or flag without help fails.
dispatch_cmds() { # script -> one command name per line (aliases included)
  local start
  # shellcheck disable=SC2016  # a literal $ in the pattern
  start=$(grep -n '^case \$CMD in$' "$1" | tail -1 | cut -d: -f1)
  tail -n "+$start" "$1" | sed '/^esac/q' | sed -n 's/^  \([a-z][a-z|-]*\)).*/\1/p' | tr '|' '\n'
}
parser_flags() { # script -> one flag per line
  sed -n '/^while \[ \$# -gt 0 \]; do$/,/^done$/p' "$1" | sed -n 's/^    \(-[-a-z|]*\)).*/\1/p' | tr '|' '\n'
}
has_word() { grep -qE -- "(^|[^A-Za-z0-9-])$1([^A-Za-z0-9-]|\$)" "$2"; }
# One exit-code table for every Ligus tool: help exit-codes prints 0 to 7,
# and every command's help points to it.
for s in "$K8S" "$LOCAL"; do
  n=$(basename "$s")
  run "$TMP/x" "$s" help exit-codes
  ok=1
  for c in 0 1 2 3 4 5 6 7; do grep -qE "^  $c  " "$TMP/x" || ok=0; done
  if [ "$RC" = 0 ] && [ $ok = 1 ]; then pass "$n help exit-codes lists 0 to 7"; else fail "$n help exit-codes lists 0 to 7"; fi
  missing=""
  for c in $(dispatch_cmds "$s"); do
    run "$TMP/x" "$s" help "$c"
    grep -q "$n help exit-codes" "$TMP/x" || missing="$missing $c"
  done
  if [ -z "$missing" ]; then pass "$n: every command's help names the shared table"; else fail "$n: no shared table in help for:$missing"; fi
done

for s in "$K8S" "$LOCAL"; do
  n=$(basename "$s")
  cmds=$(dispatch_cmds "$s"); flags=$(parser_flags "$s")
  if [ "$(printf '%s\n' "$cmds" | grep -c .)" -ge 5 ] && [ "$(printf '%s\n' "$flags" | grep -c .)" -ge 10 ]; then
    pass "$n: dispatcher and flags found ($(printf '%s' "$cmds" | tr '\n' ' '))"
  else fail "$n: could not read the dispatcher or the flags"; fi
  run "$TMP/top" "$s" --help
  run "$TMP/top2" "$s" help
  if [ "$RC" = 0 ] && cmp -s "$TMP/top" "$TMP/top2"; then pass "$n help = --help"; else fail "$n help differs from --help"; fi
  missing=""
  for hd in NAME USAGE COMMANDS FLAGS 'ONE-SHOT SEQUENCE' EXAMPLES ENVIRONMENT 'EXIT CODES' 'SEE ALSO' 'ligus help agents-guide'; do
    grep -qF -- "$hd" "$TMP/top" || missing="$missing [$hd]"
  done
  for c in $cmds; do has_word "$c" "$TMP/top" || missing="$missing cmd:$c"; done
  for f in $flags; do has_word "$f" "$TMP/top" || missing="$missing $f"; done
  for code in 0 1 2 3 4; do grep -qE "^  $code  " "$TMP/top" || missing="$missing exit:$code"; done
  if [ -z "$missing" ]; then pass "$n --help names every command, flag and exit code"; else fail "$n --help lacks:$missing"; fi
  for c in $cmds; do
    run "$TMP/hc1" "$s" help "$c"; rc1=$RC
    run "$TMP/hc2" "$s" "$c" --help; rc2=$RC
    missing=""
    for hd in NAME USAGE FLAGS EXAMPLES ENVIRONMENT 'EXIT CODES' 'SEE ALSO' "$n help <command>" 'ligus help '; do
      grep -qF -- "$hd" "$TMP/hc1" || missing="$missing [$hd]"
    done
    has_word "$c" "$TMP/hc1" || missing="$missing [$c]"
    if [ "$rc1" = 0 ] && [ "$rc2" = 0 ] && [ -z "$missing" ] && cmp -s "$TMP/hc1" "$TMP/hc2" \
      && ! grep -q '^COMMAND DETAILS' "$TMP/hc1"; then
      pass "$n help $c / $c --help"
    else fail "$n help $c: rc $rc1/$rc2, lacks:$missing"; fi
  done
  # One section order for every Ligus tool, in each command's help.
  order_bad=""
  for c in $cmds; do
    run "$TMP/ho" "$s" help "$c"
    last=0
    for hd in NAME USAGE FLAGS EXAMPLES ENVIRONMENT 'EXIT CODES' 'SEE ALSO'; do
      ln=$(grep -nx -- "$hd" "$TMP/ho" | head -1 | cut -d: -f1)
      if [ -z "$ln" ] || [ "$ln" -lt "$last" ]; then order_bad="$order_bad $c:$hd"; break; fi
      last=$ln
    done
  done
  if [ -z "$order_bad" ]; then pass "$n: every command's help keeps the shared section order"; else fail "$n: section order:$order_bad"; fi
  run "$TMP/hx" "$s" help no-such-command
  if [ "$RC" = 2 ] && [ ! -s "$TMP/hx" ]; then pass "$n help <unknown> is a usage error"; else fail "$n help <unknown>: rc $RC"; fi
done

# verify --json emits the schema without a cluster or a running process.
mkdir -p "$TMP/vh"
run "$TMP/v1" env HOME="$TMP/vh" LIGUS_LOCAL_OS=Linux PATH=/usr/bin:/bin LIGUS_BIN=/usr/bin/true \
  "$LOCAL" verify --json --role serve --data-dir "$TMP/vd" --env-file "$TMP/ve"
check_json "ligus-local verify --json, nothing running" "$TMP/v1" 1 "$RC" process
if python3 -I -c 'import json,sys; d=json.load(open(sys.argv[1])); assert d["command"]=="verify" and d["role"]=="serve" and d["dataDir"] and d["envFile"]' "$TMP/v1"; then
  pass "ligus-local verify --json names the role, data dir and env file"
else fail "ligus-local verify --json extra members"; fi
run "$TMP/v2" env KUBECONFIG="$TMP/empty-kubeconfig" "$K8S" verify --json --context agentdeploy-does-not-exist -n x
if command -v kubectl >/dev/null 2>&1 && command -v helm >/dev/null 2>&1 && command -v openssl >/dev/null 2>&1; then
  check_json "ligus-k8s verify --json, unreachable context" "$TMP/v2" 3 "$RC" kube.reachable
else
  check_json "ligus-k8s verify --json, tools missing" "$TMP/v2" 3 "$RC"
fi

# Usage errors: exit 2 with a JSON document
run "$TMP/u1" "$K8S" nonsense --json; check_json "ligus-k8s unknown command" "$TMP/u1" 2 "$RC"
run "$TMP/u2" "$K8S" install --json --namespace x; check_json "ligus-k8s install without --scenario" "$TMP/u2" 2 "$RC"
run "$TMP/u3" "$K8S" install --json --namespace x --scenario team-external-relay; check_json "ligus-k8s external relay without --relay-url" "$TMP/u3" 2 "$RC"
run "$TMP/u4" "$LOCAL" start --json --data-dir "$TMP/d"; check_json "ligus-local start without --role" "$TMP/u4" 2 "$RC"

# Errors share one shape: "<tool>: <what>", an indented fix line naming the
# help, and with --json (even after the bad flag) a failed "usage" check.
run "$TMP/u5" "$K8S" verify --nope --json
check_json "ligus-k8s a bad flag before --json still prints JSON" "$TMP/u5" 2 "$RC" usage
if head -1 "$TMP/u5.err" | grep -qx 'ligus-k8s: unknown argument: --nope' && grep -qx '  fix: see ligus-k8s verify --help' "$TMP/u5.err"; then
  pass "ligus-k8s error shape: what, then fix"
else
  fail "ligus-k8s error shape: $(cat "$TMP/u5.err")"
fi
run "$TMP/u6" "$LOCAL" frob
if head -1 "$TMP/u6.err" | grep -q '^ligus-local: unknown command: frob' && grep -qx '  fix: see ligus-local --help' "$TMP/u6.err"; then
  pass "ligus-local error shape: what, then fix"
else
  fail "ligus-local error shape: $(cat "$TMP/u6.err")"
fi

# --flag=value reads the same as --flag value.
run "$TMP/eq1" env HOME="$TMP/vh" LIGUS_LOCAL_OS=Linux PATH=/usr/bin:/bin LIGUS_BIN=/usr/bin/true \
  "$LOCAL" verify --json --role=serve --data-dir="$TMP/vd" --env-file="$TMP/ve"
check_json "ligus-local verify --flag=value" "$TMP/eq1" 1 "$RC" process
if cmp -s "$TMP/v1" "$TMP/eq1"; then pass "--flag=value gives the same result as --flag value"; else fail "--flag=value differs from --flag value"; fi
run "$TMP/eq2" "$K8S" install --json --context=x --namespace=x --scenario=team-external-relay
check_json "ligus-k8s --flag=value (external relay without --relay-url)" "$TMP/eq2" 2 "$RC"
if grep -q 'needs --relay-url' "$TMP/eq2"; then pass "ligus-k8s reads --scenario=VALUE"; else fail "ligus-k8s --scenario=VALUE not read: $(cat "$TMP/eq2")"; fi

# -o json is --json; -o text undoes it; -q prints only warn and FAIL checks.
run "$TMP/o1" env HOME="$TMP/vh" LIGUS_LOCAL_OS=Linux PATH=/usr/bin:/bin LIGUS_BIN=/usr/bin/true \
  "$LOCAL" verify -o json --role serve --data-dir "$TMP/vd" --env-file "$TMP/ve"
check_json "ligus-local -o json" "$TMP/o1" 1 "$RC" process
run "$TMP/o2" "$LOCAL" verify -o yaml
if [ "$RC" = 2 ]; then pass "-o takes json or text only"; else fail "-o yaml: rc $RC"; fi
# shellcheck disable=SC2016  # $1 expands in the inner bash
q=$(env -u NO_COLOR bash -c '. "$1"; AC_QUIET=1; ac_check a fail x fix; ac_check b warn y; ac_check c pass z; ac_check d info i; ac_check e skip s' _ "$ROOT/deploy/lib/agent-common.sh" 2>&1 >/dev/null)
case $q in *"a: x"*"b: y"*) case $q in *"c: z"*|*"d: i"*|*"e: s"*) fail "-q still prints ok, info and skip: $q" ;; *) pass "-q prints only warn and FAIL" ;; esac ;; *) fail "-q dropped a failure or warning: $q" ;; esac

# Colour: [FAIL] and [WARN] are coloured only on a terminal (or when asked),
# never with NO_COLOR or --color never, and never on stdout.
marks() { # env... -> stderr of one fail, one warn and one pass check
  # shellcheck disable=SC2016,SC2069  # $1 expands in the inner bash; only stderr is kept
  env -u NO_COLOR -u CLICOLOR_FORCE "$@" bash -c '. "$1"; [ -n "${COLOR:-}" ] && ac_set_color "$COLOR"; ac_check a fail x fix; ac_check b warn y; ac_check c pass z' _ "$ROOT/deploy/lib/agent-common.sh" 2>&1 >/dev/null
}
esc=$(printf '\033')
has_esc() { case $1 in *"$esc"*) return 0 ;; esac; return 1; }
if ! has_esc "$(marks)"; then pass "no colour when stderr isn't a terminal"; else fail "colour on a pipe"; fi
out=$(marks COLOR=always)
case $out in *"${esc}[1;31m[FAIL]${esc}[0m a: x"*"${esc}[33m[WARN]${esc}[0m b: y"*) red_yellow=1 ;; *) red_yellow=0 ;; esac
if [ "$red_yellow" = 1 ] && ! has_esc "$(printf '%s\n' "$out" | grep "c: z")"; then pass "--color always colours FAIL red and WARN yellow, not ok"; else fail "--color always: $out"; fi
if has_esc "$(marks CLICOLOR_FORCE=1)"; then pass "CLICOLOR_FORCE colours a pipe"; else fail "CLICOLOR_FORCE"; fi
if ! has_esc "$(marks CLICOLOR_FORCE=1 NO_COLOR=1)"; then pass "NO_COLOR wins over CLICOLOR_FORCE"; else fail "NO_COLOR with CLICOLOR_FORCE"; fi
if ! has_esc "$(marks CLICOLOR_FORCE=1 COLOR=never)"; then pass "--color never wins over CLICOLOR_FORCE"; else fail "--color never"; fi
# On a terminal (a pseudo-terminal from python): colour unless NO_COLOR.
on_tty() { # NO_COLOR value ("" = unset) -> what the terminal showed
  python3 -I - "$ROOT/deploy/lib/agent-common.sh" "$1" <<'PY'
import os, pty, sys
lib, no_color = sys.argv[1], sys.argv[2]
env = {k: v for k, v in os.environ.items() if k not in ("NO_COLOR", "CLICOLOR_FORCE")}
if no_color:
    env["NO_COLOR"] = no_color
pid, fd = pty.fork()
if pid == 0:
    os.execvpe("bash", ["bash", "-c", '. "$1"; ac_check a fail x fix', "_", lib], env)
out = b""
while True:
    try:
        chunk = os.read(fd, 1024)
    except OSError:
        break
    if not chunk:
        break
    out += chunk
os.waitpid(pid, 0)
sys.stdout.write(out.decode(errors="replace"))
PY
}
if has_esc "$(on_tty "")"; then pass "colour on a terminal"; else fail "no colour on a terminal"; fi
if ! has_esc "$(on_tty 1)"; then pass "NO_COLOR turns colour off on a terminal"; else fail "NO_COLOR on a terminal"; fi
run "$TMP/c1" env HOME="$TMP/vh" LIGUS_LOCAL_OS=Linux PATH=/usr/bin:/bin LIGUS_BIN=/usr/bin/true \
  "$LOCAL" verify --json --color=always --role serve --data-dir "$TMP/vd" --env-file "$TMP/ve"
check_json "ligus-local --color=always --json" "$TMP/c1" 1 "$RC" process
if has_esc "$(cat "$TMP/c1.err")" && ! has_esc "$(cat "$TMP/c1")"; then pass "--color always colours stderr, never the JSON"; else fail "--color always: stderr or stdout wrong"; fi
run "$TMP/c2" "$K8S" verify --json --color sometimes --context x -n x
check_json "ligus-k8s --color with a bad value" "$TMP/c2" 2 "$RC"

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

# ligus-local's LaunchAgent (start --launchd): the plist is rendered into a
# temp HOME and checked; install, status and removal run against a FAKE
# launchctl (LIGUS_LOCAL_LAUNCHCTL and first on PATH): nothing is ever loaded
# into the real launchd, and the real ~/Library is never touched.
H="$TMP/la-home"; mkdir -p "$H" "$TMP/fakebin" "$TMP/lc" "$TMP/b &<\"'>"
LB="$TMP/b &<\"'>/ligus"; printf '#!/bin/sh\n' >"$LB"; chmod +x "$LB"
cat >"$TMP/fakebin/launchctl" <<'FAKE'
#!/bin/sh
# Fake launchctl: records calls; "loaded" agents are files in $FAKE_LAUNCHCTL_DIR.
d=${FAKE_LAUNCHCTL_DIR:?}
printf '%s\n' "$*" >>"$d/calls"
case $1 in
  enable|disable) exit 0 ;;
  bootstrap)
    label=$(awk 'p { sub(/^[ \t]*<string>/, ""); sub(/<\/string>.*/, ""); print; exit } /<key>Label<\/key>/ { p = 1 }' "$3")
    if [ -e "$d/loaded.$label" ]; then echo "Bootstrap failed: 5: Input/output error" >&2; exit 5; fi
    cp "$3" "$d/loaded.$label" ;;
  bootout)
    label=${2##*/}
    if [ ! -e "$d/loaded.$label" ]; then echo "Boot-out failed: 3: No such process" >&2; exit 3; fi
    rm -f "$d/loaded.$label" ;;
  print)
    label=${2##*/}
    if [ ! -e "$d/loaded.$label" ]; then echo "Could not find service \"$label\" in domain" >&2; exit 113; fi
    printf '%s = {\n\tactive count = 1\n\tstate = running\n\tpid = 4242\n}\n' "$2" ;;
  *) echo "fake launchctl: unexpected: $*" >&2; exit 64 ;;
esac
FAKE
chmod +x "$TMP/fakebin/launchctl"
la() { # ligus-local with the fake launchd, the temp HOME and the awkward binary path
  env HOME="$H" PATH="$TMP/fakebin:$PATH" LIGUS_LOCAL_OS=Darwin LIGUS_LOCAL_LAUNCHCTL="$TMP/fakebin/launchctl" \
    FAKE_LAUNCHCTL_DIR="$TMP/lc" LIGUS_BIN="$LB" "$LOCAL" "$@"
}
la_env() { # file data-dir role label: an env file as configure writes it, plus extras
  local lk=LIGUS_SERVE_LABEL; [ "$3" = worker ] && lk=LIGUS_WORKER_LABEL
  mkdir -p "$2"
  { echo "# Written by deploy/local/ligus-local configure."
    echo "LIGUS_DATA_DIR='$2'"
    echo "LIGUS_REQUIRE_SANDBOX=1"
    echo "LIGUS_RELAY_URL='https://relay.example/v1?a=1&b=<2>'"
    echo "$lk='$4'"
    echo "LIGUS_WORKSPACE='local-trial-test'"
    echo "LIGUS_WORKSPACE_KEY=not-a-real-secret"
    echo "LIGUS_WORKSPACE_KEY_FILE='$2/throwaway-workspace.key'"
    echo "export LIGUS_REPOS_DIR=\"$TMP/my repos\""
    echo "LIGUS_NTFY_URL=https://bee:not-a-real-pass@ntfy.example"
  } >"$1"; chmod 600 "$1"
}
# shellcheck disable=SC2016  # the $ in the label is literal
la_env "$TMP/lw.env" "$TMP/lw data" worker 'My Box & "co" <1> $HOME'
set +e
la unit --launchd --role worker --data-dir "$TMP/lw data" --env-file "$TMP/lw.env" >"$TMP/p1.plist" 2>"$TMP/p1.err"; RC=$?
set -e
if [ "$RC" = 0 ] && python3 -I - "$TMP/p1.plist" "$LB" "$TMP/lw data" "$H" "$TMP" <<'PY'
import plistlib, sys
path, binp, data, home, tmp = sys.argv[1:]
with open(path, "rb") as f:
    p = plistlib.load(f)
label = 'My Box & "co" <1> $HOME'
assert p["Label"] == "ai.ligus.worker.my-box-co-1-home", p["Label"]
assert p["ProgramArguments"] == [binp, "worker", "--label", label], p["ProgramArguments"]
e = p["EnvironmentVariables"]
assert e["LIGUS_DATA_DIR"] == data and e["LIGUS_REQUIRE_SANDBOX"] == "1"
assert e["LIGUS_RELAY_URL"] == "https://relay.example/v1?a=1&b=<2>", e["LIGUS_RELAY_URL"]
assert e["LIGUS_WORKER_LABEL"] == label and e["LIGUS_REPOS_DIR"] == tmp + "/my repos"
assert e["LIGUS_WORKSPACE_KEY_FILE"] == data + "/throwaway-workspace.key"
assert "LIGUS_WORKSPACE_KEY" not in e, "a secret-looking setting reached the plist"
assert "LIGUS_NTFY_URL" not in e, "a URL with credentials reached the plist"
assert e.get("PATH"), "no PATH"
assert p["WorkingDirectory"] == data
log = home + "/Library/Logs/ligus/worker.my-box-co-1-home.log"
assert p["StandardOutPath"] == log and p["StandardErrorPath"] == log, p["StandardOutPath"]
assert p["RunAtLoad"] is True and p["KeepAlive"] is True and p["ThrottleInterval"] == 10
assert p["ProcessType"] == "Background" and p["Umask"] == 0o077 and p["ExitTimeOut"] == 20
PY
then pass "launchd plist: arguments, environment, escaping (spaces, quotes, &, <, \$), no secrets"
else fail "launchd plist: rc $RC $(cat "$TMP/p1.err")"; fi
if grep -Eq 'not-a-real-(secret|pass)' "$TMP/p1.plist"; then fail "launchd plist carries a secret value"; else pass "launchd plist leaves the secret value out"; fi
if grep -q 'LIGUS_WORKSPACE_KEY LIGUS_NTFY_URL' "$TMP/p1.err"; then pass "launchd unit names the setting it left out"; else fail "launchd unit: no note about the skipped setting"; fi
if command -v plutil >/dev/null 2>&1; then
  if plutil -lint "$TMP/p1.plist" >/dev/null; then pass "launchd plist passes plutil -lint"; else fail "plutil -lint"; fi
else
  printf 'skip plutil -lint (not macOS)\n'
fi
if [ ! -e "$H/Library" ]; then pass "unit --launchd writes nothing"; else fail "unit --launchd wrote under HOME"; fi
set +e
env HOME="$H" LIGUS_BIN="$TMP/x
<string>evil</string>" "$LOCAL" unit --launchd --role serve --data-dir "$TMP/lw data" --env-file "$TMP/lw.env" >"$TMP/p2.plist" 2>/dev/null; RC=$?
set -e
if [ "$RC" = 2 ] && ! grep -q evil "$TMP/p2.plist"; then pass "launchd plist refuses a line break"; else fail "launchd plist with a line break: rc $RC"; fi
run "$TMP/la0" env HOME="$H" LIGUS_LOCAL_OS=Linux LIGUS_BIN="$LB" "$LOCAL" start --launchd --json --role worker --data-dir "$TMP/lw data" --env-file "$TMP/lw.env"
check_json "ligus-local start --launchd off macOS" "$TMP/la0" 3 "$RC" launchd
run "$TMP/la0b" "$LOCAL" start --launchd --systemd --json --role serve --data-dir "$TMP/x"
check_json "ligus-local --launchd with --systemd is a usage error" "$TMP/la0b" 2 "$RC"

# Install, reinstall, status, a second box, a label clash, relabel, removal.
LA="$H/Library/LaunchAgents"; UID_=$(id -u)
la_env "$TMP/ls.env" "$TMP/ls" serve "$(hostname -s 2>/dev/null || echo local)-serve"
run "$TMP/la1" la start --launchd --json --role serve --data-dir "$TMP/ls" --env-file "$TMP/ls.env"
check_json "ligus-local start --launchd" "$TMP/la1" 0 "$RC" start
mode() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"; }
if [ -f "$LA/ai.ligus.serve.plist" ] && [ "$(mode "$LA/ai.ligus.serve.plist")" = 644 ] && [ -e "$TMP/lc/loaded.ai.ligus.serve" ]; then
  pass "launchd agent ai.ligus.serve installed (0644) and bootstrapped"
else fail "launchd install: $(ls -la "$LA" 2>&1)"; fi
if [ "$(mode "$H/Library/Logs/ligus")" = 700 ] && [ -f "$H/Library/Logs/ligus/serve.log" ]; then pass "launchd log dir is owner-only"; else fail "launchd log dir mode"; fi
if grep -qxF "bootstrap gui/$UID_ $LA/ai.ligus.serve.plist" "$TMP/lc/calls"; then pass "launchctl bootstrap gui/<uid> used"; else fail "launchctl calls: $(cat "$TMP/lc/calls")"; fi
run "$TMP/la2" la start --launchd --json --role serve --data-dir "$TMP/ls" --env-file "$TMP/ls.env"
check_json "ligus-local start --launchd again (reinstall)" "$TMP/la2" 0 "$RC" start
if [ "$(grep -c "^bootout gui/$UID_/ai.ligus.serve\$" "$TMP/lc/calls")" = 1 ] && [ "$(grep -c '^bootstrap ' "$TMP/lc/calls")" = 2 ] \
  && [ "$(find "$LA" -type f | wc -l | tr -d ' ')" = 1 ]; then pass "reinstall boots the loaded agent out first, one plist"
else fail "reinstall: $(cat "$TMP/lc/calls"; ls -a "$LA")"; fi
run "$TMP/la3" la status --json --role serve --data-dir "$TMP/ls" --env-file "$TMP/ls.env"
check_json "ligus-local status with the launchd agent" "$TMP/la3" 0 "$RC" launchd
if grep -q '"pid 4242' "$TMP/la3" || grep -q 'pid 4242' "$TMP/la3"; then pass "status reports the launchd pid"; else fail "status: $(cat "$TMP/la3")"; fi
run "$TMP/la4" la start --json --role serve --data-dir "$TMP/ls" --env-file "$TMP/ls.env"
check_json "ligus-local start (no flag) leaves the launchd box alone" "$TMP/la4" 0 "$RC" start
if [ ! -e "$TMP/ls/ligus-local-serve.pid" ]; then pass "no second process next to the launchd one"; else fail "plain start started a second process"; fi
run "$TMP/la5" la stop --json --role serve --data-dir "$TMP/ls" --env-file "$TMP/ls.env"
check_json "ligus-local stop (no flag) points at --launchd" "$TMP/la5" 1 "$RC" stop
la_env "$TMP/lw2.env" "$TMP/lw2" worker 'shed'
run "$TMP/la6" la start --launchd --json --role worker --data-dir "$TMP/lw2" --env-file "$TMP/lw2.env"
check_json "ligus-local start --launchd, a second box" "$TMP/la6" 0 "$RC" start
if [ -f "$LA/ai.ligus.worker.shed.plist" ] && [ -f "$LA/ai.ligus.serve.plist" ] && [ -f "$H/Library/Logs/ligus/worker.shed.log" ]; then pass "two boxes get distinct labels, plists and logs"; else fail "second box: $(ls "$LA")"; fi
la_env "$TMP/lw3.env" "$TMP/lw3" worker 'Shed'
cp "$LA/ai.ligus.worker.shed.plist" "$TMP/shed.before"
run "$TMP/la7" la start --launchd --json --role worker --data-dir "$TMP/lw3" --env-file "$TMP/lw3.env"
check_json "ligus-local start --launchd refuses another box's label" "$TMP/la7" 1 "$RC" launchd.label
if cmp -s "$TMP/shed.before" "$LA/ai.ligus.worker.shed.plist"; then pass "the other box's plist is untouched"; else fail "label clash overwrote a plist"; fi
la_env "$TMP/lw2.env" "$TMP/lw2" worker 'barn'
run "$TMP/la8" la start --launchd --json --role worker --data-dir "$TMP/lw2" --env-file "$TMP/lw2.env"
check_json "ligus-local start --launchd after a relabel" "$TMP/la8" 0 "$RC" start
if [ -f "$LA/ai.ligus.worker.barn.plist" ] && [ ! -e "$LA/ai.ligus.worker.shed.plist" ] && [ ! -e "$TMP/lc/loaded.ai.ligus.worker.shed" ]; then pass "relabel replaces the old agent"; else fail "relabel: $(ls "$LA")"; fi
run "$TMP/la9" la stop --launchd --json --role serve --data-dir "$TMP/ls" --env-file "$TMP/ls.env"
check_json "ligus-local stop --launchd" "$TMP/la9" 0 "$RC" stop
if [ ! -e "$LA/ai.ligus.serve.plist" ] && [ ! -e "$TMP/lc/loaded.ai.ligus.serve" ]; then pass "stop --launchd unloads and removes the plist"; else fail "stop --launchd left the agent"; fi
run "$TMP/la10" la status --launchd --json --role serve --data-dir "$TMP/ls" --env-file "$TMP/ls.env"
check_json "ligus-local status --launchd after removal" "$TMP/la10" 1 "$RC" launchd
run "$TMP/la11" la stop --launchd --json --role serve --data-dir "$TMP/ls" --env-file "$TMP/ls.env"
check_json "ligus-local stop --launchd twice" "$TMP/la11" 0 "$RC" stop
run "$TMP/la12" la uninstall --json --role worker --data-dir "$TMP/lw2" --env-file "$TMP/lw2.env"
check_json "ligus-local uninstall removes the launchd agent" "$TMP/la12" 0 "$RC" unit
if [ -z "$(find "$LA" -type f)" ] && ! ls "$TMP/lc"/loaded.* >/dev/null 2>&1; then pass "no agent left installed or loaded"; else fail "left behind: $(ls -a "$LA" "$TMP/lc")"; fi
if ! grep -Eq '^(load|unload) ' "$TMP/lc/calls"; then pass "never the deprecated launchctl load/unload"; else fail "launchctl load/unload used"; fi

# ligus-local verify finds start --systemd's unit for this data dir without
# --systemd (a FAKE systemctl first on PATH; the real user manager is never
# touched): only a unit ligus-local wrote for this env file counts.
SH="$TMP/sd-home"; mkdir -p "$SH/.config/systemd/user" "$TMP/sdbin" "$TMP/sd"
cat >"$TMP/sdbin/systemctl" <<'FAKE'
#!/bin/sh
# Fake systemctl --user: a unit is active when $FAKE_SD_DIR/active.<unit> exists.
d=${FAKE_SD_DIR:?}
printf '%s\n' "$*" >>"$d/calls"
[ "$1" = --user ] || { echo "fake systemctl: not --user: $*" >&2; exit 64; }
case $2 in
  is-active) for u; do :; done; [ -e "$d/active.$u" ] ;;
  *) echo "fake systemctl: unexpected: $*" >&2; exit 64 ;;
esac
FAKE
chmod +x "$TMP/sdbin/systemctl"
sd() { # ligus-local on "Linux" with the fake systemctl and the temp HOME
  env HOME="$SH" PATH="$TMP/sdbin:$PATH" LIGUS_LOCAL_OS=Linux FAKE_SD_DIR="$TMP/sd" LIGUS_BIN="$LB" "$LOCAL" "$@"
}
la_env "$TMP/sd1.env" "$TMP/sdd" serve sdbox
sd unit --role serve --data-dir "$TMP/sdd" --env-file "$TMP/sd1.env" >"$SH/.config/systemd/user/ligus-serve.service" 2>/dev/null
run "$TMP/sd1" sd verify --json --role serve --data-dir "$TMP/sdd" --env-file "$TMP/sd1.env"
check_json "ligus-local verify, systemd unit installed but inactive" "$TMP/sd1" 1 "$RC" systemd
touch "$TMP/sd/active.ligus-serve"
run "$TMP/sd2" sd status --json --role serve --data-dir "$TMP/sdd" --env-file "$TMP/sd1.env"
check_json "ligus-local verify finds the active systemd unit without --systemd" "$TMP/sd2" 0 "$RC" systemd
if grep -q 'systemd unit ligus-serve active' "$TMP/sd2" && grep -qx -- '--user is-active --quiet ligus-serve' "$TMP/sd/calls"; then
  pass "verify checks the unit with systemctl --user is-active"
else fail "verify with the unit: $(cat "$TMP/sd2" "$TMP/sd/calls")"; fi
la_env "$TMP/sd-other.env" "$TMP/sdo" serve other
run "$TMP/sd3" sd verify --json --role serve --data-dir "$TMP/sdo" --env-file "$TMP/sd-other.env"
check_json "ligus-local verify ignores another data dir's unit" "$TMP/sd3" 1 "$RC" process
if ! grep -q '"id":"systemd"' "$TMP/sd3"; then pass "another env file's unit is not this box's"; else fail "verify took another data dir's unit: $(cat "$TMP/sd3")"; fi
sed -i.bak 's/^Description=.*/Description=Ligus serve (hand-installed)/' "$SH/.config/systemd/user/ligus-serve.service"
run "$TMP/sd4" sd verify --json --role serve --data-dir "$TMP/sdd" --env-file "$TMP/sd1.env"
check_json "ligus-local verify ignores a hand-installed unit" "$TMP/sd4" 1 "$RC" process
if ! grep -q '"id":"systemd"' "$TMP/sd4"; then pass "a unit ligus-local didn't write is left to its owner"; else fail "verify took a hand-installed unit"; fi

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
