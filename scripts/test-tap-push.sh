#!/usr/bin/env bash
# Tests scripts/tap-push.sh against local bare repos standing in for the tap:
# two jobs push to it concurrently (the cask and the formula bump), the
# loser's first push is rejected, its retry succeeds, and nothing is
# committed twice.
#
#   bash scripts/test-tap-push.sh
# check() evals its condition later, so the single quotes (and variables only
# read inside them) are deliberate.
# shellcheck disable=SC2016,SC2034
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
push="$here/tap-push.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# Ignore the caller's git config (signing, hooks, default branch).
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export TAP_PUSH_BACKOFF=0
fails=0

ok() { echo "ok   - $*"; }
fail() { echo "FAIL - $*"; fails=$((fails + 1)); }
check() { if eval "$2"; then ok "$1"; else fail "$1"; fi; }

# The apply commands the release jobs pass: write the wanted file state.
set_cask() { mkdir -p Casks && printf 'cask "ligus-app" do\n  version "%s"\nend\n' "$1" > Casks/ligus-app.rb; }
set_formula() { mkdir -p Formula && printf 'class Ligus < Formula\n  # %s\nend\n' "$1" > Formula/ligus.rb; }
export -f set_cask set_formula

# A fresh bare "tap" with one seed commit; prints its path.
new_tap() {
  local tap="$work/$1.git" seed="$work/$1-seed"
  git init -q --bare -b main "$tap"
  git init -q -b main "$seed"
  (cd "$seed" && set_cask 0.0.9 && set_formula 0.0.9 &&
    git add -A && git -c user.name=seed -c user.email=seed@example.com commit -q -m seed &&
    git remote add origin "$tap" && git push -q origin main)
  echo "$tap"
}
count() { git --git-dir="$1" rev-list --count main; }
subjects() { git --git-dir="$1" log --format=%s main; }

# 1. Concurrent cask and formula bumps: both clone the same tip, the formula
# job pushes first, so the cask job's first push is rejected and retried.
tap=$(new_tap race)
git clone -q "$tap" "$work/cask"
git clone -q "$tap" "$work/formula"
"$push" "$work/formula" "ligus 0.1.0" set_formula 0.1.0 > "$work/formula.log" 2>&1
if "$push" "$work/cask" "ligus-app 0.1.0" set_cask 0.1.0 > "$work/cask.log" 2>&1; then
  ok "the rejected push succeeds on retry"
else
  fail "the rejected push succeeds on retry"; cat "$work/cask.log"
fi
check "the first cask push was rejected" 'grep -q "rejected (attempt 1)" "$work/cask.log"'
check "the retry pushed on attempt 2" 'grep -q "attempt 2)" "$work/cask.log"'
check "exactly one commit per bump" '[ "$(count "$tap")" -eq 3 ]'
subjects_log=$(subjects "$tap")
check "each bump committed once" '[ "$(sort <<<"$subjects_log" | uniq -d)" = "" ] && grep -qx "ligus 0.1.0" <<<"$subjects_log" && grep -qx "ligus-app 0.1.0" <<<"$subjects_log"'
check "both changes are on the tap" '[[ "$(git --git-dir="$tap" show main:Casks/ligus-app.rb)" == *\"0.1.0\"* && "$(git --git-dir="$tap" show main:Formula/ligus.rb)" == *0.1.0* ]]'
check "commits use the bot identity" '[ "$(git --git-dir="$tap" log -1 --format=%an main)" = "github-actions[bot]" ]'

# 2. Rerunning a bump the tap already has commits nothing.
git clone -q "$tap" "$work/rerun"
"$push" "$work/rerun" "ligus-app 0.1.0" set_cask 0.1.0 > "$work/rerun.log" 2>&1
check "a rerun makes no commit" '[ "$(count "$tap")" -eq 3 ] && grep -q "nothing to push" "$work/rerun.log"'

# 3. Two jobs racing with the SAME change (a rerun beside the original): the
# loser's commit is dropped by the rebase, so there is no duplicate.
tap=$(new_tap same)
git clone -q "$tap" "$work/a"
git clone -q "$tap" "$work/b"
# A different commit date than b's, so the two commits differ even when both
# land in the same second (identical commits would trivially dedupe).
GIT_AUTHOR_DATE="2001-01-01T00:00:00Z" GIT_COMMITTER_DATE="2001-01-01T00:00:00Z" \
  "$push" "$work/a" "ligus-app 0.2.0" set_cask 0.2.0 > /dev/null 2>&1
"$push" "$work/b" "ligus-app 0.2.0" set_cask 0.2.0 > "$work/b.log" 2>&1
check "the same change racing lands once" '[ "$(count "$tap")" -eq 2 ] && grep -q "nothing to push" "$work/b.log"'

# 4. A conflicting change on the tap (someone else edited the same file):
# the retry starts from the tap's tip and re-applies the bump.
tap=$(new_tap conflict)
git clone -q "$tap" "$work/c1"
git clone -q "$tap" "$work/c2"
"$push" "$work/c1" "ligus-app 0.3.0" set_cask 0.3.0 > /dev/null 2>&1
"$push" "$work/c2" "ligus-app 0.3.1" set_cask 0.3.1 > "$work/c2.log" 2>&1
check "a conflicting push is re-applied on the tap's tip" '[ "$(count "$tap")" -eq 3 ] && [[ "$(git --git-dir="$tap" show main:Casks/ligus-app.rb)" == *\"0.3.1\"* ]]'

# 5. A truly concurrent pair of pushes, many times: both always land.
tap=$(new_tap parallel)
for i in 1 2 3 4 5; do
  rm -rf "$work/p1" "$work/p2"
  git clone -q "$tap" "$work/p1"
  git clone -q "$tap" "$work/p2"
  "$push" "$work/p1" "ligus 1.$i.0" set_formula "1.$i.0" > "$work/p1.log" 2>&1 &
  p1=$!
  "$push" "$work/p2" "ligus-app 1.$i.0" set_cask "1.$i.0" > "$work/p2.log" 2>&1 &
  p2=$!
  r1=0 r2=0
  wait "$p1" || r1=$?
  wait "$p2" || r2=$?
  if [ "$r1" -ne 0 ] || [ "$r2" -ne 0 ]; then cat "$work/p1.log" "$work/p2.log"; fail "parallel round $i"; fi
done
check "every parallel bump landed exactly once" '[ "$(count "$tap")" -eq 11 ] && [ "$(subjects "$tap" | sort | uniq -d)" = "" ]'

# 6. A tap that keeps rejecting gives up after the attempt limit and fails.
tap=$(new_tap reject)
printf '#!/bin/sh\necho "rejected by hook" >&2\nexit 1\n' > "$tap/hooks/pre-receive"
chmod +x "$tap/hooks/pre-receive"
git clone -q "$tap" "$work/r"
if "$push" "$work/r" "ligus-app 0.4.0" set_cask 0.4.0 > "$work/r.log" 2>&1; then
  fail "gives up after 5 attempts"
else
  check "gives up after 5 attempts" '[ "$(grep -c "rejected (attempt" "$work/r.log")" -eq 4 ] && grep -q "after 5 attempts" "$work/r.log" && [ "$(count "$tap")" -eq 1 ]'
fi

# 7. A failing apply command fails the push without committing.
tap=$(new_tap applyfail)
git clone -q "$tap" "$work/f"
if "$push" "$work/f" "x" false > /dev/null 2>&1; then fail "a failed apply fails"; else ok "a failed apply fails"; fi
check "a failed apply commits nothing" '[ "$(count "$tap")" -eq 1 ]'

if [ "$fails" -ne 0 ]; then
  echo "$fails failed"
  exit 1
fi
echo "all tap-push tests passed"
