#!/usr/bin/env bash
# Fails if a shipped binary's normal build graph enables one of
# hive-runtime's test-only features:
#   - `test-presence`: a recovery key derived from the account id that
#     answers every passkey prompt;
#   - `test-relay`: the in-process mock relay (`hive_runtime::test_relay`),
#     which admits any request, for other clients' integration tests.
# Tests enable them through dev-dependencies only; resolver 2 keeps dev
# features out of `cargo build` of a binary.
#
# Also checks that each feature still exists and shows up when enabled (so
# this check can't pass vacuously, e.g. if a feature is renamed).
set -euo pipefail
cd "$(dirname "$0")/.."

FEATURES=(test-presence test-relay)

fail=0
for pkg in Hive hive-cli; do
  for flags in "" "--no-default-features" "--all-features"; do
    # --all-features would turn on every feature of the *packages selected*;
    # hive-runtime's own features stay off unless a normal edge enables them.
    out=$(cargo tree --locked -p "$pkg" $flags -e features,normal,build -i hive-runtime 2>/dev/null || true)
    if [ -z "$out" ]; then
      echo "check-no-test-presence: cargo tree for $pkg $flags printed nothing" >&2
      fail=1
      continue
    fi
    for feat in "${FEATURES[@]}"; do
      if grep -q "\"$feat\"" <<<"$out"; then
        echo "check-no-test-presence: $pkg ($flags) enables hive-runtime/$feat in a normal build:" >&2
        grep "\"$feat\"" <<<"$out" >&2
        fail=1
      fi
    done
  done
done

# Positive controls: the test graph of the app enables test-presence, and
# each feature shows up in the tree when it is turned on.
if ! cargo tree --locked -p Hive -e features,normal,dev -i hive-runtime 2>/dev/null | grep -q '"test-presence"'; then
  echo "check-no-test-presence: the control failed (the test graph no longer shows test-presence; update this script)" >&2
  fail=1
fi
for feat in "${FEATURES[@]}"; do
  if ! cargo tree --locked -p hive-runtime --features "$feat" -e features -i hive-runtime 2>/dev/null | grep -q "\"$feat\""; then
    echo "check-no-test-presence: the control failed (hive-runtime/$feat no longer shows when enabled; update this script)" >&2
    fail=1
  fi
done

[ "$fail" -eq 0 ] && echo "check-no-test-presence: ok"
exit "$fail"
