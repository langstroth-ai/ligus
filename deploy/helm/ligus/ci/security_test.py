#!/usr/bin/env python3
"""Security assertions on the rendered ligus chart.

Run from anywhere: python3 -I deploy/helm/ligus/ci/security_test.py
Needs helm and PyYAML. Renders the chart with several value sets and checks
properties that `helm lint` cannot see.
"""
import os
import subprocess
import sys

import yaml

CHART = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
KUBE = "1.30.0"
BASE = ["--set", "relayUrl=http://relay.example:8443", "--set", "workspace.existingSecret=ws",
        "--set", "networkPolicy.relayNamespace=ligus-relay"]


def render(*args, ok=True):
    cmd = ["helm", "template", "t", CHART, "--kube-version", KUBE, *BASE, *args]
    p = subprocess.run(cmd, capture_output=True, text=True)
    if ok and p.returncode != 0:
        raise AssertionError("render failed: %s\n%s" % (" ".join(args), p.stderr))
    if not ok:
        return p
    return [d for d in yaml.safe_load_all(p.stdout) if d]


def policies(docs):
    return [d for d in docs if d["kind"] == "NetworkPolicy"]


def selector_matches(sel, labels):
    for k, v in (sel.get("matchLabels") or {}).items():
        if labels.get(k) != v:
            return False
    for e in sel.get("matchExpressions") or []:
        val = labels.get(e["key"])
        op = e["operator"]
        if op == "In" and val not in e["values"]:
            return False
        if op == "NotIn" and val in e["values"]:
            return False
        if op == "Exists" and val is None:
            return False
        if op == "DoesNotExist" and val is not None:
            return False
    return True


def pod_templates(docs):
    for d in docs:
        if d["kind"] in ("Deployment", "StatefulSet", "Job"):
            yield d["kind"], d["metadata"]["name"], d["spec"]["template"]
        elif d["kind"] == "Pod":
            yield "Pod", d["metadata"]["name"], d
        elif d["kind"] == "ConfigMap" and "job.yaml" in (d.get("data") or {}):
            job = yaml.safe_load(d["data"]["job.yaml"])
            yield "JobTemplate", d["metadata"]["name"], job["spec"]["template"]


FAILS = []


def check(name, cond, detail=""):
    print(("ok   " if cond else "FAIL ") + name + (("  -- " + detail) if detail and not cond else ""))
    if not cond:
        FAILS.append(name)


# --- DNS egress must go to the cluster resolver only ------------------------

def test_dns_egress_is_scoped():
    docs = render("--set", "networkPolicy.allowInternetHttps=true", "--set", "networkPolicy.namespaceDefaultDeny=true",
                  "--set", "mcp.servers[0].name=linear",
                  "--set", "mcp.servers[0].image=registry.example/mcp@sha256:" + "0" * 64)
    for np in policies(docs):
        for rule in np["spec"].get("egress") or []:
            ports = [p.get("port") for p in rule.get("ports") or []]
            if 53 in ports:
                to = rule.get("to") or []
                scoped = bool(to) and all(
                    peer.get("namespaceSelector") and peer.get("podSelector") for peer in to)
                check("DNS egress of %s is limited to the cluster resolver" % np["metadata"]["name"],
                      scoped, "port 53 open to any address: a direct exfiltration channel")


# --- Egress is closed by default; HTTPS never reaches cloud metadata --------

def test_egress_closed_and_metadata_blocked():
    docs = render("--set", "networkPolicy.allowInternetHttps=true",
                  "--set", "mcp.servers[0].name=linear",
                  "--set", "mcp.servers[0].image=registry.example/mcp@sha256:" + "0" * 64)
    for np in policies(docs):
        types = np["spec"].get("policyTypes") or []
        name = np["metadata"]["name"]
        if name.endswith("-controller") or name.startswith("mcp-"):
            check("%s restricts egress by default" % name, "Egress" in types)
        for rule in np["spec"].get("egress") or []:
            for peer in rule.get("to") or []:
                block = peer.get("ipBlock")
                if block and block.get("cidr") == "0.0.0.0/0":
                    exc = block.get("except") or []
                    check("%s internet egress excludes cloud metadata" % name, "169.254.0.0/16" in exc)
                    check("%s internet egress excludes private ranges" % name, "10.0.0.0/8" in exc)
    p = render("--set", "networkPolicy.relayNamespace=", ok=False)
    check("restrictEgress with no path to the relay is refused", p.returncode != 0)
    docs = render("--set", "networkPolicy.namespaceDefaultDeny=true")
    deny = [n for n in policies(docs) if n["metadata"]["name"].endswith("-default-deny")]
    check("namespaceDefaultDeny renders an empty-selector deny-all",
          len(deny) == 1 and deny[0]["spec"]["podSelector"] == {} and not deny[0]["spec"].get("egress"))


# --- Only Ligus pods reach serve ------------------------------------------

def test_serve_ingress_excludes_foreign_pods():
    docs = render("--set", "mcp.servers[0].name=linear",
                  "--set", "mcp.servers[0].image=registry.example/mcp@sha256:" + "0" * 64,
                  "--set", "mcp.servers[0].allowUnpinned=true")
    ctrl = [p for p in policies(docs) if p["metadata"]["name"].endswith("-controller")][0]
    peers = [peer for rule in ctrl["spec"]["ingress"] for peer in rule.get("from") or []]

    def admitted(labels):
        return any("podSelector" in peer and "namespaceSelector" not in peer
                   and selector_matches(peer["podSelector"], labels) for peer in peers)

    mcp = [t for k, n, t in pod_templates(docs) if n.startswith("mcp-")][0]
    check("MCP server pods cannot reach serve:8787", not admitted(mcp["metadata"]["labels"]),
          "a third-party MCP image in the namespace can call the serve API")
    check("an unlabelled pod in the namespace cannot reach serve:8787", not admitted({"app": "other"}))
    test_pod = [t for k, n, t in pod_templates(docs) if k == "Pod" and n.endswith("-test-sync")][0]
    check("the helm test pod still reaches serve:8787", admitted(test_pod["metadata"]["labels"]))


# --- The cut executor/PR-test template and otel placeholders stay gone ----

def test_removed_values_refuse():
    for v in ("sandbox.enabled=true", "otel.endpoint=http://collector:4317"):
        p = render("--set", v, ok=False)
        check("%s is refused (unsupported in 2.0)" % v.split("=")[0], p.returncode != 0)
    docs = render()
    check("no executor Job template renders",
          not any(d["kind"] == "ConfigMap" and "job.yaml" in (d.get("data") or {}) for d in docs))


# --- Ligus's own secrets are files, never environment variables -----------

def test_secrets_are_files():
    docs = render("--set", "worker.enabled=true", "--set", "worker.instances[0].name=w1",
                  "--set", "serve.workspaceSecrets.existingSecret=ws",
                  "--set", "serve.ntfy.url=https://ntfy.example", "--set", "serve.ntfy.topic=t",
                  "--set", "serve.ntfy.tokenSecret.name=ntfy", "--set", "remote.enabled=true",
                  "--set", "remote.relayUrl=https://relay.example", "--set", "remote.regTokenSecret.name=reg",
                  "--set", "workspace.relayAccessToken=true",
                  "--set", "serve.agentEnv[0].name=ANTHROPIC_API_KEY", "--set", "serve.agentEnv[0].secretName=keys",
                  "--set", "serve.agentEnv[0].secretKey=anthropic")
    for kind, name, tpl in pod_templates(docs):
        if (tpl["metadata"].get("labels") or {}).get("app.kubernetes.io/name") != "ligus":
            continue
        spec = tpl["spec"]
        for c in (spec.get("initContainers") or []) + spec["containers"]:
            check("%s %s/%s has no envFrom Secret" % (kind, name, c["name"]),
                  not any("secretRef" in e for e in c.get("envFrom") or []))
            for e in c.get("env") or []:
                ref = (e.get("valueFrom") or {}).get("secretKeyRef")
                if ref:
                    check("%s %s/%s: only the room and allow-listed agent keys come from a Secret in env (%s)"
                          % (kind, name, c["name"], e["name"]), e["name"] in ("LIGUS_WORKSPACE", "ANTHROPIC_API_KEY"))
        for v in spec.get("volumes") or []:
            if "secret" in v:
                check("%s %s secret volume %s is 0400" % (kind, name, v["name"]), v["secret"].get("defaultMode") == 0o400)
    p = render("--set", "serve.agentEnv[0].name=LIGUS_WORKSPACE_KEY", "--set", "serve.agentEnv[0].secretName=a",
               "--set", "serve.agentEnv[0].secretKey=b", ok=False)
    check("agentEnv refuses names outside the allow-list", p.returncode != 0)
    p = render("--set", "serve.secretEnv.existingSecret=all", ok=False)
    check("the removed serve.secretEnv (envFrom) is refused", p.returncode != 0)


# --- No lookup-generated Secrets (helm template / GitOps safe) -------------

def test_no_generated_secrets():
    docs = render()
    check("the chart renders no Secret", not any(d["kind"] == "Secret" for d in docs))
    check("workspace.generate is refused", render("--set", "workspace.generate=true", ok=False).returncode != 0)
    p = subprocess.run(["helm", "template", "t", CHART, "--kube-version", KUBE, "--set", "relayUrl=x",
                        "--set", "networkPolicy.relayNamespace=r"], capture_output=True, text=True)
    check("no workspace Secret is refused", p.returncode != 0)
    a = subprocess.run(["helm", "template", "t", CHART, "--kube-version", KUBE, *BASE], capture_output=True, text=True).stdout
    b = subprocess.run(["helm", "template", "t", CHART, "--kube-version", KUBE, *BASE], capture_output=True, text=True).stdout
    check("two renders are identical", a == b and a != "")


# --- Test pods never hold the workspace key --------------------------------

def test_test_pods_hold_no_workspace_key():
    docs = render("--set", "workspace.relayAccessToken=true")
    tests = [d for d in docs if d["kind"] == "Pod"
             and (d["metadata"].get("annotations") or {}).get("helm.sh/hook") == "test"
             and d["metadata"]["labels"].get("app.kubernetes.io/name") == "ligus"]
    check("two ligus test pods", len(tests) == 2, str([t["metadata"]["name"] for t in tests]))
    for t in tests:
        items = [i for v in t["spec"].get("volumes") or [] if "secret" in v for i in v["secret"].get("items") or [{"key": "*"}]]
        check("%s mounts no workspace key" % t["metadata"]["name"], all(i["key"] != "key" and i["key"] != "*" for i in items), str(items))
        envs = [e for c in t["spec"]["containers"] for e in c.get("env") or []]
        check("%s has no Secret in env" % t["metadata"]["name"], not any("valueFrom" in e and "secretKeyRef" in e["valueFrom"] for e in envs))


# --- One identity, one writer ---------------------------------------------

def test_single_writer_volumes():
    docs = render("--set", "worker.enabled=true", "--set", "worker.instances[0].name=a",
                  "--set", "worker.instances[1].name=b")
    sets = [d for d in docs if d["kind"] == "StatefulSet" and d["metadata"]["labels"].get("app.kubernetes.io/name") == "ligus"]
    check("serve plus one StatefulSet per worker instance", len(sets) == 3, str([d["metadata"]["name"] for d in sets]))
    for d in sets:
        n = d["metadata"]["name"]
        check("%s has exactly one replica" % n, d["spec"]["replicas"] == 1)
        claims = {c["metadata"]["name"]: c["spec"]["accessModes"] for c in d["spec"]["volumeClaimTemplates"]}
        check("%s data volume is ReadWriteOncePod" % n, claims.get("data") == ["ReadWriteOncePod"])
        check("%s HOME is its own volume" % n, "home" in claims)
        mounts = d["spec"]["template"]["spec"]["containers"][0]["volumeMounts"]
        check("%s never mounts the data volume twice (no HOME subPath)" % n,
              sum(1 for m in mounts if m["name"] == "data") == 1)
    for bad in (["--set", "worker.enabled=true", "--set", "worker.replicas=3"],
                ["--set", "worker.enabled=true"],
                ["--set", "serve.persistence.accessMode=ReadWriteMany"]):
        check("refused: %s" % " ".join(bad[1::2]), render(*bad, ok=False).returncode != 0)


# --- Third-party MCP images are pinned by digest ---------------------------

def test_mcp_images_pinned():
    p = render("--set", "mcp.servers[0].name=linear",
               "--set", "mcp.servers[0].image=registry.example/mcp-linear:latest", ok=False)
    check("an MCP server image given by tag is refused", p.returncode != 0,
          "a moving tag lets whoever controls the registry swap code that holds MCP secrets")
    docs = render("--set", "mcp.servers[0].name=linear",
                  "--set", "mcp.servers[0].image=registry.example/mcp-linear@sha256:" + "0" * 64)
    check("an MCP server image pinned by digest renders", any(d["kind"] == "Deployment" for d in docs))
    docs = render("--set", "mcp.servers[0].name=linear",
                  "--set", "mcp.servers[0].image=registry.example/mcp-linear:1.2",
                  "--set", "mcp.servers[0].allowUnpinned=true")
    check("allowUnpinned opts out explicitly", any(d["kind"] == "Deployment" for d in docs))


# --- Overriding internetExcept never reopens cloud metadata ----------------

def test_metadata_stays_blocked_when_internet_except_is_overridden():
    # A list value replaces the default list: someone narrowing the private
    # ranges must not silently drop the metadata exclusions with them.
    docs = render("--set", "networkPolicy.allowInternetHttps=true",
                  "--set", "networkPolicy.internetExcept={10.0.0.0/8}",
                  "--set", "mcp.servers[0].name=linear",
                  "--set", "mcp.servers[0].image=registry.example/mcp@sha256:" + "0" * 64)
    seen = 0
    for np in policies(docs):
        for rule in np["spec"].get("egress") or []:
            for peer in rule.get("to") or []:
                block = peer.get("ipBlock")
                if block and block.get("cidr") == "0.0.0.0/0":
                    seen += 1
                    exc = block.get("except") or []
                    check("%s keeps 169.254.0.0/16 out with internetExcept overridden" % np["metadata"]["name"],
                          "169.254.0.0/16" in exc, str(exc))
                    check("%s keeps 100.100.100.200/32 out with internetExcept overridden" % np["metadata"]["name"],
                          "100.100.100.200/32" in exc, str(exc))
    check("internet egress rules rendered", seen >= 2, str(seen))
    # Free-form egress (extraEgress, mcp.servers[].egress) that opens every
    # address must exclude metadata too, IPv4 and IPv6 (AWS fd00:ec2::254).
    for args, what in (
        (["--set", "networkPolicy.extraEgress[0].to[0].ipBlock.cidr=0.0.0.0/0"], "extraEgress 0.0.0.0/0"),
        (["--set", "networkPolicy.extraEgress[0].to[0].ipBlock.cidr=::/0"], "extraEgress ::/0"),
        (["--set", "mcp.servers[0].name=x", "--set", "mcp.servers[0].image=r/m@sha256:" + "0" * 64,
          "--set", "mcp.servers[0].egress[0].to[0].ipBlock.cidr=::/0"], "mcp egress ::/0"),
    ):
        p = render(*args, ok=False)
        check("%s without a metadata exclusion is refused" % what, p.returncode != 0)
    docs = render("--set", "networkPolicy.extraEgress[0].to[0].ipBlock.cidr=::/0",
                  "--set", "networkPolicy.extraEgress[0].to[0].ipBlock.except={fd00:ec2::254/128}")
    check("extraEgress ::/0 excluding fd00:ec2::254 renders", any(d["kind"] == "NetworkPolicy" for d in docs))


TESTS = [test_no_generated_secrets, test_test_pods_hold_no_workspace_key, test_dns_egress_is_scoped, test_egress_closed_and_metadata_blocked, test_serve_ingress_excludes_foreign_pods,
         test_removed_values_refuse, test_secrets_are_files, test_single_writer_volumes,
         test_mcp_images_pinned, test_metadata_stays_blocked_when_internet_except_is_overridden]

if __name__ == "__main__":
    for t in TESTS:
        t()
    if FAILS:
        print("\n%d check(s) failed" % len(FAILS))
        sys.exit(1)
    print("\nall checks passed")
