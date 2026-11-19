# Sign in with your identity provider (langstroth-auth)

**langstroth-auth** lets people reach your relay by signing in with the identity
provider (IdP) you already run, instead of pasting relay tokens around. It
works with any OpenID Connect provider: [Pocket ID](ligus-auth-pocket-id.md)
for a home lab, [Dex](ligus-auth-dex.md) to federate GitHub, LDAP or SAML
(and the Dex a Kubeflow cluster already has), or Keycloak, Authentik, Okta,
Entra ID and the like. It also has a small SCIM 2.0 endpoint, so an IdP that
provisions over SCIM can switch someone's access off the moment they leave.

langstroth-auth is optional. It is a separate service (`langstroth-auth`, in the
[relay repository](https://github.com/langstroth-ai/langstroth-relay)) that runs next to the
relay, never inside it. It ships as the image
`ghcr.io/langstroth-ai/langstroth-auth:<version>` and the Helm chart
`deploy/helm/langstroth-auth` (published at
`oci://ghcr.io/langstroth-ai/charts/langstroth-auth`), or build
`./cmd/langstroth-auth` with Go. It reads `/etc/langstroth-auth/config.json`
unless `--config` or `LANGSTROTH_AUTH_CONFIG` names another file.

## What it does, and what it never does

langstroth-auth decides who can **reach the relay**. It never decides who is **in
a workspace**.

| Layer | Who decides | What it controls |
|---|---|---|
| Relay transport | langstroth-auth (from IdP groups) | which workspace ids a person's devices may read and write on the relay |
| Workspace membership | the workspace's Admins, in the signed roster | who is a member, with which role |
| Content | the workspace key (end-to-end encryption) | who can read or write anything in the workspace |

So signing in with your company account does **not** make you a member of
any workspace. A langstroth-auth token carries no workspace key and writes nothing
to a roster: to join, a person still needs an invite (which carries the
workspace key) and an Admin's signed `MemberAdded`. An IdP that is
compromised, or a group that is set up wrong, can at most let someone reach
ciphertext they cannot open and cannot add to.

!!! warning "SSO is not membership"
    When you remove someone from a group at your IdP, langstroth-auth stops their
    relay access, but they stay in the roster of every workspace they joined
    until an Admin removes them, and they keep any workspace key they already
    had. Removing someone from a workspace is still a roster change plus a
    key rotation.

## How it fits together

```mermaid
sequenceDiagram
  participant D as Device (desktop, CLI, phone)
  participant A as langstroth-auth
  participant I as Your IdP
  participant R as Relay
  D->>A: start sign-in (device code or browser + PKCE)
  A->>I: OIDC authorization code + PKCE, state, nonce
  I-->>A: ID token (issuer, audience, nonce checked; JWKS-verified)
  A-->>D: relay access token (hrt1, 15 min) + refresh token (device-bound)
  D->>R: Authorization: Bearer hrt1.…
  R->>R: verify with LANGSTROTH_RELAY_TOKEN_PUBKEY, check workspaces and tid
```

- **Desktop and CLI** sign in with a **device code**: the app shows a short
  code, you open the langstroth-auth page in a browser, enter it, sign in at your
  IdP and approve the device by name. (`ligus serve` boxes and workers do the
  same with `ligus auth login`.)
- **The phone** will use the system browser with authorization code and PKCE
  (planned for the iOS app).
- Every token request is **signed with the device's own key** over a server
  nonce. The refresh token only works from that device: a copied session file
  is useless elsewhere.

## Tokens

Access tokens are ordinary relay `hrt1` tokens signed by the langstroth-auth
issuer key. The relay verifies them with `LANGSTROTH_RELAY_TOKEN_PUBKEY` exactly as
before.

| Claim | Value |
|---|---|
| `sub` | `<tenant>:<user id>` (an opaque langstroth-auth id, never an email) |
| `exp` | 15 minutes from issue by default; never more than an hour |
| `caps`, `plan`, `turn`, `max_members`, `retention_days` | from the tenant config |
| `workspaces` | the workspace ids granted to the person's groups; never empty |
| `tid` | the tenant id |
| `uid` | a hash of the IdP issuer and subject |
| `did` | the device id whose key signed the token request |

`tid`, `uid` and `did` are optional claims: tokens without them (the ones
`langstroth-relay issue` mints) are byte-for-byte what they were before. langstroth-auth
refuses to mint a token with no workspaces at all, because a relay in `claims`
scope mode would read a missing claim as "any workspace".

Refresh tokens are opaque, stored only as hashes, and rotate on every use. If
an old refresh token is ever presented again, langstroth-auth assumes it was
copied and ends the whole session. A session lasts at most `session_max_age`
(30 days by default) before the person signs in at the IdP again, and an
unused one expires after `refresh_idle_ttl` (7 days).

## Set it up

### 1. Create the issuer key

```sh
langstroth-auth keygen --out /etc/langstroth-auth/signing-key
# public_key=3b6a27bc…   (the private key is only in the file, mode 0600)
```

The private key never leaves that file. langstroth-auth refuses a key file that
other users can read. It can also come from a systemd credential named
`langstroth-auth-signing-key` or a Kubernetes Secret mounted read-only.

### 2. Point the relay at it

```sh
LANGSTROTH_RELAY_TOKEN_PUBKEY=3b6a27bc…        # the public key from keygen
LANGSTROTH_RELAY_WORKSPACE_SCOPE=strict        # tokens only reach their workspaces
LANGSTROTH_RELAY_TENANTS=home                  # optional: only this tenant's tokens
```

| Relay setting | Default | Effect |
|---|---|---|
| `LANGSTROTH_RELAY_TOKEN_PUBKEY` | unset | require signed tokens from this issuer |
| `LANGSTROTH_RELAY_WORKSPACE_SCOPE` | `off` | `strict`: every workspace route needs a token listing that workspace |
| `LANGSTROTH_RELAY_TENANTS` | unset | comma-separated tenant ids; tokens without a listed `tid` get `401` |
| `LANGSTROTH_RELAY_TOKEN_PUBKEY_NEXT` | unset | a second issuer key accepted while you rotate the issuer key |

### 3. Register langstroth-auth at your IdP

Create an OIDC client (confidential, with a secret, or public with PKCE
only) with the redirect URI

```text
https://auth.example.com/t/<tenant>/callback
```

and the scopes `openid email profile`, plus `groups` if group membership
should drive access.

### 4. Write the config

`/etc/langstroth-auth/config.json`:

```json
{
  "listen": ":8090",
  "admin_listen": "127.0.0.1:8091",
  "public_url": "https://auth.example.com",
  "signing_key_file": "/etc/langstroth-auth/signing-key",
  "store": {"driver": "file", "path": "/var/lib/langstroth-auth/state.json"},
  "access_ttl": "15m",
  "refresh_idle_ttl": "168h",
  "session_max_age": "720h",
  "trusted_proxies": ["10.0.0.0/8"],
  "tenants": [
    {
      "id": "home",
      "oidc": {
        "issuer": "https://id.example.com",
        "client_id": "langstroth-auth",
        "client_secret_file": "/etc/langstroth-auth/idp-client-secret",
        "scopes": ["openid", "email", "profile", "groups"]
      },
      "provisioning": "jit",
      "idp_groups": true,
      "workspaces": ["7f0c2a5e-3d4b-4b8e-9a51-2a6f3c1d9e01"],
      "grants": {"family": ["7f0c2a5e-3d4b-4b8e-9a51-2a6f3c1d9e01"]}
    }
  ]
}
```

Then `langstroth-auth check-config --config /etc/langstroth-auth/config.json` and
`langstroth-auth serve --config /etc/langstroth-auth/config.json`. Put it behind TLS
(the relay's reverse proxy works) and list that proxy in `trusted_proxies`.

#### Config reference

| Key | Default | Meaning |
|---|---|---|
| `listen` | `:8090` | public listener (sign-in pages, token endpoint, SCIM) |
| `admin_listen` | off | operator API; needs `LANGSTROTH_AUTH_ADMIN_TOKEN_FILE`. Keep it private |
| `public_url` | required | the address people and the IdP reach |
| `signing_key_file` | | issuer seed file (or `LANGSTROTH_AUTH_SIGNING_KEY_FILE`, a systemd credential, `LANGSTROTH_AUTH_SIGNING_KEY`) |
| `store` | file, memory only | `{"driver":"file","path":…}` for one instance, `{"driver":"postgres","dsn_env":"LANGSTROTH_AUTH_DATABASE_URL"}` for more |
| `access_ttl` | `15m` | at most `1h` |
| `refresh_idle_ttl` | `168h` | |
| `session_max_age` | `720h` | how often people must sign in at the IdP again |
| `clock_skew` | `60s` | leeway for ID token times, at most `5m` |
| `rate_per_ip`, `burst_per_ip` | 5/s, 30 | per-IP limit on the public endpoints |
| `audit_log` | stderr | JSON audit log file |
| `tenants[].oidc.issuer` | required | must match the IdP's discovery document exactly; `https` unless `allow_insecure_http` |
| `tenants[].oidc.client_id`, `client_secret_file` / `client_secret_env` | | the client you registered |
| `tenants[].oidc.token_auth` | `basic` with a secret, else `none` | `basic`, `post` or `none` |
| `tenants[].oidc.claim_map` | `sub`, `email`, `groups` | which claims hold the subject, email and groups |
| `tenants[].oidc.allowed_algs` | RS/PS/ES/EdDSA | ID token algorithms; `none` and HMAC are never accepted |
| `tenants[].provisioning` | `jit` | `jit`: first sign-in creates the user. `scim`: only SCIM-provisioned, active users may sign in |
| `tenants[].link_claim`, `link_attr` | `email`, `userName` | how a first sign-in finds its SCIM user (an email must be verified) |
| `tenants[].idp_groups` | `false` | also trust the ID token's groups (captured at sign-in) |
| `tenants[].scim_token_file` / `scim_token_env` / `scim_token_sha256` | none | enables SCIM for the tenant |
| `tenants[].workspaces` | | the workspace ids this tenant owns. `["*"]` is allowed only with a single tenant |
| `tenants[].grants` | | group name to workspace ids, seeded into the store at start |
| `tenants[].redirect_uris` | | extra exact redirect URIs (the phone app's); loopback is always allowed |

A workspace id is the relay room id that every device of the workspace
uses. You can list them, or, on a single-tenant install, use `["*"]` and grant
`*` to a group.

## Sign in

**Desktop:** Settings, Team sync, *Organization sign-in*. Paste
`https://auth.example.com/t/<tenant>`, press *Sign in*, enter the code shown
in the browser page that opens, sign in at your IdP and approve the device.
While the session is valid its token is used instead of the *Relay access
token* field.

**CLI, `ligus serve`, workers:**

```sh
ligus auth login https://auth.example.com/t/home --label serve-box
#   To sign in this box, open https://auth.example.com/t/home/device
#   and enter the code:  BCDF-GHJK
ligus auth status
ligus auth logout
```

The session is saved in `<data dir>/ligus-auth.json` (mode 0600), and every
`ligus` command refreshes the token in the background. An explicit
`LIGUS_RELAY_ACCESS_TOKEN` takes precedence over the sign-in.

Then join workspaces as usual, with an invite: langstroth-auth only opens the
relay.

## Provisioning and deprovisioning (SCIM)

Give each tenant a SCIM token (`langstroth-auth gen-token`; store only its hash
with `scim_token_sha256`, or keep it in a file), and point your IdP's SCIM
app at

```text
https://auth.example.com/t/<tenant>/scim/v2
```

It supports `Users` and `Groups` (create, read, filter by `userName`,
`externalId` or `displayName`, replace, patch, delete) and
`ServiceProviderConfig`. langstroth-auth stores only `userName`, `externalId`,
`displayName` and `active`; email lists, phone numbers and the like are
accepted and dropped.

- Setting a user `active: false`, or deleting them, **revokes their refresh
  tokens at once**. Their current access token stops working within the
  access TTL (15 minutes by default).
- Removing someone from a group narrows their workspaces at their next
  refresh. With no grants left, the refresh is refused.
- Without SCIM, people lose access when their session reaches
  `session_max_age` and the IdP refuses the next sign-in, or when you switch
  them off with the admin API. Shorten `session_max_age` if your IdP has no
  SCIM.

## Operator API

On `admin_listen`, with `Authorization: Bearer <LANGSTROTH_AUTH_ADMIN_TOKEN_FILE contents>`:

| Route | Does |
|---|---|
| `GET /admin/v1/tenants` | tenants, their workspaces and modes |
| `GET /admin/v1/tenants/{t}/grants` | group grants |
| `PUT /admin/v1/tenants/{t}/grants/{group}` `{"workspaces":[…]}` | set a grant (only the tenant's own workspaces) |
| `DELETE /admin/v1/tenants/{t}/grants/{group}` | remove a grant |
| `GET /admin/v1/tenants/{t}/users` | users |
| `POST /admin/v1/tenants/{t}/users/{id}/active` `{"active":false}` | switch someone off (revokes their sessions) |
| `POST /admin/v1/tenants/{t}/users/{id}/revoke` | end someone's sessions |
| `POST /admin/v1/tenants/{t}/devices/{device}/revoke` | end the sessions of one device |

## Several tenants

Each tenant has its own IdP, client, SCIM token, users, groups and
workspaces. langstroth-auth keeps them apart:

- a workspace id belongs to exactly one tenant (overlaps refuse to start), and
  grants can name only the tenant's own workspaces, checked again every time a
  token is minted;
- ID tokens are checked against that tenant's issuer, client and keys only;
- the same IdP subject in two tenants is two unrelated users;
- sign-in state, codes, device codes, nonces and refresh tokens from one
  tenant are rejected by another;
- with `LANGSTROTH_RELAY_TENANTS`, the relay itself refuses other tenants' tokens.

`LANGSTROTH_RELAY_TENANTS` only filters tokens. For tenants that don't
trust each other, the relay's tenancy mode (`LANGSTROTH_RELAY_TENANCY`)
also keeps each tenant's workspaces, quotas and admin scope apart; see
[Multi-tenant](multi-tenant.md).

## Security notes

- **OIDC:** authorization code with PKCE (S256) to the IdP, single-use
  `state` and `nonce`, exact issuer match in discovery and in the ID token
  (and in the `iss` response parameter, RFC 9207), audience and authorized
  party checks, `exp`/`iat`/`nbf` with bounded clock skew, keys only from the
  IdP's JWKS (refetched on an unknown key id, at most every 30 seconds;
  retired keys stop working at the next refresh). Embedded or remote key hints
  in token headers are refused.
- **Clients:** PKCE is mandatory; redirects go only to loopback addresses or
  configured URIs; the device approval page is bound to the browser that
  signed in and shows the device's name and code.
- **Secrets:** the issuer key, IdP client secret, SCIM tokens and admin token
  come from files or the environment and are never logged. Refresh tokens,
  SCIM tokens and admin tokens are stored only as SHA-256 hashes.
- **Rate limits** on every public endpoint, a tighter one on device-code
  entry, and on failed SCIM or admin authentication.
- **Audit log:** one JSON line per sign-in, token, refresh, reuse detection,
  SCIM change and admin action. People appear as their opaque langstroth-auth id or
  the hashed `uid`; device ids and IP addresses as salted hashes. No names or
  emails.

## Limits to know

- Access tokens are bearer tokens: a stolen one works until it expires (at
  most the access TTL). Only refresh tokens are bound to the device.
- A phone that stays offline longer than `refresh_idle_ttl` must sign in
  again.
- The file store is for one instance. With Postgres you can run more, but
  sign-in and device-code steps are held in memory, so give the load balancer
  session affinity. The Helm chart (`langstroth-auth`) runs exactly one
  replica.
- Rotating the issuer key: start the relay with `LANGSTROTH_RELAY_TOKEN_PUBKEY_NEXT`
  set to the new key, switch langstroth-auth to it, and after an access TTL make it
  the only key.
