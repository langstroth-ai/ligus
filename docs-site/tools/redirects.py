#!/usr/bin/env python3
"""Redirects for the single docs site, generated from tools/page-map.tsv.

There is one docs site, https://docs.langstroth.ai/, built from docs-site/:

  /ligus/    Using Ligus            (docs-site/docs/ligus/)
  /operate/  Operating Langstroth   (docs-site/docs/operate/)

Before it, two sites served pages at other paths:

  ligus       https://docs.ligus.ai/       the 0.1 user site
  langstroth  https://docs.langstroth.ai/  the 0.1 operator site

page-map.tsv lists every old page and where it lives now. docs.ligus.ai is a
domain alias of the single Netlify site.

  python3 docs-site/tools/redirects.py generate
      Rewrite the generated blocks in mkdocs.yml (mkdocs-redirects stub
      pages at every old path) and netlify.toml (the 301s).

  python3 docs-site/tools/redirects.py check [--refs] [--built]
      Assert every old URL has a working new home. Without --built it checks
      the map and the generated config. --built also checks the built site
      in docs-site/site and runs every old URL of the two old sites (in
      the /page/, /page and /page/index.html forms), plus version.json and
      llms.txt on every host, through an emulation of Netlify's rules (first
      match wins, force, shadowing, host-prefixed rules) until it lands on a
      file: none may 404, every page must land on its mapped target in one
      301, and version.json must be served without a redirect. --refs reads
      the old sites' pages and files from git (needs full history), asserts
      each page is in the map, and with --built emulates the old files too.
"""
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DOCS = os.path.dirname(HERE)  # docs-site/
SRC = os.path.join(DOCS, "docs")
SITE = os.path.join(DOCS, "site")
PRIMARY = "docs.langstroth.ai"
ALIASES = ("docs.ligus.ai",)
HOSTS = {"ligus": "docs.ligus.ai", "langstroth": PRIMARY}
# The commit the old sites were built from (for --refs).
REF_SPLIT = "71e1084c"  # release/v0.1.0 (the 0.1.0 release branch) before the merge: docs-site/{user,operator}
# Files served on the alias hosts as they are, without a redirect: shipped
# desktop builds read https://docs.ligus.ai/version.json for their update
# check.
SERVED_ON_ALIASES = ("/version.json",)
# Files served under a new name: (old path, new path) on docs.langstroth.ai.
RENAMED_FILES = (
    ("/ligus/images/hive-lockup-dark.svg", "/ligus/images/ligus-lockup-dark.svg"),
    ("/ligus/images/hive-lockup-light.svg", "/ligus/images/ligus-lockup-light.svg"),
    # Screenshots replaced by light/dark pairs: the old name serves the light one.
    ("/ligus/images/add-workspace.png", "/ligus/images/desktop-overview-light.png"),
    ("/ligus/images/onboarding-appearance.png", "/ligus/images/desktop-onboarding-appearance-light.png"),
    ("/ligus/images/onboarding-github.png", "/ligus/images/desktop-onboarding-identity-light.png"),
    ("/ligus/images/onboarding-identity.png", "/ligus/images/desktop-onboarding-identity-light.png"),
    ("/ligus/images/onboarding-runtime.png", "/ligus/images/desktop-onboarding-agent-light.png"),
    ("/ligus/images/onboarding-team.png", "/ligus/images/desktop-onboarding-team-light.png"),
    ("/ligus/images/onboarding-welcome.png", "/ligus/images/desktop-onboarding-identity-light.png"),
    ("/ligus/images/onboarding-workspace.png", "/ligus/images/desktop-onboarding-identity-light.png"),
    ("/ligus/images/overview.png", "/ligus/images/desktop-overview-light.png"),
    ("/ligus/images/settings-general.png", "/ligus/images/desktop-settings-profile-light.png"),
    ("/ligus/images/settings-runtimes.png", "/ligus/images/desktop-settings-models-light.png"),
    ("/ligus/images/settings-team-sync.png", "/ligus/images/desktop-settings-profile-light.png"),
    ("/ligus/images/settings-tools-mcp.png", "/ligus/images/desktop-settings-tools-light.png"),
    ("/ligus/images/team-invite.png", "/ligus/images/desktop-invite-light.png"),
)
BEGIN = "# BEGIN generated"
END = "# END generated"


def load_map():
    rows = []
    with open(os.path.join(HERE, "page-map.tsv")) as f:
        for line in f:
            if line.startswith("#") or not line.strip():
                continue
            old, new, sources = line.rstrip("\n").split("\t")
            srcs = tuple(sources.split(","))
            assert all(s in HOSTS for s in srcs), line
            rows.append((old, new, srcs))
    return rows


def url_path(md):
    """MkDocs directory URL for a page: deploy/index.md -> /deploy/."""
    if md in ("index.md", "README.md"):
        return "/"
    for index in ("/index.md", "/README.md"):  # MkDocs treats README.md as an index page
        if md.endswith(index):
            return "/" + md[: -len(index) + 1]
    return "/" + md[:-3] + "/"


def absolute(new):
    return f"https://{PRIMARY}{url_path(new)}"


def rule(frm, to, status=301, force=True):
    out = ["[[redirects]]", f'  from = "{frm}"', f'  to = "{to}"', f"  status = {status}"]
    if force:
        out.append("  force = true")
    return "\n".join(out)


def page_rules(prefix, old, to):
    """The directory URL (Netlify matches it with and without the trailing
    slash) and its index.html."""
    p = url_path(old)
    out = [rule(prefix + p, to)]
    if p != "/":
        out.append(rule(prefix + p + "index.html", to))
    return out


def netlify_rules(rows):
    out = []
    for host in ALIASES:
        pre = f"https://{host}"
        out.append(f"# {host}: a domain alias of this site.")
        for path in SERVED_ON_ALIASES:
            out.append(f"# {path} is served here as it is (200, no redirect) for the desktop update check.")
            out.append(rule(pre + path, path, status=200))
        out.append("# Every old page goes straight to its new page.")
        for old, new, _ in rows:
            out += page_rules(pre, old, absolute(new))
        out.append(f"# Anything else: the same path on {PRIMARY}, where the rules below")
        out.append("# answer it.")
        out.append(rule(pre + "/*", f"https://{PRIMARY}/:splat"))
    out.append(f"# {PRIMARY}: old paths of the older sites. force = true because")
    out.append("# mkdocs-redirects also writes a stub page at each of these paths; on")
    out.append("# Netlify the 301 wins.")
    for old, new, _ in rows:
        if old == "index.md":
            continue  # the home page itself
        out += page_rules("", old, url_path(new))
    out.append("# Files that were renamed, at their old paths (also under /images/).")
    for old, new in RENAMED_FILES:
        out.append(rule(old, new))
        out.append(rule(old.replace("/ligus/images/", "/images/", 1), new))
    out.append("# Old screenshot URLs (/images/...). Not forced: the theme's own files")
    out.append("# under /images/ are served as they are.")
    out.append(rule("/images/*", "/ligus/images/:splat", force=False))
    return "\n\n".join(out)


def mkdocs_maps(rows):
    return "\n".join(f"        {old}: {new}" for old, new, _ in rows if old != "index.md")


def splice(path, name, body):
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
    splice(os.path.join(DOCS, "mkdocs.yml"), "redirect_maps", mkdocs_maps(rows))
    splice(os.path.join(DOCS, "netlify.toml"), "redirects", netlify_rules(rows))
    print(f"generated redirects for {len(rows)} old pages")


# ---------------------------------------------------------------- old sites


def git(*args):
    root = subprocess.check_output(["git", "-C", DOCS, "rev-parse", "--show-toplevel"], text=True).strip()
    return subprocess.check_output(["git", "-C", root, *args], text=True)


def old_sites_from_git():
    """{source: (pages, files)} for the two old sites. pages are page
    paths (.md) and "url:/path/" for each path the old host answered with a
    redirect; files are the other files it served (images, llms.txt,
    version.json, stylesheets)."""
    import tomllib

    out = {}
    for src, d in (("ligus", "user"), ("langstroth", "operator")):
        pre = f"docs-site/{d}/docs/"
        tree = git("ls-tree", "-r", "--name-only", REF_SPLIT, pre).split()
        pages = {f[len(pre):] for f in tree if f.endswith(".md")}
        files = {f[len(pre):] for f in tree if not f.endswith(".md") and not f.endswith("CNAME")}
        toml = tomllib.loads(git("show", f"{REF_SPLIT}:docs-site/{d}/netlify.toml"))
        for r in toml.get("redirects", []):
            u = r["from"]
            if u.startswith("/"):
                pages.add("url:" + (u[: -len("index.html")] if u.endswith("index.html") else u))
        if d == "user":
            cfg = git("show", f"{REF_SPLIT}:docs-site/user/mkdocs.yml")
            pages |= set(re.findall(r"^\s+([\w./-]+\.md): ", cfg, re.M))
        out[src] = (pages, files)
    return out


# ---------------------------------------------------------- Netlify emulator


def norm(path):
    """Netlify matches a rule with and without the trailing slash."""
    return path.rstrip("/") or "/"


def built_file(path):
    """The built file a path serves, or None (pretty URLs: /a and /a/ serve
    a/index.html)."""
    p = path.lstrip("/")
    cands = [p] if p and not p.endswith("/") else []
    cands.append(os.path.join(p, "index.html"))
    for c in cands:
        f = os.path.join(SITE, c)
        if os.path.isfile(f):
            return f
    return None


def match(rule_from, host, path):
    """The splat ("" when none) if `rule_from` matches host+path, else None."""
    if rule_from.startswith("https://"):
        rhost, _, rpath = rule_from[len("https://"):].partition("/")
        if rhost != host:
            return None
        rpath = "/" + rpath
    else:
        rpath = rule_from
    if rpath.endswith("/*"):
        base = rpath[:-2]
        if path == base or path.startswith(base + "/"):
            return path[len(base) + 1:]
        return None
    return "" if norm(rpath) == norm(path) else None


def resolve(rules, host, path, limit=6):
    """Follow the rules from https://host/path like Netlify does. Returns
    (statuses of the rules applied, final host, final path, served file or
    None)."""
    hops = []
    for _ in range(limit):
        hit = None
        for r in rules:
            splat = match(r["from"], host, path)
            if splat is None:
                continue
            if not r.get("force") and built_file(path):
                continue  # shadowed by a file that exists
            hit = (r, splat)
            break
        if not hit:
            return hops, host, path, built_file(path)
        r, splat = hit
        to = r["to"].replace(":splat", splat)
        hops.append(r["status"])
        if r["status"] == 200:
            return hops, host, path, built_file(to)
        if to.startswith("https://"):
            host, _, rest = to[len("https://"):].partition("/")
            path = "/" + rest
        else:
            path = to
    return hops, host, path, None


# -------------------------------------------------------------------- check


def load_toml(path):
    import tomllib  # Python 3.11+

    with open(path, "rb") as f:
        return tomllib.load(f)


def load_mkdocs_maps():
    import yaml  # PyYAML ships with mkdocs

    class Loader(yaml.SafeLoader):
        pass

    Loader.add_multi_constructor("tag:yaml.org,2002:python/name:", lambda l, s, n: None)
    with open(os.path.join(DOCS, "mkdocs.yml")) as f:
        mk = yaml.load(f, Loader=Loader)
    for p in mk["plugins"]:
        if isinstance(p, dict) and "redirects" in p:
            return p["redirects"]["redirect_maps"] or {}
    return {}


def check(refs, built):
    rows = load_map()
    errors = []
    by_old = {o: s for o, _, s in rows}
    by_url = {url_path(o): s for o, _, s in rows}

    old_files = {}
    if refs:
        for src, (pages, files) in old_sites_from_git().items():
            missing = []
            for p in sorted(pages):
                srcs = by_url.get(p[4:]) if p.startswith("url:") else by_old.get(p)
                if srcs is None:
                    missing.append(p)
                elif src not in srcs:
                    missing.append(f"{p} (source {src} not listed)")
            print(f"{src} ({HOSTS[src]}): {len(pages)} old pages and redirected paths, "
                  f"{len(files)} other files, {len(missing)} not in page-map.tsv")
            errors += [f"{src}: old page {m} has no entry in page-map.tsv" for m in missing]
            old_files[src] = files

    rules = load_toml(os.path.join(DOCS, "netlify.toml")).get("redirects", [])
    maps = load_mkdocs_maps()

    for o, n, srcs in rows:
        if not os.path.isfile(os.path.join(SRC, n)):
            errors.append(f"{o}: target docs/{n} does not exist")
        if n.split("/")[0] not in ("ligus", "operate"):
            errors.append(f"{o}: target {n} is outside /ligus/ and /operate/")
        if o != "index.md":
            if os.path.exists(os.path.join(SRC, o)):
                errors.append(f"{o}: a page still exists at the old path")
            if maps.get(o) != n:
                errors.append(f"{o}: mkdocs redirect_maps has {maps.get(o)!r}, want {n!r}")

    allowed_200 = {f"https://{h}{p}" for h in ALIASES for p in SERVED_ON_ALIASES}
    for r in rules:
        if r["status"] not in (200, 301):
            errors.append(f"rule {r['from']} has status {r['status']}")
        if r["status"] == 200 and r["from"] not in allowed_200:
            errors.append(f"unexpected 200 rule {r['from']}")

    checked = 0
    if built and not os.path.isdir(SITE):
        errors.append(f"{SITE} is missing: build the site first")
    elif built:
        for o, n, _ in rows:
            if not built_file(url_path(n)):
                errors.append(f"{n}: not built in site/")
            if o != "index.md":
                stub = os.path.join(SITE, url_path(o).lstrip("/"), "index.html")
                if not os.path.isfile(stub) or "refresh" not in open(stub).read():
                    errors.append(f"{o}: no redirect stub at {stub}")
        # Every old page URL, on every host, in every form.
        for host in (PRIMARY,) + ALIASES:
            for o, n, _ in rows:
                want = "/" if (o == "index.md" and host == PRIMARY) else url_path(n)
                p = url_path(o)
                for form in ([p] if p == "/" else [p, p.rstrip("/"), p + "index.html"]):
                    checked += 1
                    hops, fhost, fpath, f = resolve(rules, host, form)
                    where = f"https://{host}{form}"
                    if f is None:
                        errors.append(f"{where}: 404 after {hops} at https://{fhost}{fpath}")
                    elif fhost != PRIMARY or norm(fpath) != norm(want):
                        errors.append(f"{where}: lands on https://{fhost}{fpath}, want {want}")
                    elif hops != ([] if (host == PRIMARY and want == "/") else [301]):
                        errors.append(f"{where}: rules applied {hops}, want exactly one 301")
            # Files every host serves.
            for path in SERVED_ON_ALIASES + ("/llms.txt",):
                checked += 1
                hops, fhost, fpath, f = resolve(rules, host, path)
                if f is None:
                    errors.append(f"https://{host}{path}: 404")
                elif path in SERVED_ON_ALIASES and fhost != host:
                    errors.append(f"https://{host}{path}: redirected ({hops}); it must be served as it is")
        # Other files of the old sites (images, stylesheets, llms.txt, version.json).
        for src, files in old_files.items():
            for fpath in sorted(files):
                checked += 1
                hops, fhost, final, f = resolve(rules, HOSTS[src], "/" + fpath)
                if f is None:
                    errors.append(f"https://{HOSTS[src]}/{fpath}: 404 after {hops} at https://{fhost}{final}")

    count = lambda s: sum(1 for r in rows if s in r[2])
    print(f"{len(rows)} old pages: {count('ligus')} from docs.ligus.ai, {count('langstroth')} from docs.langstroth.ai")
    print(f"netlify.toml: {len(rules)} redirect rules; mkdocs redirect stubs: {len(maps)}")
    if built:
        print(f"emulated {checked} old URLs and files across {1 + len(ALIASES)} hosts")
    if errors:
        print("\n".join("FAIL " + e for e in errors))
        sys.exit(1)
    print("OK: every old URL on docs.ligus.ai and docs.langstroth.ai reaches its page")


if __name__ == "__main__":
    args = sys.argv[1:]
    if args[:1] == ["generate"]:
        generate()
    elif args[:1] == ["check"]:
        check("--refs" in args, "--built" in args)
    else:
        print(__doc__)
        sys.exit(2)
