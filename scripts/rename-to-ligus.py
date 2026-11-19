#!/usr/bin/env python3
"""The code-wide rename Hive -> Ligus (Ligus is short for ligustica) for 0.1.0.

Run from the repository root on a clean tree:

    python3 scripts/rename-to-ligus.py            # rename paths and contents
    python3 scripts/rename-to-ligus.py --dry-run  # print what would change

What it does, in order:

1. Renames every tracked path with a "hive" component (`git mv`), e.g.
   crates/hive-core -> crates/ligus-core, dashboard_hive.rs ->
   dashboard_ligus.rs, HiveBrand.tsx -> LigusBrand.tsx.
2. Rewrites the contents of every tracked text file with the rules below.
   Before any rule runs, every match of the KEEP list is swapped out for a
   placeholder and swapped back afterwards, so those bytes never change.

It is mechanical. What it can't decide (removing the Hive 1.x compat code,
docs that mention the old name, the apiaryhq redirects) is done by hand
afterwards, in the same pull request.

KEEP: wire and crypto identifiers stay byte-identical, forever. Each entry
is a regex and the reason it is pinned. The golden vectors in
crates/ligus-core/tests/golden/ are the proof: they must pass unchanged.
"""
import argparse
import os
import re
import subprocess
import sys

KEEP = [
    # Domain-separation strings: signing preimages, KDF info and salts.
    (r"hive-envelope-v3", "event envelope signing preimage tag (golden vectors)"),
    (r"hive-device-cert-v1", "device certificate preimage tag (golden vectors)"),
    (r"hive-device-enroll-v1", "device enrollment preimage tag"),
    (r"hive-workspace-key-seal-v1", "workspace key sealing HKDF info"),
    (r"hive-workspace-key-argon2id", "workspace key Argon2id salt"),
    (r"hive-tunnel[/.]1", "relay tunnel protocol label, HKDF salt and WebSocket subprotocol"),
    (r"hive/sync/1", "iroh P2P ALPN"),
    # Code prefixes people paste between devices.
    (r"hivews1", "workspace invite prefix"),
    (r"hivedev1", "device enrollment code prefix"),
    (r"hivepeer1", "old peer code prefix (its parser is removed by hand)"),
    (r"(?<![A-Za-z0-9_])hive_(?=[\"`])", "peer code prefix"),
    (r"hive_AbC123example", "a peer code in a relay pairing test"),
    # UUIDv5 namespaces (16 ASCII bytes each).
    (r"hiveconfiglogses|hiveroomworkspac|hivegithaccount|hivedmroompair01", "UUIDv5 namespaces"),
    # The relay's /v1 surface and its own names (langstroth-relay, separate repo).
    (r"(?i:x-hive-(?:more|next-after|github-token|event-id))", "relay /v1 HTTP headers"),
    (r"hive_relay_requests_[a-z_]+", "relay Prometheus metric names"),
    (r'"hive":\s*\{\s*"v"', "APNs payload key the phone reads"),
    (r"hiveLink", "APNs payload key the phone reads"),
    # Not ours.
    (r"(?i:toolhive)", "Stacklok ToolHive, a third-party product"),
    (r"Casks/hive\.rb|Formula/hive-cli\.rb|maps hive to ligus-app", "the Homebrew tap's formula and cask renames"),
    # Old docs URLs that still redirect.
    (r"upgrade-from-hive", "old docs URL kept as a redirect source"),
    # Bees, not the product.
    (r"box to a hive|a hive holds its brood|Opening the hive|Hive inspection scheduler|\"hive-notes\"",
     "beekeeping prose and the beekeeping demo workspace"),
]

# Never rewritten: the golden vectors are compared byte for byte.
SKIP_DIRS = ("crates/hive-core/tests/golden/", "crates/ligus-core/tests/golden/")
SKIP_FILES = ("scripts/rename-to-ligus.py",)

# The relay's own settings are LANGSTROTH_RELAY_* since relay v0.3.0, which
# refuses LIGUS_RELAY_* except LIGUS_RELAY_URL. These are the relay-side
# names this repo mentions (in docs and in hints that tell an operator what
# to set on the relay). LIGUS_RELAY_URL, LIGUS_RELAY_ACCESS_TOKEN(_FILE),
# LIGUS_RELAY_ISSUER_KEY, LIGUS_RELAY_TOKEN and the ligus-k8s
# LIGUS_RELAY_REPO_URL/REF/CHART are read by Ligus itself and stay.
RELAY_SIDE = (
    "ACCESS_TOKENS ADDR ADMIN_LOGINS APNS_ APNS_ENV APNS_KEY APNS_KEY_FILE APNS_KEY_ID "
    "APNS_TEAM_ID APNS_TOPIC APNS_TOPICS CLIENT_IP_HEADER CONTACT_RECLAIM_HOLD_DAYS DATA_DIR "
    "FRIEND_CAP FRIEND_REQ_RETENTION_DAYS GITHUB_BURST GITHUB_RATE GITHUB_SOCIAL INBOX_MAX_ROWS "
    "INBOX_RETENTION_DAYS INVITE_BURST INVITE_RATE INVITE_REQUIRE_FRIEND MAX_BODY_BYTES "
    "MAX_DEVICE_BLOBS_PER_WS MAX_ENVELOPES MAX_KEYRING_BYTES_PER_WS MAX_KEYRING_PER_WS "
    "MAX_LIST_BYTES MAX_PAIRINGS MAX_ROTATION_BYTES MAX_SSE MAX_SSE_PER_CLIENT MAX_STORE_BYTES "
    "MEMBERSHIP MEMBERSHIP_PUBKEY METRICS_TOKEN PUSH_BURST PUSH_RATE READ_BURST READ_RATE "
    "REQUIRE_DB RETENTION_DAYS TENANTS TOKEN_PUBKEY TOKEN_PUBKEY_NEXT TRUSTED_PROXIES TUNNEL "
    "TUNNEL_HOSTS WORKSPACE_SCOPE WS_WRITER_BURST WS_WRITER_RATE"
).split()
RELAY_SIDE_RE = "|".join(sorted(map(re.escape, RELAY_SIDE), key=len, reverse=True))

# A "hive" that starts a word: not preceded by a letter, or preceded by an
# escaped newline or tab inside a string literal ("\nHive: ..."). Lowercase
# "hive" inside a word is left alone (archive), and so is "HIVE" after a
# capital (ARCHIVE). "Hive" after a lowercase letter is a camelCase part
# (FakeHive, StoreHive) and is renamed.
START = r"(?:(?<![A-Za-z])|(?<=\\n)|(?<=\\t))"

RULES = [
    # The desktop app crate: package Hive, lib hive_app_lib.
    (r'(?m)^name = "Hive"$', 'name = "ligus-desktop"', ("app/Cargo.toml", "Cargo.lock")),
    (r"(?<![A-Za-z])-p Hive\b", "-p ligus-desktop", None),
    (r"\bfor pkg in Hive\b", "for pkg in ligus-desktop", None),
    (r"\bhive_app_lib\b", "ligus_desktop_lib", None),
    # The relay is langstroth-relay; its settings are LANGSTROTH_RELAY_*.
    (r"(?<![A-Za-z])hive-relay\b", "langstroth-relay", None),
    (r"(?<![A-Za-z])hive_relay\b", "langstroth_relay", None),
    (r"\bHIVE_RELAY_", "LANGSTROTH_RELAY_", None),
    (rf"\bLIGUS_RELAY_({RELAY_SIDE_RE})(?![A-Z_])", r"LANGSTROTH_RELAY_\1", None),
    # A word start the generic rules can't see: after a URL escape (%3Ahive)
    # or an ANSI color code ("\x1b[2mhive").
    (r"(?<=%[0-9A-Fa-f][0-9A-Fa-f])hive", "ligus", None),
    (r"(?<=\[2m)hive", "ligus", None),
    # Everything else, case kept.
    (START + r"HIVE", "LIGUS", None),
    (r"(?<![A-Z])" + r"Hive", "Ligus", None),
    (START + r"hive", "ligus", None),
]

PLACEHOLDER = "\x00KEEP{}\x00"


def git(*args):
    return subprocess.check_output(["git", *args], text=True)


def tracked():
    return [p for p in git("ls-files", "-z").split("\0") if p]


def compiled(rules):
    return [(re.compile(p), r, files) for p, r, files in rules]


KEEP_RE = re.compile("|".join(f"(?:{p})" for p, _ in KEEP))
RULES_C = compiled(RULES)


def rewrite(text, path=None):
    kept = []

    def stash(m):
        kept.append(m.group(0))
        return PLACEHOLDER.format(len(kept) - 1)

    out = KEEP_RE.sub(stash, text)
    for rx, repl, files in RULES_C:
        if files is not None and path not in files:
            continue
        out = rx.sub(repl, out)
    return re.sub(r"\x00KEEP(\d+)\x00", lambda m: kept[int(m.group(1))], out)


def new_path(path):
    parts = path.split("/")
    return "/".join(rewrite(p) for p in parts)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()
    if git("status", "--porcelain", "--untracked-files=no").strip() and not args.dry_run:
        sys.exit("the tree has uncommitted changes; commit or stash them first")

    # 1. Paths.
    moves = [(p, new_path(p)) for p in tracked() if new_path(p) != p and p not in SKIP_FILES]
    for old, new in moves:
        print(f"mv {old} -> {new}")
        if not args.dry_run:
            os.makedirs(os.path.dirname(new) or ".", exist_ok=True)
            subprocess.check_call(["git", "mv", old, new])

    # git mv leaves the old directories behind, empty.
    if not args.dry_run:
        for old, _ in moves:
            d = os.path.dirname(old)
            while d and os.path.isdir(d) and not os.listdir(d):
                os.rmdir(d)
                d = os.path.dirname(d)

    # 2. Contents (in a dry run, read each file at its old path).
    changed = 0
    for old in tracked():
        path = old if args.dry_run is False else new_path(old)
        src = old
        if path in SKIP_FILES or path.startswith(SKIP_DIRS):
            continue
        if not os.path.isfile(src) or os.path.islink(src):
            continue
        with open(src, "rb") as f:
            raw = f.read()
        if b"\0" in raw:
            continue  # binary
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError:
            continue
        out = rewrite(text, path)
        if out != text:
            changed += 1
            if args.dry_run:
                print(f"edit {path}")
            else:
                with open(path, "wb") as f:
                    f.write(out.encode("utf-8"))
    print(f"{len(moves)} paths renamed, {changed} files rewritten")


if __name__ == "__main__":
    main()
