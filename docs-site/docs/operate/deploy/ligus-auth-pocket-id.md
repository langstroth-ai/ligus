# langstroth-auth with Pocket ID

[Pocket ID](https://pocket-id.org) is a small, passkey-only OIDC provider:
people sign in with Touch ID, Face ID or a security key, and there are no
passwords to leak. It suits a home lab or a family relay. This page sets up
Pocket ID, [langstroth-auth](ligus-auth.md) and the relay together.

Pocket ID's passkeys and the passkeys Ligus will use for presence on admin
actions (Phase 0, Track B; not in 0.1.0) can be the same physical
authenticator, but nothing in the code ties them together: Pocket ID gates
the relay, while Ligus presence will gate admin events in a workspace.

## What you need

- three names pointing at one box, for example `id.example.com`,
  `auth.example.com` and `relay.example.com`;
- Docker with Compose;
- the files in the relay repository under
  `deploy/compose/ligus-auth-pocket-id/` (`docker-compose.yml`, `Caddyfile`,
  `ligus-auth.json`).

## 1. Keys

```sh
cd deploy/compose/ligus-auth-pocket-id
mkdir -p secrets && chmod 700 secrets
docker run --rm -v "$PWD/secrets:/out" --user "$(id -u)" \
  ghcr.io/langstroth-ai/langstroth-auth:0.2.0 keygen --out /out/signing-key
# public_key=3b6a27bc…
echo "LIGUS_RELAY_TOKEN_PUBKEY=3b6a27bc…" > .env
```

## 2. Start Pocket ID and create the client

Replace `example.com` in `Caddyfile`, `docker-compose.yml` and
`ligus-auth.json`, then start Caddy and Pocket ID first:

```sh
docker compose up -d caddy pocket-id
```

Open `https://id.example.com/setup`, create your admin account with a
passkey, then in **OIDC Clients** add a client:

| Field | Value |
|---|---|
| Name | Ligus |
| Callback URLs | `https://auth.example.com/t/home/callback` |
| Public client | off (keep the secret) |
| PKCE | on |

Copy the client id into `ligus-auth.json` (`oidc.client_id`) and save the
secret:

```sh
printf %s '<client secret>' > secrets/pocket-id-client-secret
chmod 600 secrets/pocket-id-client-secret
```

## 3. Groups

In Pocket ID, create a **user group** called `ligus` and add the people who
should reach the relay. The example config trusts the ID token's `groups`
claim (`idp_groups: true`), asks for the `groups` scope, and grants the
`ligus` group every workspace on this single-tenant relay:

```json
"workspaces": ["*"],
"grants": {"ligus": ["*"]}
```

To be stricter, list workspace ids instead of `*` and grant them per group.

## 4. Start langstroth-auth and the relay

```sh
docker compose up -d
docker compose logs langstroth-auth | head
```

The relay runs with `LIGUS_RELAY_WORKSPACE_SCOPE=strict` and
`LIGUS_RELAY_TENANTS=home`, so only tokens from this langstroth-auth reach it.

## 5. Sign in

- **Desktop:** Settings, Team sync, Organization sign-in, with
  `https://auth.example.com/t/home`, and set the relay URL to
  `https://relay.example.com`.
- **Serve box or worker:** `ligus auth login https://auth.example.com/t/home`.

Then create or join workspaces with invites as usual.

## Deprovisioning

Pocket ID does not push changes over SCIM, so langstroth-auth learns about
removals at the next IdP sign-in. To cut someone off now, remove them from
the `ligus` group in Pocket ID **and** switch them off in langstroth-auth:

```sh
curl -sS -H "Authorization: Bearer $(cat admin-token)" \
  http://127.0.0.1:8091/admin/v1/tenants/home/users
curl -sS -H "Authorization: Bearer $(cat admin-token)" -X POST \
  -d '{"active":false}' http://127.0.0.1:8091/admin/v1/tenants/home/users/<id>/active
```

(The admin API is off unless you set `admin_listen` and
`LIGUS_AUTH_ADMIN_TOKEN_FILE`; keep it on loopback.) Then remove them from
the workspaces they joined and rotate the workspace key: langstroth-auth gates the
relay, not membership.

Lower `session_max_age` (for example to `168h`) if you want people to
re-confirm with their passkey at Pocket ID every week.
