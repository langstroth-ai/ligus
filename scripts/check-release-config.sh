#!/usr/bin/env bash
# Fail a release whose updater or version config isn't ready.
#
#   scripts/check-release-config.sh <tag> [path/to/tauri.conf.json]
#
# - The updater pubkey must be set and must not be the placeholder
#   (REPLACE_WITH_LIGUS_UPDATER_PUBKEY). Installs verify every update against
#   it, so shipping the placeholder would strand them.
# - The updater endpoint must be this repo's latest.json.
# - The tag must be a plain release tag, v<version>, matching the app version
#   (for example v0.1.0 for version 0.1.0), and every ligus crate in
#   Cargo.lock and web/package.json must carry that same version.
#   LIGUS_RELEASE_ROOT (default: the repo) is where those two files live.
set -euo pipefail

tag="${1:?usage: check-release-config.sh <tag> [tauri.conf.json]}"
conf="${2:-app/tauri.conf.json}"
root="${LIGUS_RELEASE_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"

python3 -I - "$tag" "$conf" "$root" <<'PY'
import json, os, re, sys
tag, conf, root = sys.argv[1], sys.argv[2], sys.argv[3]
c = json.load(open(conf))
errs = []
up = c.get("plugins", {}).get("updater", {})
key = (up.get("pubkey") or "").strip()
if not key or "REPLACE_WITH" in key:
    errs.append("the updater pubkey in %s is unset or still the placeholder; put the Ligus updater public key there" % conf)
want = "https://github.com/langstroth-ai/ligus/releases/latest/download/latest.json"
if up.get("endpoints") != [want]:
    errs.append("the updater endpoint must be %s" % want)
m = re.fullmatch(r"v(\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?)", tag)
if not m:
    errs.append("tag %r is not a plain release tag like v0.1.0" % tag)
elif m.group(1) != c.get("version"):
    errs.append("tag %s does not match the app version %s" % (tag, c.get("version")))
else:
    want_v = m.group(1)
    lock = open(os.path.join(root, "Cargo.lock")).read()
    crates = re.findall(r'\[\[package\]\]\nname = "(ligus-[^"]+)"\nversion = "([^"]+)"', lock)
    if not crates:
        errs.append("no ligus crates found in %s/Cargo.lock" % root)
    for name, v in crates:
        if v != want_v:
            errs.append("crate %s is %s in Cargo.lock, not %s" % (name, v, want_v))
    web = json.load(open(os.path.join(root, "web/package.json"))).get("version")
    if web != want_v:
        errs.append("web/package.json version is %s, not %s" % (web, want_v))
for e in errs:
    print("::error::" + e)
sys.exit(1 if errs else 0)
PY
