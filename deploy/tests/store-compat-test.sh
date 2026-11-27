#!/usr/bin/env bash
# shellcheck disable=SC2015  # pass never fails
# Local storage across versions (workspace model v3, invariant V1): a data dir
# the released 0.4.1 CLI wrote is migrated up to user_version 2 by this build,
# keeps its chats, and the 0.4.1 CLI then opens it read-only: every command
# stops with exit 3 and the newer-data message, and the database is not
# changed.
#
#   bash deploy/tests/store-compat-test.sh <path to this build's ligus>
#
# The 0.4.1 binary is LIGUS_OLD_BIN, or the release asset for this platform
# fetched with gh (checked against its .sha256). Everything runs in a
# throwaway data dir and HOME with the file secret backend, so nothing
# touches the person's own data or keychain.
set -euo pipefail

NEW=${1:?usage: store-compat-test.sh <path to this build ligus>}
OLD_TAG=v0.4.1
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
fails=0
pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; fails=$((fails+1)); }

OLD=${LIGUS_OLD_BIN:-}
if [ -z "$OLD" ]; then
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) asset=ligus-aarch64-apple-darwin ;;
    Darwin-x86_64) asset=ligus-x86_64-apple-darwin ;;
    Linux-aarch64) asset=ligus-aarch64-unknown-linux-musl ;;
    Linux-x86_64) asset=ligus-x86_64-unknown-linux-musl ;;
    *) echo "no $OLD_TAG binary for $(uname -s)-$(uname -m); set LIGUS_OLD_BIN" >&2; exit 2 ;;
  esac
  mkdir -p "$TMP/old"
  gh release download "$OLD_TAG" -R langstroth-ai/ligus -p "$asset" -p "$asset.sha256" -D "$TMP/old"
  want=$(cut -d' ' -f1 <"$TMP/old/$asset.sha256")
  if command -v sha256sum >/dev/null 2>&1; then got=$(sha256sum "$TMP/old/$asset" | cut -d' ' -f1); else got=$(shasum -a 256 "$TMP/old/$asset" | cut -d' ' -f1); fi
  [ "$want" = "$got" ] || { echo "checksum mismatch for $asset" >&2; exit 2; }
  OLD="$TMP/old/$asset"
  chmod +x "$OLD"
fi

DATA="$TMP/data"
mkdir -p "$DATA" "$TMP/home"
run() { # bin args...  (sets RC, OUT, ERR)
  local bin=$1; shift
  set +e
  env -i PATH="/usr/bin:/bin" HOME="$TMP/home" LIGUS_DATA_DIR="$DATA" LIGUS_SECRET_BACKEND=file LIGUS_NO_UPDATE_CHECK=1 \
    "$bin" "$@" >"$TMP/out" 2>"$TMP/err"
  RC=$?
  set -e
  OUT=$(cat "$TMP/out"); ERR=$(cat "$TMP/err")
}
version() { python3 -I -c 'import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute("PRAGMA user_version").fetchone()[0])' "$DATA/ligus.db"; }

# 1. The 0.4.1 CLI writes a chat with a message.
run "$OLD" --version
case "$OUT" in *0.4.1*) pass "old binary is 0.4.1" ;; *) fail "old binary is not 0.4.1: $OUT"; exit 1 ;; esac
run "$OLD" new compat
CHAT=$(printf '%s\n' "$OUT" | head -1)
run "$OLD" send "$CHAT" "written by 0.4.1"
[ "$RC" = 0 ] && pass "0.4.1 writes a chat" || fail "0.4.1 send exit $RC: $ERR"
[ "$(version)" = 1 ] && pass "0.4.1 leaves user_version 1" || fail "0.4.1 left user_version $(version)"

# 2. This build migrates it up and keeps the chat.
run "$NEW" tail "$CHAT"
[ "$RC" = 0 ] && pass "this build opens it" || fail "this build: exit $RC: $ERR"
case "$OUT" in *"written by 0.4.1"*) pass "the chat survives the migration" ;; *) fail "the chat is gone: $OUT" ;; esac
[ "$(version)" = 2 ] && pass "migrated to user_version 2" || fail "user_version $(version) after this build"
if ls "$DATA"/ligus.db.bak-log-v2-* >/dev/null 2>&1; then pass "backed up first"; else fail "no backup"; fi
run "$NEW" send "$CHAT" "written by this build"
[ "$RC" = 0 ] && pass "this build writes" || fail "this build send exit $RC: $ERR"

# 3. The 0.4.1 CLI opens it read-only: exit 3, the message, no change.
before=$(python3 -I -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$DATA/ligus.db")
for cmd in "status --json" "chats" "tail $CHAT"; do
  # shellcheck disable=SC2086
  run "$OLD" $cmd
  if [ "$RC" = 3 ] && printf '%s%s' "$OUT" "$ERR" | grep -q "written by a newer Ligus"; then pass "0.4.1 $cmd: read-only, exit 3"; else fail "0.4.1 $cmd: exit $RC: $ERR"; fi
done
after=$(python3 -I -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$DATA/ligus.db")
[ "$before" = "$after" ] && pass "0.4.1 changed nothing" || fail "0.4.1 changed the database"
[ "$(version)" = 2 ] && pass "still user_version 2" || fail "user_version $(version)"

[ "$fails" = 0 ] || { echo "$fails failed"; exit 1; }
echo "all passed"
