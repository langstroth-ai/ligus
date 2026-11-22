# shellcheck shell=bash
# Shared helpers for deploy/kubernetes/ligus-k8s and deploy/local/ligus-local.
# Sourced, never run. Bash 3.2 compatible (macOS /bin/bash).
#
# Conventions every command follows (AGENTS.md, "Exit codes"):
#   0  done (warnings allowed)
#   1  a check or an operation failed; the JSON says which and what to do
#   2  usage error (unknown flag, missing argument)
#   3  a precondition is missing that the operator can fix and rerun
#      (a tool, the relay chart, a Secret the tool can create)
#   4  a human must decide or act (a real workspace secret, exposure,
#      weakening a default); stop and ask
# Progress goes to stderr. With --json, stdout carries exactly one JSON
# object. No command ever prints a secret value.

AC_JSON=0
AC_COLOR=auto
AC_CHECKS=""
AC_NEXT=""
AC_WORST=0

ac_log() { printf '%s\n' "$*" >&2; }

# --color auto|always|never (a script's flag). Auto colours the [FAIL] and
# [WARN] marks only when stderr is a terminal and NO_COLOR is unset (or
# CLICOLOR_FORCE asks for colour anyway). JSON on stdout is never coloured.
ac_set_color() {
  case $1 in
    auto|always|never) AC_COLOR=$1 ;;
    *) ac_usage_error "--color must be auto, always or never" ;;
  esac
}
ac_color_on() {
  case $AC_COLOR in always) return 0 ;; never) return 1 ;; esac
  [ -n "${NO_COLOR:-}" ] && return 1
  [ -n "${CLICOLOR_FORCE:-}" ] && [ "${CLICOLOR_FORCE}" != 0 ] && return 0
  [ -t 2 ]
}

# JSON string escape (backslash, quote, control characters as spaces).
ac_jstr() {
  local s=$1
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  s=$(printf '%s' "$s" | tr '\n\r\t' '   ')
  printf '"%s"' "$s"
}

# ac_check <id> <pass|warn|fail|info|skip> <detail> [fix]
# A fail raises the exit code to 1 unless a higher code was already set.
ac_check() {
  local id=$1 status=$2 detail=$3 fix=${4:-}
  local obj
  obj="{\"id\":$(ac_jstr "$id"),\"status\":$(ac_jstr "$status"),\"detail\":$(ac_jstr "$detail")"
  [ -n "$fix" ] && obj="$obj,\"fix\":$(ac_jstr "$fix")"
  obj="$obj}"
  if [ -z "$AC_CHECKS" ]; then AC_CHECKS=$obj; else AC_CHECKS="$AC_CHECKS,$obj"; fi
  case $status in
    fail) [ "$AC_WORST" -lt 1 ] && AC_WORST=1 ;;
  esac
  local mark tag
  case $status in pass) mark="ok  " ;; warn) mark="WARN" ;; fail) mark="FAIL" ;; skip) mark="skip" ;; *) mark="info" ;; esac
  tag="[$mark]"
  if { [ "$status" = fail ] || [ "$status" = warn ]; } && ac_color_on; then
    # Red for a failure, yellow for a warning; the word stays for no colour.
    if [ "$status" = fail ]; then tag=$(printf '\033[1;31m[%s]\033[0m' "$mark"); else tag=$(printf '\033[33m[%s]\033[0m' "$mark"); fi
  fi
  ac_log "$tag $id: $detail"
  if [ -n "$fix" ] && [ "$status" != pass ]; then ac_log "       -> $fix"; fi
  return 0
}

# Raise the exit code (never lower it).
ac_raise() { [ "$AC_WORST" -lt "$1" ] && AC_WORST=$1; return 0; }

# A next step for the operator (shown last, and in the JSON as "next").
ac_next() {
  if [ -z "$AC_NEXT" ]; then AC_NEXT=$(ac_jstr "$1"); else AC_NEXT="$AC_NEXT,$(ac_jstr "$1")"; fi
  ac_log "next: $1"
}

# ac_finish <tool> <command> [extra json members, already encoded, comma-led]
ac_finish() {
  local tool=$1 cmd=$2 extra=${3:-}
  local ok=true
  [ "$AC_WORST" -ne 0 ] && ok=false
  if [ "$AC_JSON" = 1 ]; then
    printf '{"schema":"ligus.agent-deploy.v1","tool":%s,"command":%s,"ok":%s,"exitCode":%s,"checks":[%s],"next":[%s]%s}\n' \
      "$(ac_jstr "$tool")" "$(ac_jstr "$cmd")" "$ok" "$AC_WORST" "$AC_CHECKS" "$AC_NEXT" "$extra"
  fi
  exit "$AC_WORST"
}

ac_usage_error() {
  ac_log "error: $1"
  ac_log "run with --help for usage"
  if [ "$AC_JSON" = 1 ]; then
    printf '{"schema":"ligus.agent-deploy.v1","tool":%s,"command":"usage","ok":false,"exitCode":2,"checks":[],"next":[],"error":%s}\n' \
      "$(ac_jstr "${AC_TOOL:-ligus}")" "$(ac_jstr "$1")"
  fi
  exit 2
}

ac_have() { command -v "$1" >/dev/null 2>&1; }

# version_ge A B: A >= B for dotted numeric versions (leading v ignored).
ac_version_ge() {
  local a=${1#v} b=${2#v}
  local IFS=.
  # shellcheck disable=SC2206
  local x=($a) y=($b)
  local i
  for i in 0 1 2; do
    local p=${x[$i]:-0} q=${y[$i]:-0}
    p=${p%%[!0-9]*}; q=${q%%[!0-9]*}
    p=${p:-0}; q=${q:-0}
    if [ "$p" -gt "$q" ]; then return 0; fi
    if [ "$p" -lt "$q" ]; then return 1; fi
  done
  return 0
}

# Random strings, generated locally and never echoed.
ac_rand_b64() { openssl rand -base64 "$1" | tr -d '\n'; }
ac_uuid() {
  if [ -r /proc/sys/kernel/random/uuid ]; then cat /proc/sys/kernel/random/uuid
  elif ac_have uuidgen; then uuidgen | tr '[:upper:]' '[:lower:]'
  else openssl rand -hex 16 | sed 's/^\(.\{8\}\)\(.\{4\}\)\(.\{4\}\)\(.\{4\}\)/\1-\2-\3-\4-/'
  fi
}
