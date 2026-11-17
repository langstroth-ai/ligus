#!/usr/bin/env bash
# Fails if a shipped binary's normal build graph enables hive-runtime's
# `test-presence` feature (a recovery key derived from the account id that
# answers every passkey prompt). Tests enable it through dev-dependencies
# only; resolver 2 keeps dev features out of `cargo build` of a binary.
#
# Also checks that the test graph does enable it (so this check can't pass
# vacuously, e.g. if the feature is renamed).
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
for pkg in Hive hive-cli; do
  for flags in "" "--no-default-features" "--all-features"; do
    # --all-features would turn on every feature of the *packages selected*;
    # hive-runtime's own features stay off unless a normal edge enables them.
    out=$(cargo tree --locked -p "$pkg" $flags -e features,normal,build -i hive-runtime 2>/dev/null || true)
    if [ -z "$out" ]; then
      echo "check-no-test-presence: cargo tree for $pkg $flags printed nothing" >&2
      fail=1
    elif grep -q 'test-presence' <<<"$out"; then
      echo "check-no-test-presence: $pkg ($flags) enables hive-runtime/test-presence in a normal build:" >&2
      grep 'test-presence' <<<"$out" >&2
      fail=1
    fi
  done
done

# Positive control: the test graph of the app does enable it.
if ! cargo tree --locked -p Hive -e features,normal,dev -i hive-runtime 2>/dev/null | grep -q 'test-presence'; then
  echo "check-no-test-presence: the control failed (the test graph no longer shows test-presence; update this script)" >&2
  fail=1
fi

[ "$fail" -eq 0 ] && echo "check-no-test-presence: ok"
exit "$fail"
