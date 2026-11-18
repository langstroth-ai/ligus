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
        "--set", "networkPolicy.relayNamespace=langstroth-relay"]


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


# --- Model gateways: allow-list file, credentials as files, exact egress -----

def _gw(i=0, **kv):
    """--set args for modelGateways[i]; kv maps dotted keys to values."""
    base = {"name": "team-gw", "url": "https://gw.gw.svc.cluster.local:8443",
            "auth.mode": "serviceAccountToken", "auth.audience": "llm-gw",
            "networkPolicy.namespaceSelector.matchLabels.kubernetes\\.io/metadata\\.name": "gw",
            "networkPolicy.ports[0]": "8443"}
    base.update(kv)
    out = []
    for k, v in base.items():
        if v is None:
            continue
        out += ["--set-string" if k in ("name", "url", "auth.mode", "auth.audience", "auth.secretName",
                                         "auth.secretKey", "auth.header") else "--set",
                "modelGateways[%d].%s=%s" % (i, k, v)]
    return out


def test_model_gateways():
    import json
    docs = render(*_gw(**{"auth.expirationSeconds": "1800", "attribution": "true"}))
    cms = [d for d in docs if d["kind"] == "ConfigMap" and d["metadata"]["name"].endswith("-model-gateways")]
    check("model gateways ConfigMap renders", len(cms) == 1)
    cfg = json.loads(cms[0]["data"]["model-gateways.json"]) if cms else {}
    g = (cfg.get("gateways") or [{}])[0]
    check("allow-list JSON names the gateway, URL and wire",
          g.get("name") == "team-gw" and g.get("url") == "https://gw.gw.svc.cluster.local:8443" and g.get("api") == "openai")
    check("allow-list JSON points at the projected token file, no secret inline",
          g.get("auth") == {"mode": "serviceAccountToken", "tokenFile": "/etc/ligus/model-gateways/team-gw/token"}
          and g.get("attribution") is True and "caFile" not in g, str(g))
    hosts = [(n, t) for k, n, t in pod_templates(docs) if k == "StatefulSet"]
    for n, t in hosts:
        spec = t["spec"]
        check("%s keeps automountServiceAccountToken false" % n, spec.get("automountServiceAccountToken") is False)
        vols = {v["name"]: v for v in spec.get("volumes") or []}
        src = ((vols.get("mg-team-gw") or {}).get("projected") or {}).get("sources") or []
        sat = [s_["serviceAccountToken"] for s_ in src if "serviceAccountToken" in s_]
        check("%s mounts an audience-bound, expiring SA token" % n,
              sat == [{"audience": "llm-gw", "expirationSeconds": 1800, "path": "token"}], str(sat))
        c = [c for c in spec["containers"] if c["name"] in ("serve", "worker")][0]
        env = {e["name"]: e.get("value") for e in c.get("env") or []}
        check("%s gets LIGUS_MODEL_GATEWAYS_FILE" % n,
              env.get("LIGUS_MODEL_GATEWAYS_FILE") == "/etc/ligus/model-gateways-config/model-gateways.json")
        check("%s is gateway-exclusive by default" % n, "LIGUS_MODEL_GATEWAYS_EXCLUSIVE" not in env)
        mounts = {m["name"]: m for m in c.get("volumeMounts") or []}
        check("%s mounts gateway credentials read-only under /etc/ligus/model-gateways" % n,
              mounts.get("mg-team-gw", {}).get("mountPath") == "/etc/ligus/model-gateways/team-gw"
              and mounts["mg-team-gw"].get("readOnly") is True)
        check("%s puts no gateway credential in env" % n,
              not any("valueFrom" in e and "MODEL_GATEWAY" in e["name"] for e in c.get("env") or []))
    nps = [p for p in policies(docs) if p["metadata"]["name"].endswith("-model-gateways")]
    check("model gateway NetworkPolicy renders", len(nps) == 1)
    if nps:
        sp = nps[0]["spec"]
        check("it selects the controller pods only",
              sp["podSelector"]["matchLabels"].get("ligus.ai/role") == "controller" and sp["policyTypes"] == ["Egress"])
        rules = sp.get("egress") or []
        check("one rule, exactly the listed peer and port",
              len(rules) == 1 and rules[0]["ports"] == [{"port": 8443, "protocol": "TCP"}]
              and rules[0]["to"] == [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "gw"}}}],
              str(rules))
    # ipBlock: exactly the listed private CIDR.
    docs = render(*_gw(**{"networkPolicy.namespaceSelector.matchLabels.kubernetes\\.io/metadata\\.name": None,
                          "networkPolicy.ipBlock.cidrs[0]": "10.20.30.40/32"}))
    np_ = [p for p in policies(docs) if p["metadata"]["name"].endswith("-model-gateways")][0]
    check("a private-range gateway opens only its listed CIDR",
          np_["spec"]["egress"][0]["to"] == [{"ipBlock": {"cidr": "10.20.30.40/32"}}])
    # No policy without restricted egress (it would isolate open pods).
    docs = render(*_gw(), "--set", "networkPolicy.restrictEgress=false")
    check("no model gateway policy when egress isn't restricted",
          not [p for p in policies(docs) if p["metadata"]["name"].endswith("-model-gateways")])
    # Exclusive off, apiKey and mtls shapes.
    docs = render(*_gw(**{"auth.mode": "apiKey", "auth.secretName": "k", "auth.secretKey": "key", "auth.header": "x-api-key"}),
                  "--set", "modelGatewaysExclusive=false")
    serve = [t for k, n, t in pod_templates(docs) if n.endswith("-serve")][0]
    env = {e["name"]: e.get("value") for e in serve["spec"]["containers"][0]["env"]}
    check("modelGatewaysExclusive=false sets LIGUS_MODEL_GATEWAYS_EXCLUSIVE=0", env.get("LIGUS_MODEL_GATEWAYS_EXCLUSIVE") == "0")
    vols = {v["name"]: v for v in serve["spec"]["volumes"]}
    check("apiKey comes from the Secret as a file",
          vols["mg-team-gw"]["projected"]["sources"] == [{"secret": {"name": "k", "items": [{"key": "key", "path": "api-key"}]}}])
    cm = [d for d in docs if d["kind"] == "ConfigMap" and d["metadata"]["name"].endswith("-model-gateways")][0]
    g = json.loads(cm["data"]["model-gateways.json"])["gateways"][0]
    check("apiKey JSON names the key file and header",
          g["auth"] == {"mode": "apiKey", "keyFile": "/etc/ligus/model-gateways/team-gw/api-key", "header": "x-api-key"})
    docs = render(*_gw(**{"auth.mode": "mtls", "auth.secretName": "client-tls"}))
    cm = [d for d in docs if d["kind"] == "ConfigMap" and d["metadata"]["name"].endswith("-model-gateways")][0]
    g = json.loads(cm["data"]["model-gateways.json"])["gateways"][0]
    check("mtls JSON pins the CA and names cert and key",
          g.get("caFile", "").endswith("/ca.crt") and g["auth"].get("certFile", "").endswith("/tls.crt")
          and g["auth"].get("clientKeyFile", "").endswith("/tls.key"))
    # Refusals.
    bad = {
        "a catch-all IPv4 CIDR": dict(**{"networkPolicy.ipBlock.cidrs[0]": "0.0.0.0/0"}),
        "a /4 IPv4 CIDR": dict(**{"networkPolicy.ipBlock.cidrs[0]": "16.0.0.0/4"}),
        "a catch-all IPv6 CIDR": dict(**{"networkPolicy.ipBlock.cidrs[0]": "::/0"}),
        "a CIDR overlapping link-local metadata": dict(**{"networkPolicy.ipBlock.cidrs[0]": "169.0.0.0/8"}),
        "a CIDR inside link-local": dict(**{"networkPolicy.ipBlock.cidrs[0]": "169.254.1.0/24"}),
        "a CIDR covering Alibaba metadata": dict(**{"networkPolicy.ipBlock.cidrs[0]": "100.100.0.0/16"}),
        "a CIDR covering AWS IPv6 metadata": dict(**{"networkPolicy.ipBlock.cidrs[0]": "fd00::/16"}),
        "an IPv6 link-local CIDR": dict(**{"networkPolicy.ipBlock.cidrs[0]": "fe80::/64"}),
        "a gateway with no peers": dict(**{"networkPolicy.namespaceSelector.matchLabels.kubernetes\\.io/metadata\\.name": None}),
        "a gateway with no ports": dict(**{"networkPolicy.ports[0]": None}),
        "plaintext http without allowPlaintext": dict(url="http://gw.gw.svc:4000"),
        "a URL with credentials": dict(url="https://u:p@gw.example"),
        "mtls over http": dict(url="http://gw.gw.svc:4000", allowPlaintext="true",
                               **{"auth.mode": "mtls", "auth.secretName": "c"}),
        "apiKey without a Secret": dict(**{"auth.mode": "apiKey"}),
        "mtls without a Secret": dict(**{"auth.mode": "mtls"}),
        "an unknown auth mode": dict(**{"auth.mode": "basic"}),
        "a short-lived SA token below the kubelet minimum": dict(**{"auth.expirationSeconds": "60"}),
        "an SA token that lives longer than a day": dict(**{"auth.expirationSeconds": "604800"}),
        "an SA token for the kube-apiserver's audience":
            dict(**{"auth.audience": "https://kubernetes.default.svc.cluster.local"}),
        "an SA token for the kube-apiserver's short audience": dict(**{"auth.audience": "kubernetes.default.svc"}),
        "a bad name": dict(name="Team_GW"),
        "an unknown wire": dict(api="gemini"),
    }
    for what, kv in bad.items():
        p = render(*_gw(**kv), ok=False)
        check("modelGateways refuses %s" % what, p.returncode != 0, p.stdout[-200:])
    p = render(*_gw(), *_gw(1), ok=False)
    check("modelGateways refuses duplicate names", p.returncode != 0)
    # One audience per gateway: a gateway must not be able to replay the
    # token it was sent to another gateway (or to the API server).
    p = render(*_gw(), *_gw(1, name="other-gw"), ok=False)
    check("modelGateways refuses two gateways sharing an SA token audience", p.returncode != 0)
    p = render(*_gw(), *_gw(1, name="other-gw", **{"auth.audience": "other-aud"}), ok=False)
    check("modelGateways accepts distinct audiences", p.returncode == 0, p.stderr[-300:])
    p = render(*_gw(), "--set", "serve.extraEnv[0].name=LIGUS_MODEL_GATEWAYS_FILE", "--set", "serve.extraEnv[0].value=/x", ok=False)
    check("extraEnv can't override the gateway allow-list", p.returncode != 0)
    for ok_cidr in ("10.0.0.0/8", "2001:db8::/32", "fd00:ec3::/32", "100.101.0.0/16"):
        p = render(*_gw(**{"networkPolicy.ipBlock.cidrs[0]": ok_cidr}), ok=False)
        check("modelGateways accepts %s" % ok_cidr, p.returncode == 0, p.stderr[-300:])
    p = render(*_gw(url="http://gw.gw.svc:4000", allowPlaintext="true"), ok=False)
    check("allowPlaintext opts into an in-mesh http:// gateway", p.returncode == 0, p.stderr[-300:])
    docs = render("-f", os.path.join(CHART, "examples", "model-gateway-values.yaml"))
    check("the example values render", any(d["metadata"]["name"].endswith("-model-gateways") for d in docs))
    docs = render()
    check("no model gateway objects by default",
          not any(d["metadata"]["name"].endswith("-model-gateways") for d in docs))


# --- The cut executor/PR-test template and otel placeholders stay gone ----

def test_removed_values_refuse():
    for v in ("sandbox.enabled=true", "otel.endpoint=http://collector:4317"):
        p = render("--set", v, ok=False)
        check("%s is refused (unsupported in 0.1)" % v.split("=")[0], p.returncode != 0)
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


# --- extraEnv can't put secrets in env or undo the fail-closed settings -----

def test_extra_env_cannot_override_secrets_or_sandbox():
    # Kubernetes keeps the last of two env entries with one name, and Ligus
    # prefers LIGUS_WORKSPACE_KEY from env over the mounted cli.key: extraEnv
    # would silently override the chart's secret files and sandbox settings.
    for comp in ("serve", "worker"):
        extra = ["--set", "worker.enabled=true", "--set", "worker.instances[0].name=a"] if comp == "worker" else []
        for name, val in (("LIGUS_REQUIRE_SANDBOX", "0"), ("LIGUS_SANDBOX", "unconfined"),
                          ("LIGUS_WORKSPACE_KEY", "k"), ("LIGUS_SECRETS_DIR", "/tmp"),
                          ("LIGUS_RELAY_ACCESS_TOKEN_FILE", "/tmp/t"), ("LIGUS_SECRET_BACKEND", "file")):
            p = render(*extra, "--set", "%s.extraEnv[0].name=%s" % (comp, name),
                       "--set", "%s.extraEnv[0].value=%s" % (comp, val), ok=False)
            check("%s.extraEnv %s is refused" % (comp, name), p.returncode != 0)
        p = render(*extra, "--set", "%s.extraEnv[0].name=SOMETHING" % comp,
                   "--set", "%s.extraEnv[0].valueFrom.secretKeyRef.name=ws" % comp,
                   "--set", "%s.extraEnv[0].valueFrom.secretKeyRef.key=key" % comp, ok=False)
        check("%s.extraEnv from a Secret is refused (secrets are files)" % comp, p.returncode != 0)
        docs = render(*extra, "--set", "%s.extraEnv[0].name=RUST_LOG" % comp, "--set", "%s.extraEnv[0].value=info" % comp)
        check("%s.extraEnv of a plain setting renders" % comp, bool(docs))
    p = render("--set", "agentEnvAllowList[0]=LIGUS_WORKSPACE_KEY",
               "--set", "serve.agentEnv[0].name=LIGUS_WORKSPACE_KEY",
               "--set", "serve.agentEnv[0].secretName=ws", "--set", "serve.agentEnv[0].secretKey=key", ok=False)
    check("agentEnvAllowList can't be widened to Ligus's own variables", p.returncode != 0)


# --- The image's base layers are pinned ---------------------------------------

def test_dockerfile_bases_pinned_by_digest():
    import re
    path = os.path.join(CHART, "..", "..", "container", "Dockerfile")
    with open(path) as f:
        froms = [l.split()[1] for l in f if re.match(r"^FROM\s", l)]
    check("the Dockerfile has FROM lines", len(froms) >= 2, str(froms))
    for img in froms:
        check("base image %s is pinned by digest" % img, "@sha256:" in img)


# --- helm test pods outlive success so `helm test --logs` can read them -------

def test_test_pods_kept_for_logs():
    # `helm test --logs` reads every test pod's log after the whole suite
    # finishes; a hook-succeeded pod is gone by then, so the command errors and
    # its output (the sync marker the plaintext check greps for) is lost.
    docs = render()
    tests = [d for d in docs if d["kind"] == "Pod"
             and (d["metadata"].get("annotations") or {}).get("helm.sh/hook") == "test"]
    check("the chart renders helm test pods", len(tests) >= 2, str([d["metadata"]["name"] for d in tests]))
    for d in tests:
        pol = d["metadata"]["annotations"].get("helm.sh/hook-delete-policy", "")
        check("test pod %s is not deleted on success" % d["metadata"]["name"], "hook-succeeded" not in pol, pol)


# --- Box admissions (Phase B): serve and workers are their own members -------

def test_box_admissions():
    docs = render("--set", "serve.admission.existingSecret=sa",
                  "--set", "worker.enabled=true", "--set", "worker.instances[0].name=w1",
                  "--set", "worker.instances[0].admission.existingSecret=wa",
                  "--set", "worker.instances[1].name=w2")
    seen = {}
    for kind, name, tpl in pod_templates(docs):
        if (tpl["metadata"].get("labels") or {}).get("app.kubernetes.io/name") != "ligus" or kind != "StatefulSet":
            continue
        spec = tpl["spec"]
        main = spec["containers"][0]
        env = {e["name"]: e for e in main.get("env") or []}
        vols = {v["name"]: v for v in spec.get("volumes") or []}
        seen[name] = "LIGUS_ADMISSION_FILE" in env
        check("%s never sets LIGUS_SERVE_OWNER" % name, "LIGUS_SERVE_OWNER" not in env)
        if "LIGUS_ADMISSION_FILE" in env:
            e = env["LIGUS_ADMISSION_FILE"]
            check("%s: the admission is a file, not an env secret" % name, "valueFrom" not in e and e["value"].startswith("/etc/ligus/secrets/admission/"))
            v = vols.get("admission", {}).get("secret", {})
            check("%s: the admission volume is 0400 and optional (deleted once redeemed)" % name,
                  v.get("defaultMode") == 0o400 and v.get("optional") is True, str(v))
            mounts = [m for m in main.get("volumeMounts") or [] if m["name"] == "admission"]
            check("%s: the admission is mounted read-only" % name, len(mounts) == 1 and mounts[0].get("readOnly") is True)
            for c in spec.get("initContainers") or []:
                check("%s/%s: the preflight never sees the admission" % (name, c["name"]),
                      not any(m["name"] == "admission" for m in c.get("volumeMounts") or []))
    check("serve and w1 get their own admission, w2 none",
          seen.get("t-ligus-serve") is True and seen.get("t-ligus-worker-w1") is True and seen.get("t-ligus-worker-w2") is False, str(seen))
    secrets = set()
    for kind, name, tpl in pod_templates(docs):
        for v in tpl["spec"].get("volumes") or []:
            if v["name"] == "admission":
                secrets.add(v["secret"]["secretName"])
    check("each box reads only its own admission Secret", secrets == {"sa", "wa"}, str(secrets))
    p = render("--set", "serve.owner=alice", ok=False)
    check("the removed serve.owner is refused with a migration error", p.returncode != 0 and "own service member" in p.stderr, p.stderr[-300:])
    for name in ("LIGUS_ADMISSION_FILE", "LIGUS_SERVE_OWNER"):
        p = render("--set", "serve.extraEnv[0].name=%s" % name, "--set", "serve.extraEnv[0].value=x", ok=False)
        check("serve.extraEnv %s is refused" % name, p.returncode != 0)


TESTS = [test_box_admissions, test_test_pods_kept_for_logs, test_no_generated_secrets, test_test_pods_hold_no_workspace_key, test_dns_egress_is_scoped, test_egress_closed_and_metadata_blocked, test_serve_ingress_excludes_foreign_pods,
         test_removed_values_refuse, test_secrets_are_files, test_single_writer_volumes,
         test_mcp_images_pinned, test_metadata_stays_blocked_when_internet_except_is_overridden,
         test_extra_env_cannot_override_secrets_or_sandbox, test_dockerfile_bases_pinned_by_digest,
         test_model_gateways]

if __name__ == "__main__":
    for t in TESTS:
        t()
    if FAILS:
        print("\n%d check(s) failed" % len(FAILS))
        sys.exit(1)
    print("\nall checks passed")
