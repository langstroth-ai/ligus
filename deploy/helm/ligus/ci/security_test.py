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
    docs = render("--set", "sandbox.enabled=true", "--set", "networkPolicy.restrictEgress=true",
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


TESTS = [test_dns_egress_is_scoped]

if __name__ == "__main__":
    for t in TESTS:
        t()
    if FAILS:
        print("\n%d check(s) failed" % len(FAILS))
        sys.exit(1)
    print("\nall checks passed")
