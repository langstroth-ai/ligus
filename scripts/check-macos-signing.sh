#!/usr/bin/env bash
# Release checks for the macOS app's native-passkeys signing (macOS only).
#
#   scripts/check-macos-signing.sh profile <embedded.provisionprofile>
#       Before the build: the Developer ID provisioning profile must be for
#       H5H3NNR456.ai.ligus.desktop, team H5H3NNR456, carry the Associated
#       Domains entitlement, and not be expired. An app signed with the
#       associated-domains entitlement and a profile that doesn't match is
#       killed at launch, so a wrong profile fails the release here.
#       --decoded: the file is the profile's plist already (tests).
#
#   scripts/check-macos-signing.sh entitlements <Ligus.app> (--with-profile | --without-profile)
#       After signing: `codesign -d --entitlements -` must show the
#       entitlements app/Ligus.entitlements asks for (with the profile
#       embedded), or no associated-domains entitlement (without one).
#
# Exits 1 with a ::error:: line on the first failed check.
set -euo pipefail

APP_ID="${LIGUS_EXPECTED_APP_ID:-H5H3NNR456.ai.ligus.desktop}"
TEAM="${LIGUS_EXPECTED_TEAM_ID:-H5H3NNR456}"
DOMAIN="${LIGUS_EXPECTED_ASSOCIATED_DOMAIN:-webcredentials:ligus.ai}"
PLISTBUDDY=/usr/libexec/PlistBuddy
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fail() {
  echo "::error::$*" >&2
  exit 1
}

usage() {
  sed -n '2,17p' "$0" >&2
  exit 2
}

pb() { # pb <plist> <key path>: print the value, or nothing if it's absent
  "$PLISTBUDDY" -c "Print $2" "$1" 2>/dev/null || true
}

check_profile() {
  local file="" decoded=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --decoded) decoded=1 ;;
      -*) usage ;;
      *) file="$1" ;;
    esac
    shift
  done
  [ -n "$file" ] && [ -s "$file" ] || fail "provisioning profile missing or empty: ${file:-<none>}"
  local plist="$TMP/profile.plist"
  if [ "$decoded" = 1 ]; then
    cp "$file" "$plist"
  else
    security cms -D -i "$file" -o "$plist" 2>/dev/null || fail "APPLE_PROVISIONING_PROFILE is not a signed provisioning profile (security cms -D failed); is the secret base64 of the .provisionprofile?"
  fi
  plutil -lint -s "$plist" >/dev/null 2>&1 || fail "the provisioning profile does not decode to a property list"

  local app_id team domains expires now
  app_id="$(pb "$plist" ":Entitlements:com.apple.application-identifier")"
  [ "$app_id" = "$APP_ID" ] || fail "provisioning profile app id is '${app_id:-<none>}', expected $APP_ID"
  team="$(pb "$plist" ":TeamIdentifier:0")"
  [ "$team" = "$TEAM" ] || fail "provisioning profile team is '${team:-<none>}', expected $TEAM"
  team="$(pb "$plist" ":Entitlements:com.apple.developer.team-identifier")"
  [ "$team" = "$TEAM" ] || fail "provisioning profile team-identifier entitlement is '${team:-<none>}', expected $TEAM"
  domains="$(pb "$plist" ":Entitlements:com.apple.developer.associated-domains")"
  [ -n "$domains" ] || fail "provisioning profile has no Associated Domains entitlement (enable it for $APP_ID and regenerate the profile)"
  # ISO 8601 in UTC: compares as a string.
  expires="$(plutil -extract ExpirationDate raw -o - "$plist" 2>/dev/null || true)"
  [ -n "$expires" ] || fail "provisioning profile has no ExpirationDate"
  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  [[ "$expires" > "$now" ]] || fail "provisioning profile expired at $expires"
  echo "Provisioning profile OK: $APP_ID, team $TEAM, associated domains, valid until $expires."
}

check_entitlements() {
  local app="" mode=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --with-profile) mode=with ;;
      --without-profile) mode=without ;;
      -*) usage ;;
      *) app="$1" ;;
    esac
    shift
  done
  [ -n "$app" ] && [ -n "$mode" ] || usage
  [ -d "$app" ] || fail "no app bundle at $app"
  local ents="$TMP/entitlements.plist"
  codesign -d --entitlements - --xml "$app" >"$ents" 2>/dev/null || fail "codesign -d --entitlements failed on $app (is it signed?)"
  if [ "$mode" = without ]; then
    if [ -s "$ents" ] && [ -n "$(pb "$ents" ":com.apple.developer.associated-domains")" ]; then
      fail "$app claims associated domains without a provisioning profile: it would be killed at launch"
    fi
    echo "Entitlements OK: no associated-domains entitlement (no provisioning profile)."
    return
  fi
  [ -s "$ents" ] || fail "$app has no entitlements; expected the native-passkeys set"
  local v
  v="$(pb "$ents" ":com.apple.application-identifier")"
  [ "$v" = "$APP_ID" ] || fail "signed app-identifier is '${v:-<none>}', expected $APP_ID"
  v="$(pb "$ents" ":com.apple.developer.team-identifier")"
  [ "$v" = "$TEAM" ] || fail "signed team-identifier is '${v:-<none>}', expected $TEAM"
  v="$(pb "$ents" ":com.apple.developer.associated-domains")"
  grep -qx "[[:space:]]*$DOMAIN" <<<"$v" || fail "signed associated domains lack $DOMAIN: ${v:-<none>}"
  [ -s "$app/Contents/embedded.provisionprofile" ] || fail "$app has no embedded.provisionprofile"
  echo "Entitlements OK: $APP_ID, team $TEAM, $DOMAIN, profile embedded."
}

[ $# -ge 1 ] || usage
cmd="$1"
shift
case "$cmd" in
  profile) check_profile "$@" ;;
  entitlements) check_entitlements "$@" ;;
  *) usage ;;
esac
