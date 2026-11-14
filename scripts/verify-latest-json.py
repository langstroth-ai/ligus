#!/usr/bin/env python3
"""Check a release's latest.json before anyone's updater reads it.

    scripts/verify-latest-json.py --repo owner/repo --release-id ID \
        --tag vX.Y.Z --platforms macos,windows,linux
    scripts/verify-latest-json.py --from-dir DIR --tag vX.Y.Z --platforms ...

tauri-action rebuilds latest.json in every matrix job by reading the copy on
the release, merging its own platform and re-uploading it, so two jobs that
finish together can drop each other's entry, and a rerun replaces assets
(new asset ids). This fails the release if, for any platform that was built:

  - latest.json is missing, or its version is not the tag's,
  - the platform entry is missing,
  - its URL does not name an asset on THIS release, or names the wrong kind
    of bundle, or a bundle without the version in its file name,
  - its signature differs from the .sig asset uploaded next to the bundle.

--from-dir reads latest.json, assets.json ([{id, name}]) and the .sig files
from DIR instead of the GitHub API (used by the tests).
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile

# matrix name -> (latest.json key, accepted updater bundle suffixes)
PLATFORMS = {
    "macos": ("darwin-aarch64", (".app.tar.gz",)),
    "windows": ("windows-x86_64", (".msi", ".exe")),
    "linux": ("linux-x86_64", (".AppImage",)),
}
TAG_RE = re.compile(r"^v(\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?)$")
API_ASSET_RE = re.compile(r"^https://api\.github\.com/repos/[^/]+/[^/]+/releases/assets/(\d+)$")
DOWNLOAD_RE = re.compile(r"^https://github\.com/[^/]+/[^/]+/releases/download/([^/]+)/([^/]+)$")


def check(latest: dict | None, assets: list[dict], sigs: dict[str, str], tag: str,
          platforms: list[str]) -> list[str]:
    m = TAG_RE.match(tag)
    if not m:
        return [f"malformed tag {tag!r}"]
    version = m.group(1)
    if latest is None:
        return ["latest.json is not on the release"]
    errors = []
    if latest.get("version") != version:
        errors.append(f"latest.json version {latest.get('version')!r} != {version!r}")
    by_id = {str(a["id"]): a["name"] for a in assets}
    names = set(by_id.values())
    entries = latest.get("platforms") or {}
    for p in platforms:
        key, suffixes = PLATFORMS[p]
        entry = entries.get(key)
        if not entry:
            errors.append(f"{key}: missing from latest.json")
            continue
        url = entry.get("url", "")
        name = None
        if (am := API_ASSET_RE.match(url)):
            name = by_id.get(am.group(1))
        elif (dm := DOWNLOAD_RE.match(url)) and dm.group(1) == tag:
            name = dm.group(2) if dm.group(2) in names else None
        if name is None:
            errors.append(f"{key}: url {url} is not an asset of this release")
            continue
        if not name.endswith(suffixes):
            errors.append(f"{key}: {name} is not a {'/'.join(suffixes)} bundle")
        if f"_{version}_" not in name:
            errors.append(f"{key}: {name} does not carry version {version}")
        sig = sigs.get(name + ".sig")
        if sig is None:
            errors.append(f"{key}: {name}.sig is not on the release")
        elif sig.strip() != str(entry.get("signature", "")).strip():
            errors.append(f"{key}: signature differs from {name}.sig")
    return errors


def gh(*args: str) -> bytes:
    return subprocess.run(["gh", *args], check=True, capture_output=True).stdout


def fetch(repo: str, release_id: str, out: str) -> None:
    assets = json.loads(gh("api", "--paginate", f"repos/{repo}/releases/{release_id}/assets?per_page=100",
                           "--jq", "[.[] | {id, name}]").decode() or "[]")
    with open(os.path.join(out, "assets.json"), "w") as f:
        json.dump(assets, f)
    for a in assets:
        if a["name"] == "latest.json" or a["name"].endswith(".sig"):
            data = gh("api", "-H", "Accept: application/octet-stream",
                      f"repos/{repo}/releases/assets/{a['id']}")
            with open(os.path.join(out, os.path.basename(a["name"])), "wb") as f:
                f.write(data)


def load_dir(d: str):
    with open(os.path.join(d, "assets.json")) as f:
        assets = json.load(f)
    latest = None
    p = os.path.join(d, "latest.json")
    if os.path.exists(p):
        with open(p) as f:
            latest = json.load(f)
    sigs = {}
    for a in assets:
        n = os.path.basename(a["name"])
        if n.endswith(".sig") and os.path.exists(os.path.join(d, n)):
            with open(os.path.join(d, n)) as f:
                sigs[n] = f.read()
    return latest, assets, sigs


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo")
    ap.add_argument("--release-id")
    ap.add_argument("--from-dir")
    ap.add_argument("--tag", required=True)
    ap.add_argument("--platforms", required=True)
    a = ap.parse_args()
    platforms = [p.strip().lower() for p in a.platforms.split(",") if p.strip()]
    unknown = [p for p in platforms if p not in PLATFORMS]
    if unknown or not platforms:
        print(f"unknown platforms: {unknown or a.platforms!r}", file=sys.stderr)
        return 2
    if a.from_dir:
        latest, assets, sigs = load_dir(a.from_dir)
    else:
        if not (a.repo and a.release_id):
            print("--repo and --release-id are required without --from-dir", file=sys.stderr)
            return 2
        with tempfile.TemporaryDirectory() as d:
            fetch(a.repo, a.release_id, d)
            latest, assets, sigs = load_dir(d)
    errors = check(latest, assets, sigs, a.tag, platforms)
    for e in errors:
        print(f"::error::latest.json: {e}")
    if not errors:
        print(f"latest.json OK for {a.tag}: {', '.join(PLATFORMS[p][0] for p in platforms)}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
