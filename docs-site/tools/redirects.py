#!/usr/bin/env python3
"""Redirects for the docs split, generated from tools/page-map.tsv.

Before 2.0 there was one docs site (docs.apiaryhq.ai, then docs.ligus.ai).
It is now two sites:

  user      docs-site/user      https://docs.ligus.ai/
  operator  docs-site/operator  https://docs.langstroth.ai/

page-map.tsv lists every page of the old site and where it lives now.

  python3 docs-site/tools/redirects.py generate
      Rewrite the generated blocks in user/mkdocs.yml (mkdocs-redirects stub
      pages), user/netlify.toml and operator/netlify.toml.

  python3 docs-site/tools/redirects.py check [--ref GIT_REF ...] [--built]
      Assert every old page has a working new home: a redirect on
      docs.ligus.ai (or the same path still there), a host rule for
      docs.apiaryhq.ai, and a target page that exists. --ref also lists the
      pages (nav entries and files) of the old site at that git ref and
      asserts each is in the map. --built checks the built sites in
      user/site and operator/site too.
"""
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DOCS = os.path.dirname(HERE)
BASE = {"user": "https://docs.ligus.ai/", "operator": "https://docs.langstroth.ai/"}
OLD_HOST = "https://docs.apiaryhq.ai"
BEGIN = "# BEGIN generated"
END = "# END generated"


def load_map():
    rows = []
    with open(os.path.join(HERE, "page-map.tsv")) as f:
        for line in f:
            if line.startswith("#") or not line.strip():
                continue
            old, site, new = line.rstrip("\n").split("\t")
            assert site in BASE, line
            rows.append((old, site, new))
    return rows


def url_path(md):
    """MkDocs directory URL for a page: deploy/index.md -> /deploy/."""
    if md in ("index.md", "README.md"):
        return "/"
    for index in ("/index.md", "/README.md"):  # MkDocs treats README.md as an index page
        if md.endswith(index):
            return "/" + md[: -len(index) + 1]
    return "/" + md[:-3] + "/"


def target(site, new):
    return BASE[site].rstrip("/") + url_path(new)


def rule(frm, to, force, comment=None):
    out = []
    if comment:
        out.append(f"# {comment}")
    out += ["[[redirects]]", f'  from = "{frm}"', f'  to = "{to}"', "  status = 301"]
    if force:
        out.append("  force = true")
    return "\n".join(out)


def page_rules(frm_prefix, old, to, force):
    """Two rules per page: the directory URL (Netlify matches it with and
    without the trailing slash) and its index.html."""
    p = url_path(old)
    rules = [rule(frm_prefix + p, to, force)]
    if p != "/":
        rules.append(rule(frm_prefix + p + "index.html", to, force))
    return rules


def moved(rows):
    return [(o, s, n) for o, s, n in rows if s != "user" or o != n]


def user_rules(rows):
    out = [
        "# docs.apiaryhq.ai (the 1.x docs domain) points at this site as a domain",
        "# alias. Every page of the old site goes straight to its new home; anything",
        "# else falls through to the same path on docs.ligus.ai.",
    ]
    for o, s, n in rows:
        out += page_rules(OLD_HOST, o, target(s, n), True)
    out.append(rule(OLD_HOST + "/*", BASE["user"] + ":splat", True))
    out.append("# Pages that left this site or moved within it. force = true because")
    out.append("# mkdocs-redirects also writes a stub page at each of these paths for")
    out.append("# GitHub Pages; on Netlify the 301 wins.")
    for o, s, n in moved(rows):
        out += page_rules("", o, target(s, n), True)
    return "\n\n".join(out)


def operator_rules(rows):
    out = ["# User-site pages asked for on this host go to docs.ligus.ai."]
    for o, s, n in rows:
        if s == "user":
            out += page_rules("", o, target(s, n), False)
            if o != n:
                out += page_rules("", n, target(s, n), False)
    return "\n\n".join(out)


def mkdocs_maps(rows):
    lines = []
    for o, s, n in moved(rows):
        to = n if s == "user" else target(s, n)
        lines.append(f"        {o}: {to}")
    return "\n".join(lines)


def splice(path, name, body, indent=""):
    with open(path) as f:
        text = f.read()
    pat = re.compile(
        rf"(^[ \t]*{re.escape(BEGIN)} {name}\n).*?(^[ \t]*{re.escape(END)} {name}\n)",
        re.S | re.M,
    )
    assert pat.search(text), f"markers for {name} missing in {path}"
    text = pat.sub(lambda m: m.group(1) + body + "\n" + m.group(2), text)
    with open(path, "w") as f:
        f.write(text)


def generate():
    rows = load_map()
    splice(os.path.join(DOCS, "user/mkdocs.yml"), "redirect_maps", mkdocs_maps(rows))
    splice(os.path.join(DOCS, "user/netlify.toml"), "redirects", user_rules(rows))
    splice(os.path.join(DOCS, "operator/netlify.toml"), "redirects", operator_rules(rows))
    print(f"generated redirects for {len(rows)} old pages ({len(moved(rows))} moved)")


def old_pages_at(ref):
    """Pages of the pre-split site at a git ref: every nav entry in
    mkdocs.yml plus every .md file under docs-site/."""
    root = subprocess.check_output(["git", "-C", DOCS, "rev-parse", "--show-toplevel"], text=True).strip()
    cfg = subprocess.check_output(["git", "-C", root, "show", f"{ref}:mkdocs.yml"], text=True)
    nav = cfg.split("\nnav:", 1)[1]
    pages = set(re.findall(r":\s*([\w./-]+\.md)\s*$", nav, re.M))
    files = subprocess.check_output(["git", "-C", root, "ls-tree", "-r", "--name-only", ref, "docs-site/"], text=True)
    pages |= {f[len("docs-site/"):] for f in files.split() if f.endswith(".md")}
    return pages


def load_toml(path):
    import tomllib  # Python 3.11+

    with open(path, "rb") as f:
        return tomllib.load(f)


def check(refs, built):
    rows = load_map()
    errors = []
    olds = {o for o, _, _ in rows}
    for ref in refs:
        pages = old_pages_at(ref)
        missing = sorted(pages - olds)
        print(f"{ref}: {len(pages)} old pages, {len(missing)} not in page-map.tsv")
        errors += [f"{ref}: old page {p} has no entry in page-map.tsv" for p in missing]

    user = load_toml(os.path.join(DOCS, "user/netlify.toml"))
    operator = load_toml(os.path.join(DOCS, "operator/netlify.toml"))
    ur = {(r["from"]): r for r in user.get("redirects", [])}
    orr = {(r["from"]): r for r in operator.get("redirects", [])}
    import yaml  # PyYAML ships with mkdocs

    class Loader(yaml.SafeLoader):
        pass

    Loader.add_multi_constructor("tag:yaml.org,2002:python/name:", lambda l, s, n: None)
    mk = yaml.load(open(os.path.join(DOCS, "user/mkdocs.yml")), Loader=Loader)
    maps = {}
    for p in mk["plugins"]:
        if isinstance(p, dict) and "redirects" in p:
            maps = p["redirects"]["redirect_maps"] or {}

    for o, s, n in rows:
        dest = target(s, n)
        if not os.path.isfile(os.path.join(DOCS, s, "docs", n)):
            errors.append(f"{o}: target {s}/docs/{n} does not exist")
        for frm in (OLD_HOST + url_path(o),) + ((OLD_HOST + url_path(o) + "index.html",) if url_path(o) != "/" else ()):
            r = ur.get(frm)
            if not r or r["to"] != dest or r["status"] != 301 or not r.get("force"):
                errors.append(f"{o}: no forced 301 {frm} -> {dest} in user/netlify.toml")
        if s == "user" and o == n:
            if o != "index.md" and os.path.exists(os.path.join(DOCS, "operator/docs", n)):
                errors.append(f"{o}: present in both sites")
            continue
        for frm in (url_path(o), url_path(o) + "index.html"):
            r = ur.get(frm)
            if not r or r["to"] != dest or r["status"] != 301 or not r.get("force"):
                errors.append(f"{o}: no forced 301 {frm} -> {dest} on docs.ligus.ai")
        want = n if s == "user" else dest
        if maps.get(o) != want:
            errors.append(f"{o}: mkdocs redirect_maps has {maps.get(o)!r}, want {want!r}")
        if os.path.exists(os.path.join(DOCS, "user/docs", o)):
            errors.append(f"{o}: moved, but a page still exists at the old path on the user site")
        if built:
            stub = os.path.join(DOCS, "user/site", url_path(o).lstrip("/"), "index.html")
            if not os.path.isfile(stub) or "refresh" not in open(stub).read():
                errors.append(f"{o}: built user site has no redirect stub at {stub}")
    catch = ur.get(OLD_HOST + "/*")
    if not catch or catch["to"] != BASE["user"] + ":splat" or not catch.get("force"):
        errors.append("no docs.apiaryhq.ai catch-all rule")
    for o, s, n in rows:
        if s == "user":
            r = orr.get(url_path(o))
            if not r or r["to"] != target(s, n):
                errors.append(f"{o}: docs.langstroth.ai has no rule to {target(s, n)}")
    if built:
        for s in BASE:
            for o, ss, n in rows:
                if ss == s and not os.path.isfile(os.path.join(DOCS, s, "site", url_path(n).lstrip("/"), "index.html")):
                    errors.append(f"{n}: not built in {s}/site")

    n_moved = len(moved(rows))
    print(f"{len(rows)} old pages: {sum(1 for r in rows if r[1] == 'user')} on docs.ligus.ai "
          f"({sum(1 for o, s, n in rows if s == 'user' and o != n)} at a new path), "
          f"{sum(1 for r in rows if r[1] == 'operator')} on docs.langstroth.ai; {n_moved} moved")
    print(f"user/netlify.toml: {len(ur)} redirect rules; operator/netlify.toml: {len(orr)}")
    if errors:
        print("\n".join("FAIL " + e for e in errors))
        sys.exit(1)
    print("OK: every old page has a 301 or still lives at its path, and every docs.apiaryhq.ai path is mapped")


if __name__ == "__main__":
    args = sys.argv[1:]
    if args[:1] == ["generate"]:
        generate()
    elif args[:1] == ["check"]:
        refs = [args[i + 1] for i, a in enumerate(args) if a == "--ref"]
        check(refs, "--built" in args)
    else:
        print(__doc__)
        sys.exit(2)
