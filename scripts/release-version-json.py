#!/usr/bin/env python3
"""Release notes and docs-site/docs/version.json for promote-release.yml.

    scripts/release-version-json.py summary NOTES.md
    scripts/release-version-json.py compare vA vB
    scripts/release-version-json.py update --file docs-site/docs/version.json \
        --tag vX.Y.Z --notes-file docs/release-notes/vX.Y.Z.md

Release notes live in docs/release-notes/vX.Y.Z.md. Their summary, which
shipped desktops show in the "update available" banner (the `notes` field of
version.json), is either a first-line comment

    <!-- summary: One sentence about the release. -->

(invisible on the GitHub release page) or, without one, the first paragraph
of the file after any `#` headings, with Markdown emphasis, code and links
reduced to plain text. It must be one paragraph of at most MAX_SUMMARY
characters ending in a full stop.

summary  prints the summary, or fails (exit 1) with the reason.
compare  prints -1, 0 or 1 as vA sorts before, equal to or after vB
         (numeric X.Y.Z; a -suffix sorts before the plain version).
update   rewrites version.json for the tag ({"tag", "name", "url", "notes"})
         only if the tag is newer than the one in the file, and prints one of
           updated  the file now names the tag
           current  the file already names the tag with the same content
           newer    the file names a newer tag; left alone (never downgrade)
         A file that names the tag with other content is rewritten (updated).
"""
from __future__ import annotations

import argparse
import json
import re
import sys

TAG_RE = re.compile(r"^v(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.]+))?$")
SUMMARY_COMMENT_RE = re.compile(r"^<!--\s*summary:\s*(.*?)\s*-->\s*$")
MAX_SUMMARY = 400
RELEASES_URL = "https://github.com/langstroth-ai/ligus/releases/latest"
WHATS_NEW = "https://docs.langstroth.ai/ligus/reference/whats-new/"


def parse_tag(tag: str):
    m = TAG_RE.match(tag)
    if not m:
        raise ValueError(f"malformed tag {tag!r} (want vX.Y.Z)")
    core = tuple(int(x) for x in m.group(1, 2, 3))
    # A prerelease suffix sorts before the release itself.
    return core, (0, m.group(4)) if m.group(4) else (1, "")


def compare(a: str, b: str) -> int:
    ka, kb = parse_tag(a), parse_tag(b)
    return (ka > kb) - (ka < kb)


def plain(text: str) -> str:
    text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", text)  # links, images
    text = re.sub(r"`([^`]*)`", r"\1", text)
    text = re.sub(r"(\*\*|__)(.+?)\1", r"\2", text)
    text = re.sub(r"(?<![\w*])[*_](\S(?:.*?\S)?)[*_](?![\w*])", r"\1", text)
    return re.sub(r"\s+", " ", text).strip()


def summary(notes: str) -> str:
    lines = notes.splitlines()
    first = next((ln for ln in lines if ln.strip()), "")
    m = SUMMARY_COMMENT_RE.match(first.strip())
    if m:
        text = plain(m.group(1))
    else:
        para: list[str] = []
        for ln in lines:
            s = ln.strip()
            if not para and (not s or s.startswith("#") or s.startswith("<!--")):
                continue
            if not s or s.startswith("#") or s.startswith("```"):
                break
            para.append(s)
        text = plain(" ".join(para))
    if not text:
        raise ValueError("no summary: add a first paragraph or a <!-- summary: ... --> first line")
    if len(text) > MAX_SUMMARY:
        raise ValueError(f"summary is {len(text)} characters (max {MAX_SUMMARY}): shorten the first paragraph or add <!-- summary: ... -->")
    if not text.endswith("."):
        raise ValueError(f"summary must end with a full stop: {text!r}")
    return text


def manifest(tag: str, notes_text: str) -> dict:
    parse_tag(tag)
    return {
        "tag": tag,
        "name": f"Ligus {tag[1:]}",
        "url": RELEASES_URL,
        "notes": f"{summary(notes_text)} See {WHATS_NEW}.",
    }


def update(path: str, tag: str, notes_text: str) -> str:
    want = manifest(tag, notes_text)
    with open(path, encoding="utf-8") as f:
        have = json.load(f)
    order = compare(str(have.get("tag", "")), tag) if TAG_RE.match(str(have.get("tag", ""))) else -1
    if order > 0:
        return "newer"
    if order == 0 and have == want:
        return "current"
    with open(path, "w", encoding="utf-8") as f:
        json.dump(want, f, indent=2, ensure_ascii=False)
        f.write("\n")
    return "updated"


def main() -> int:
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("summary")
    s.add_argument("notes")
    c = sub.add_parser("compare")
    c.add_argument("a")
    c.add_argument("b")
    u = sub.add_parser("update")
    u.add_argument("--file", required=True)
    u.add_argument("--tag", required=True)
    u.add_argument("--notes-file", required=True)
    a = ap.parse_args()
    try:
        if a.cmd == "summary":
            with open(a.notes, encoding="utf-8") as f:
                print(summary(f.read()))
        elif a.cmd == "compare":
            print(compare(a.a, a.b))
        else:
            with open(a.notes_file, encoding="utf-8") as f:
                print(update(a.file, a.tag, f.read()))
    except (ValueError, OSError, json.JSONDecodeError) as e:
        print(f"release-version-json: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
