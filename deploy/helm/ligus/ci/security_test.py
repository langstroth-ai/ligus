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
BASE = ["--set", "relayUrl=http://relay.example:8443", "--set", "workspace.generate=true"]


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
    docs = render("--set", "networkPolicy.restrictEgress=true",
                  "--set", "networkPolicy.relayNamespace=ligus-relay")
    for np in policies(docs):
        for rule in np["spec"].get("egress") or []:
            ports = [p.get("port") for p in rule.get("ports") or []]
            if 53 in ports:
                to = rule.get("to") or []
                scoped = bool(to) and all(
                    peer.get("namespaceSelector") and peer.get("podSelector") for peer in to)
                check("DNS egress of %s is limited to the cluster resolver" % np["metadata"]["name"],
                      scoped, "port 53 open to any address: a direct exfiltration channel")


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
    docs = render("--set", "worker.enabled=true", "--set", "serve.workspaceSecrets.existingSecret=ws",
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


TESTS = [test_dns_egress_is_scoped, test_serve_ingress_excludes_foreign_pods,
         test_removed_values_refuse, test_secrets_are_files, test_mcp_images_pinned]

if __name__ == "__main__":
    for t in TESTS:
        t()
    if FAILS:
        print("\n%d check(s) failed" % len(FAILS))
        sys.exit(1)
    print("\nall checks passed")
