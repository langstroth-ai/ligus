# Model gateways (LLM gateways) in Ligus 2.0

Status: approved by the owner 2026-11-17; built on `feat/model-gateways`
for 2.0.0. When this document and the code disagree, the code wins.

## 1. Use case and non-goals

A team runs a dedicated LLM gateway: LiteLLM, kgateway or Envoy AI Gateway
(Envoy and the Gateway API), or self-hosted vLLM/KServe pools behind the
Gateway API Inference Extension. It runs in the same cluster or in another
one. Ligus agents running on `ligus serve` or on `ligus worker` hosts in
Kubernetes route their model traffic through it, so that the gateway is the
one place that holds provider keys, enforces quotas and records cost.

Ligus stays **gateway-agnostic**. A gateway is any endpoint that speaks the
OpenAI chat-completions wire or the Anthropic Messages wire. No gateway is
required, none is embedded, and nothing here is specific to one product.

Non-goals: Ligus does not terminate TLS for agents (the egress proxy still
splices `CONNECT` tunnels untouched), does not route between gateways, does
not load-balance, and does not read the gateway's own policies.

## 2. Summary of the design

| Concern | Decision |
|---|---|
| Who decides which gateways exist | The **operator** of a host: the chart's `modelGateways` list, rendered into a mounted file (`LIGUS_MODEL_GATEWAYS_FILE`). Never a workspace member, never an agent. |
| What the workspace sees | Gateway **names** (and their wire, `openai` or `anthropic`) on the host's durable `WorkspaceHost` record. Never a URL, a path or a secret. |
| How an agent uses one | Its workspace runtime names the gateway (`WorkspaceRuntime.gateway`) and the agent is bound to a host that offers it. Both are Admin + passkey presence today; no new event. |
| What a gateway host refuses | With a non-empty allow-list the host is **gateway-exclusive** by default: every agent turn it runs must go through a listed gateway. A runtime whose endpoint is not a listed gateway is refused before anything starts. |
| Credentials | Per gateway: a projected, audience-bound ServiceAccount token (`serviceAccountToken`), an API key file (`apiKey`), or a client certificate with a pinned CA (`mtls`). All are files, never env. The host process reads them; agent children never can. |
| CLI agents | The per-turn egress proxy gains a **gateway route**: plain HTTP on loopback, `http://127.0.0.1:<port>/ligus-gw/<turn-token>/<name>/...`. The proxy strips every client-supplied credential header, adds the gateway's, and forwards over its own TLS client. The CLI gets that base URL and a dummy key. |
| Hosted-API turns | The HTTP model clients use the same `Gateway` object: its TLS client (mTLS identity, pinned CA) and the same header injection function the proxy calls. |
| Attribution | Opt-in per gateway: `X-Ligus-Workspace` and `X-Ligus-Agent`, each a keyed hash (HMAC-SHA256, truncated) of the id. No names, no content. |
| Network | A separate, additive NetworkPolicy opens egress from serve and worker pods to exactly the listed selectors or CIDRs and ports. Metadata and catch-all ranges are refused at render time. |

## 3. Operator allow-list (chart, item 1)

```yaml
modelGateways:
  - name: litellm                 # [a-z0-9-]{1,63}; what the workspace sees
    url: https://litellm.gw.svc.cluster.local:4000   # base URL; the API path is appended
    api: openai                   # openai | anthropic (the wire it speaks)
    auth:
      mode: serviceAccountToken   # serviceAccountToken | apiKey | mtls | none
      audience: litellm           # serviceAccountToken: the token's audience
      expirationSeconds: 3600     # serviceAccountToken: kubelet rotates at 80 %
      # apiKey:  secretName, secretKey, header (default Authorization: Bearer)
      # mtls:    secretName (tls.crt, tls.key, ca.crt keys)
    caSecret: { name: "", key: ca.crt }   # optional pinned CA for https (any mode)
    allowPlaintext: false         # http:// only inside a mesh or isolated path
    attribution: false            # send pseudonymous workspace and agent ids
    networkPolicy:                # what serve and workers may reach for this gateway
      namespaceSelector: { matchLabels: { kubernetes.io/metadata.name: gw } }
      podSelector: { matchLabels: { app: litellm } }
      # or: ipBlock: { cidrs: [203.0.113.10/32] }
      ports: [4000]
```

Rendering:

- `templates/model-gateways.yaml` (a new file, so the existing
  `networkpolicy.yaml` is untouched) renders:
  - a ConfigMap with `model-gateways.json`, the binary's allow-list: name, URL,
    wire, auth mode and the **file paths** of its credential material inside
    the pod, attribution switch;
  - a NetworkPolicy `<release>-model-gateways` selecting the controller pods
    (serve and workers), Egress only, one rule per gateway. NetworkPolicies
    are a union, so it only adds. It renders only when
    `networkPolicy.enabled` and `networkPolicy.restrictEgress` (without
    restricted egress a second Egress policy would isolate pods that were
    open).
- Validation at render time (`fail`): duplicate or malformed names; a URL
  that is not `https://` (or `http://` with `allowPlaintext`); userinfo in the
  URL; `mtls` over plain HTTP; an `ipBlock` CIDR that is a catch-all
  (`0.0.0.0/0`, `::/0`, or any prefix shorter than /8 for IPv4 or /16 for
  IPv6), or that overlaps link-local or a metadata address (169.254.0.0/16,
  100.100.100.200/32, fd00:ec2::254/128); a gateway with neither selectors nor
  CIDRs (a policy rule with no `to` would open everything); no ports.
  A private-range CIDR is allowed, but only exactly as listed: nothing else
  in private ranges is opened (`internetExcept` still applies to
  `allowInternetHttps`).
- Volumes, added to serve and every worker by one include each:
  - `serviceAccountToken`: a projected volume with a
    `serviceAccountToken { audience, expirationSeconds, path }` source, at
    `/etc/ligus/model-gateways/<name>/token`. The pod keeps
    `automountServiceAccountToken: false`; only the audience-bound token is
    mounted.
  - `apiKey`: the Secret key mounted at `/etc/ligus/model-gateways/<name>/api-key`
    (mode 0400).
  - `mtls`: `tls.crt`, `tls.key`, `ca.crt` from the Secret.
  - `caSecret`: `ca.crt`.
- `LIGUS_MODEL_GATEWAYS_FILE=/etc/ligus/model-gateways-config/model-gateways.json`.

## 4. The binary's allow-list (`hive_runtime::model_gateway`)

`ModelGateways::load()` reads `LIGUS_MODEL_GATEWAYS_FILE` once per process
(a missing variable means no gateways; an unreadable or invalid file is a
startup error: an operator who listed gateways must not get a host that
silently runs without them). It validates the same rules as the chart
(name, scheme, no userinfo, mtls needs https), and it:

- adds every credential file's directory to the sandbox deny list
  (`executor::configure_host`), so no agent child reads a token, key or
  client key even if the files moved;
- exposes `offers() -> Vec<HostModelGateway>` (name and wire) for the host
  record;
- exposes `exclusive()`: true when the list is non-empty, unless
  `LIGUS_MODEL_GATEWAYS_EXCLUSIVE=0` (chart: `modelGatewaysExclusive: false`).

`resolve(runtime)` decides a turn's model route:

| Runtime | Host without gateways | Gateway host |
|---|---|---|
| `gateway` names a listed gateway | refused: "this host doesn't offer gateway X" | via that gateway |
| `gateway` names an unlisted one | refused | refused |
| no `gateway`, endpoint equals a listed gateway URL (prefix match on the base URL) | direct (unchanged) | via that gateway (credential injected) |
| no `gateway`, any other endpoint or a CLI's vendor endpoint | direct (unchanged) | refused when exclusive; direct otherwise |

A desktop or a plain custom base URL is unaffected: without a gateways file
nothing changes.

## 5. Host-offered gateways (item 2)

`WorkspaceHost` gains

```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub model_gateways: Vec<HostModelGateway>,   // { name, api }
```

serve and workers fill it from `offers()` when they register themselves
(`ensure_host`, which now also rewrites the record when the offered list
changes). The record is a self-host upsert: the host's own identity may
write it (existing self-host allowance), anyone else needs Admin and
presence (existing rule). A name means nothing without the operator's file
on that host, so a forged offer can't redirect anything: the turn is refused
on a host that doesn't have the gateway locally.

`WorkspaceRuntime` gains

```rust
#[serde(default, skip_serializing_if = "String::is_empty")]
pub gateway: String,
```

and `RuntimeTarget` gains `gateway: Option<String>` (device-local runtimes,
`[[runtimes]] gateway = "..."`). Both are skipped when empty, so every
existing event and config is byte-identical.

The web UI lists, per host, the gateways it offers (Right rail → hosts, and
the agent's host menu), and a runtime form offers a gateway picker built from
those names instead of a URL field. Choosing a gateway for an agent shows
which hosts offer it; binding the agent to a host that doesn't is allowed
(the operator may add it later) but the UI says the agent won't run there.

## 6. Authentication (item 3)

`Gateway::auth_headers()` is called for **every** request (the proxy route
and the HTTP clients), so rotation needs no restart:

- `serviceAccountToken`: reads the projected token file on every request
  (a small file; no cache to go stale). Sent as `Authorization: Bearer <jwt>`. The kubelet refreshes the file at 80 % of
  its lifetime.
- `apiKey`: reads the key file the same way. Sent as
  `Authorization: Bearer <key>` by default, or as a named header
  (`x-api-key` for an Anthropic-wire gateway that wants it, `api-key`).
- `mtls`: no header. `Gateway::client()` builds a reqwest client with the
  identity (cert chain + key PEM) and **only** the pinned CA as trust root
  (`tls_certs_only`). Built per turn and per proxied request, so a rotated
  certificate (cert-manager) is picked up.
- `caFile` (any https mode): trust only that CA.
- `none`: no credential (a gateway that authenticates by mesh identity or
  network position only).

Values are never logged and are held in `Zeroizing` buffers.

## 7. Credential injection for CLI agents (item 4)

The proxy keeps its `CONNECT` splicer (no TLS interception). A turn whose
runtime resolves to a gateway gets, in `EnforcementPlan`:

- `gateway: Some(GatewayRoute { name, api })`;
- no vendor endpoints and no gateway host in the allow-list: the child
  reaches the model **only** through the proxy route (MCP hosts and presets
  are unchanged);
- no provider key in the env.

When the proxy has started, `plan_env` adds the route as the CLI's base URL
and a dummy key:

| CLI | Variables |
|---|---|
| Claude Code (`api: anthropic`) | `ANTHROPIC_BASE_URL`, `ANTHROPIC_API_KEY=ligus-gateway` |
| Codex, Hermes, Aider (`api: openai`) | `OPENAI_BASE_URL`, `OPENAI_API_BASE`, `OPENAI_API_KEY=ligus-gateway` |
| Aider, Hermes (`api: anthropic`) | `ANTHROPIC_BASE_URL`, `ANTHROPIC_API_KEY=ligus-gateway` |
| pi, Antigravity, Copilot | refused: Antigravity and Copilot have no base-URL setting; pi reads its base URL from a provider file Ligus writes before the proxy (and so the route) exists |

The route is `http://127.0.0.1:<port>/ligus-gw/<proxy-token>/<name>`. The
proxy token is the per-turn token the child already holds in `HTTPS_PROXY`,
so the URL adds no capability; it keeps other local processes off the
route. `NO_PROXY` already covers loopback, and every executor already lets
the child reach the proxy port (Seatbelt rule, Landlock port rule, the bwrap
helper's forwarder).

On a gateway route the proxy:

1. checks the token (constant time), that `<name>` is one of the turn's
   gateways, and that the request is the gateway wire's model API only
   (`model_gateway::route_path_allowed`: chat/completions, completions,
   responses, embeddings, messages, count_tokens, `GET /v1/models`; no dot
   segments, encoded dots or slashes), so a child can't use the injected
   credential on the gateway's admin routes (LiteLLM key minting);
2. reads the request head and body (`Content-Length` or chunked; at most
   32 MiB);
3. drops hop-by-hop headers and every credential a client could supply:
   `Authorization`, `Proxy-Authorization`, `X-Api-Key`, `Api-Key`, `Cookie`,
   and any `X-Ligus-*`;
4. adds `Gateway::auth_headers()` and, if enabled, the attribution headers;
5. sends it with `Gateway::client()` to `<gateway url><rest of path>`,
   dialing an address it resolved and vetted itself (never link-local,
   unspecified, multicast or a metadata address);
6. streams the response back chunked, `Connection: close`.

What the child can see: the loopback URL, the dummy key, the proxy token. It
cannot read the credential files (deny list) and never gets the credential in
env, argv or files. Tests: `gateway_route_injects_and_strips` (a fake
upstream records headers), `gateway_credential_absent_from_child` (env,
argv and a scan of the turn's files), `gateway_files_are_denied_to_children`.

## 8. Hosted-API turns

`ResolvedRuntime.gateway: Option<GatewaySelection>` (name plus the turn's
workspace and agent ids for attribution). Dispatch builds `OpenAiClient` /
`AnthropicClient` with `.with_gateway(gw)`: the request goes to the gateway
URL with the provider path appended, through `Gateway::client()`, and
`Gateway::apply()` (the same function the proxy uses) strips any key and adds
the gateway's headers. A runtime's own API key is never sent to a gateway.

## 9. Authorization (item 5)

No new event. Existing rules already cover every change that could redirect
an agent's model traffic:

- `WorkspaceRuntimeUpserted` / `WorkspaceRuntimesUpdated` /
  `WorkspaceRuntimeRemoved`: Admin, and presence under rules v2
  (`authz/rules.rs`). The new `gateway` field rides these events, so setting
  or changing it, or the `endpoint`, needs Admin and presence.
- `AgentUpserted` / `AgentRosterUpdated` that change `host_id` or
  `runtime_id`: Admin and presence (A.2.5).
- `WorkspaceHostUpserted`: the host's own record (self-host) or Admin and
  presence for anyone else's.

Tests: `authz_gateway_change_needs_admin_and_presence` (Contributor denied,
Admin without presence denied, Admin with presence allowed, gateway-only and
endpoint-only changes alike) and the property test
`prop_runtime_route_change_requires_admin_presence` over arbitrary runtime
pairs. No domain-separation string or wire id changes; the new fields are
skipped when empty (`gateway_fields_skip_when_empty`).

## 10. Attribution headers (item 6)

Off by default, per gateway. When on, every request through that gateway
carries

```
X-Ligus-Workspace: <32 hex>   = hex(HMAC-SHA256(k, "ligus-gateway-attribution-v1\0" || "w" || 0x00 || workspace_uuid_bytes)[..16])
X-Ligus-Agent:     <32 hex>   = same with "a" and the agent id
```

`k` is the operator's attribution key file (`modelGatewayAttribution.secretName`,
the same Secret on serve and every worker so the ids agree across hosts),
else a 32-byte key the host generates in its data dir. The gateway can
count, rate-limit and bill per workspace and agent but cannot recover the
ids, names or anything else. The domain string is new and is added to
phase0-specs §0.1. A request with no known workspace or agent omits that
header.

## 11. Threat model: endpoint redirection

| Threat | Mitigation |
|---|---|
| A member points an agent at an attacker endpoint to exfiltrate prompts, code and the gateway credential | Runtime and binding changes need Admin + passkey presence. A gateway host is exclusive: only operator-listed gateways. Credentials never go to a non-listed URL (they are attached only by `Gateway`, keyed by the operator's name → URL). |
| An agent (prompt injection) rewrites its CLI's base URL or config | The CLI's only route is the proxy; the gateway credential is added by the proxy for the listed URL only; the child can't reach any other model host (allow-list, NetworkPolicy). |
| The child reads the credential | Files are on the deny list; not in env or argv; the proxy strips client auth so a key the child forges isn't forwarded either. |
| A forged host record advertises a gateway | Names only; the executing host checks its own operator file. |
| DNS rebinding of the gateway name to metadata or loopback | The proxy and the clients resolve and vet addresses (`ip_forbidden` + metadata), and dial the vetted address; NetworkPolicy refuses metadata. |
| Token replay by the gateway | Audience-bound, short-lived projected tokens; one audience per gateway. A gateway that is compromised can replay its own audience's token only to itself. |
| A token from another namespace | The gateway verifies issuer, audience and the `kubernetes.io` namespace / service account claims (docs). |
| Plain HTTP | Refused unless `allowPlaintext`, and never for mtls. |

Residual risks are listed in the docs page and the threat model: a gateway
operator sees all model traffic (by design); `allowPlaintext` inside a
cluster without a mesh exposes bearer tokens to the node network; a
compromised host process (not a child) holds the credential.

## 12. Tests and end-to-end verification

Unit and property tests: listed above, plus config validation, token
rotation (file replaced → next request carries the new token), mTLS client
construction, and the chart's `security_test.py` cases (catch-all CIDR,
metadata overlap, missing selectors, plaintext without opt-in all refused;
projected volume has audience and expiry; NetworkPolicy rule per gateway).

End-to-end on a local Kubernetes (OrbStack), throwaway namespaces:
kgateway (primary, if it installs cleanly) with JWT auth against the
cluster's OIDC JWKS, a stub OpenAI-compatible backend, Ligus serve with
`modelGateways`; a second, lighter run with LiteLLM or the stub in `apiKey`
mode. Results are recorded in the PR.
