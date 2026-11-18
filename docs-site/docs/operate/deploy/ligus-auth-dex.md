# langstroth-auth with Dex

[Dex](https://dexidp.io) is an OIDC provider that federates other identity
sources: GitHub organizations and teams, LDAP, SAML, Google, Microsoft and
more. Kubeflow ships with Dex, so a team that runs Kubeflow can point
[langstroth-auth](ligus-auth.md) at the Dex it already has and reuse the same
groups.

## 1. Add a static client to Dex

In Dex's config (for Kubeflow, the `dex` ConfigMap or the
`dex-config` Secret in the `auth` namespace):

```yaml
staticClients:
  - id: langstroth-auth
    name: Ligus
    secretEnv: LIGUS_AUTH_DEX_SECRET
    redirectURIs:
      - https://auth.example.com/t/acme/callback
```

Generate the secret with `langstroth-auth gen-token` and give the same value to
Dex (as `LIGUS_AUTH_DEX_SECRET`) and to langstroth-auth (as a file).

## 2. Groups from your connectors

Dex puts groups in the ID token when the client asks for the `groups` scope.
What they look like depends on the connector:

| Connector | Group names |
|---|---|
| GitHub | `<org>:<team>` (with `teamNameField: slug`), and `<org>` for org membership |
| LDAP | the `nameAttr` of each matching group (`groupSearch`) |
| SAML | the values of `groupsAttr` |
| Kubeflow's default (static passwords) | none: use the admin API, or add a connector |

A GitHub example with two teams:

```yaml
connectors:
  - type: github
    id: github
    name: GitHub
    config:
      clientID: $GITHUB_CLIENT_ID
      clientSecret: $GITHUB_CLIENT_SECRET
      redirectURI: https://dex.example.com/callback
      orgs:
        - name: acme
          teams: [platform, ml]
      teamNameField: slug
```

## 3. langstroth-auth config

```json
{
  "public_url": "https://auth.example.com",
  "signing_key_file": "/etc/ligus-auth/secrets/signing-key",
  "store": {"driver": "file", "path": "/var/lib/ligus-auth/state.json"},
  "refresh_idle_ttl": "24h",
  "session_max_age": "24h",
  "tenants": [
    {
      "id": "acme",
      "oidc": {
        "issuer": "https://dex.example.com",
        "client_id": "langstroth-auth",
        "client_secret_file": "/etc/ligus-auth/secrets/dex-client-secret",
        "scopes": ["openid", "email", "profile", "groups"]
      },
      "provisioning": "jit",
      "idp_groups": true,
      "workspaces": ["7f0c2a5e-3d4b-4b8e-9a51-2a6f3c1d9e01", "b3a1c9d0-5e2f-4c7a-8d16-0f9e4b2a7c55"],
      "grants": {
        "acme:platform": ["7f0c2a5e-3d4b-4b8e-9a51-2a6f3c1d9e01", "b3a1c9d0-5e2f-4c7a-8d16-0f9e4b2a7c55"],
        "acme:ml": ["b3a1c9d0-5e2f-4c7a-8d16-0f9e4b2a7c55"]
      }
    }
  ]
}
```

Dex has no SCIM, so langstroth-auth sees group changes only when someone signs in
at Dex again. That is why this example sets `session_max_age` to a day:
everyone goes back through Dex (and GitHub, LDAP or SAML) daily, and someone
removed from a team loses relay access within a day, or at once if you also
switch them off with the [admin API](ligus-auth.md#operator-api).

## 4. Run it

**Docker Compose:** `deploy/compose/ligus-auth-dex/` in the relay repository
has Dex, langstroth-auth, the relay and Caddy, with this config.

**Kubernetes, next to the langstroth-relay chart:** install the `langstroth-auth`
chart from the same repository.

```sh
kubectl create secret generic langstroth-auth-signing-key \
  --from-file=signing-key=./signing-key
kubectl create secret generic langstroth-auth-secrets \
  --from-file=dex-client-secret=./dex-client-secret
helm install langstroth-auth deploy/helm/ligus-auth -f langstroth-auth-values.yaml
```

`langstroth-auth-values.yaml`:

```yaml
config:
  public_url: https://auth.example.com
  session_max_age: 24h
  refresh_idle_ttl: 24h
  tenants:
    - id: acme
      oidc:
        issuer: https://dex.example.com
        client_id: langstroth-auth
        client_secret_file: /etc/ligus-auth/secrets/dex-client-secret
        scopes: [openid, email, profile, groups]
      idp_groups: true
      workspaces: ["7f0c2a5e-3d4b-4b8e-9a51-2a6f3c1d9e01"]
      grants:
        acme:platform: ["7f0c2a5e-3d4b-4b8e-9a51-2a6f3c1d9e01"]
signingKey:
  existingSecret: langstroth-auth-signing-key
secretFiles:
  existingSecret: langstroth-auth-secrets
persistence:
  enabled: true
ingress:
  enabled: true
  className: nginx
  host: auth.example.com
```

and in the langstroth-relay chart's values (`config.env`):

```yaml
config:
  env:
    LIGUS_RELAY_TOKEN_PUBKEY: "<public key from langstroth-auth keygen>"
    LIGUS_RELAY_WORKSPACE_SCOPE: strict
    LIGUS_RELAY_TENANTS: acme
```

On a Kubeflow cluster, Dex is usually reachable in-cluster at
`http://dex.auth.svc.cluster.local:5556/dex`, but its **issuer** is the
public URL in Dex's config (for example `https://kubeflow.example.com/dex`).
Use the issuer exactly as Dex states it in
`/.well-known/openid-configuration`; langstroth-auth refuses a mismatch.

## 5. Sign in

People use `https://auth.example.com/t/acme` in the desktop's Organization
sign-in or with `ligus auth login`. Their GitHub team (or LDAP or SAML group)
decides which workspaces they can reach on the relay. Joining a workspace is
still an invite plus an Admin's approval in Ligus.
