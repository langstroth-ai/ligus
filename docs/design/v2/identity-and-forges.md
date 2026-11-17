# Ligus 2.0: identity without GitHub, and many forges

Status: design spec, written 2026-11-14. Nothing was implemented. All repositories were opened read-only. Ligus is short for *ligustica*.

**Amendment 2026-11-16 (owner decision; the code wins).** 2.0 dropped the SourceHut and Tangled identity-link verifiers. The rows for them in §2.3, the sr.ht "verified by name" residual in S1, the atproto PDS/DID fetching and the reserved `AtprotoRecord` and `OidcAttestation` methods below are superseded. A link naming `sourcehut` or `tangled` still folds unchanged (the wire shape, the statement and `ligus-identity-link-v1@ligus.ai` are untouched) and is shown as self-asserted, never verified (`hive_core::identity_link::DROPPED_PROVIDERS`). A method this build doesn't know, `atprotoRecord` and `oidcAttestation` included, folds through the `#[serde(other)]` arm and shows as "not checked".

**Scope.**

| What | Where | Branch |
|---|---|---|
| Core, runtime, CLI and serve, desktop, web | this repo | `release/v2.0.0` @ `329b2c7d` |
| Relay (Go) and ligus-auth | relay repo (`ligus-ai/relay`) | `release/v0.2.0` @ `1994c3c` |
| Earlier specs this one builds on | [`architecture-review.md`](architecture-review.md), [`phase0-specs.md`](phase0-specs.md), [`directory-trust.md`](directory-trust.md), and the iOS ligus-auth sign-in note (iOS app repo) | n/a |

Unless a path says otherwise, citations are relative to the root of this repo. Relay paths are prefixed `relay/` and refer to the relay repo. Line numbers are as of the commits above.

**Labels.**
- **[V]** means verified: read in code, read in official docs, or seen in an anonymous read-only HTTP response on 2026-11-14.
- **[I]** means inferred.
- External claims carry a URL.

**Owner decisions this spec takes as given.**
- A Ligus account is an account key plus device certificates plus passkeys (WebAuthn, rules v2).
- ligus-auth is provider-agnostic OIDC plus SCIM for organizations.
- The relay never sees a forge token.
- 2.0 starts from scratch. There is no data migration.
- Existing crypto domain-separation strings and wire ids never change.
- Relay `/v1` and `hrt1` stay byte-compatible. Only additive changes are allowed.

---

## 0. Summary

1. **Identity.** A Ligus account is an account key with a random UUIDv4 `account_id`, which is what `IdentityStore::bootstrap` already generates (`crates/hive-runtime/src/identity_store.rs:146-175`).
   - GitHub sign-in stops being identity. The switch from the local id to `account_id_for(github_id)` is deleted from the app.
   - The profile is a display name plus an avatar. Both already ride on `ActorIdentity`.
2. **Linked identities** are optional, opt-in for each workspace, and proven on the device.
   - The universal method is an **SSHSIG statement**. The user signs it with an SSH key the forge already lists publicly for them. That covers GitHub, GitLab, Forgejo, Gitea, Codeberg, SourceHut and Tangled.
   - Every other device verifies it against the forge's anonymous key listing, keyed by the forge's stable numeric id (or DID).
   - This needs **one new string**, the SSHSIG namespace `ligus-identity-link-v1@ligus.ai`. Section 2.4 explains why it can't be avoided.
   - GitHub and GitLab also allow a zero-new-crypto fallback: publish the Ligus account key as a forge *signing* key.
3. **Discovery** happens through invite links and QR codes, and through co-members of shared workspaces. Their key-agreement keys are already signed on the config log, so the relay is not involved.
   - The relay's GitHub social routes are replaced by additive, opaque-id **contact** routes in relay v0.3. A contact is a signed record the client verifies, plus a sealed drop box.
   - The GitHub routes are deprecated: off by default in v0.3, and removed in v0.4.
4. **DMs** get random room ids, carried inside the sealed invite. They are never derived from public ids.
5. **Repo work** goes behind a `Forge` trait. 2.0 ships GitHub, GitLab and Forgejo/Gitea/Codeberg.
   - Tokens live only in the OS secret store of the device or box that does the git work. They are never synced and never sent to the relay.
   - Agents get forge data only through read-only tools in the Ligus MCP server. The token stays in the host process.
6. **Tangled and SourceHut.**
   - Identity links: 2.0, because verifying one is cheap.
   - Repo work: after 2.0. Tangled's data model moved to `org.tangled.*` on 2026-10-30, and writes need atproto OAuth (PAR + DPoP). SourceHut contributes by email.
   - Bitbucket and Azure DevOps: 2.x, on demand, token only. They have no public key listing, so their links can only be self-asserted.

---

## 1. Inventory of current GitHub coupling

There are 117 findings in total. The tables below cover everything that matters for design. Copilot-runtime, CI and updater items are under "Other" and are not identity.

### 1.1 Identity (32 sites)

| # | Where | What |
|---|---|---|
| I1 | `crates/hive-runtime/src/github.rs:1-7` | The module doc says "The GitHub user is the Hive *account*". |
| I2 | `github.rs:13-18` | Device-flow endpoints `github.com/login/device/code` and `/login/oauth/access_token`. Scope `read:user user:email`. |
| I3 | `github.rs:20-27` **[V]** | `ACCOUNT_NS = 0x6869_7665_6769_7468_6163_636f_756e_7401` (ASCII "hivegithaccount\x01"); `account_id_for(id) = UUIDv5(ACCOUNT_NS, "github:<id>")`. This derives the account id from a public id. |
| I4 | `github.rs:50-72` | `GithubAccount{id, login, name, email, avatar_url}` and `account_id()`. |
| I5-I7 | `github.rs:96-199` | Device-flow start and poll, `/user`, `/user/emails`. |
| I8-I10 | `app/build.rs:11,25-41`; `app/src/lib.rs:12882-12924` | The OAuth client id is baked in at build time or set at runtime (`LIGUS_GITHUB_CLIENT_ID`). No client id is committed. |
| I11-I12 | `app/src/lib.rs:350-359, 929-947` | `github_account` in plain `settings.json`; `github_token` in the OS secret store (`SETTINGS_SECRETS`, `secret_store.rs`). |
| I13-I14 | `app/src/lib.rs:12926-13020` | `github_login_poll` adopts the GitHub id as the actor (`adopt_account_identity`), overwrites `git_email` with the GitHub primary email, and calls `register_directory`. |
| I15 | `app/src/lib.rs:11303-11307` | At startup, with a GitHub account present, the app calls `set_author_account(account_id_for(id))`. **This is the local-to-GitHub id switch.** |
| I16 | `app/src/lib.rs:14514-14520` | `github_logout` doesn't revert the actor id. |
| I17 | `app/src/lib.rs:9233-9240` | A `turn_runs_here` workaround for author-id churn at sign-in. |
| I18-I22 | `crates/hive-runtime/src/chat_service.rs:485-560, 780, 1284-1315`; `event_store.rs:437` | Account adoption and migration of the provisional self, dedupe and re-publish. The whole class exists only because the id changes at GitHub sign-in. |
| I23 | `crates/hive-core/src/events.rs:214-222` | The `AccountKeyRegistered` doc says "GitHub-anchored". Only the comment is GitHub-specific; the wire isn't. |
| I24-I27 | `envelope_verifier.rs:28-32,150-160,…`; `identity_verifier.rs:1-72,301-330`; `sync_engine.rs:127-132,704-760` | Dormant "Option C" `GithubSigningKeys` mode, which checks account keys against `api.github.com/users/{login}/ssh_signing_keys`. Enabled only in tests; the env switch is ignored (P0.8). |
| I28 | `app/src/lib.rs:602-623` | `WorkspaceConn.dm_account` (`github:<id>`) and `founder_actor_id` (`github:<id>` for DMs) inside `hivews1:` invites. |
| I29 | `crates/hive-runtime/src/relay_client.rs:353-366` **[V]** | `authed()` attaches `X-Hive-Github-Token` to **every** relay request, not only directory calls. The relay operator can read it (`docs-site/networking/self-host.md:293-300`). |
| I30 | `app/src/lib.rs:6421-6434, 11645-11908, 12089-12160` | Every relay client, the sync loop and probes carry the token. |
| I31 | `crates/hive-cli/src/main.rs:14,55,176,273,362,428` | The headless CLI forwards env `LIGUS_RELAY_GITHUB_TOKEN` the same way. |
| I32 | `web/src/lib/ipc.ts:508-536`; `web/src/components/settings/AccountSection.tsx:8-320`; `web/src/components/Onboarding.tsx:15-19,202-290,520-600,913` | The sign-in UI. Onboarding step 1 offers GitHub. The git email is locked while signed in. |

The relay keys this identity in two different id spaces:
- `github:<numeric id>` for relay membership, friends, inbox, DMs and founder pins;
- `account_id_for(id)` for the roster and sealing.

Both must go.

### 1.2 Discovery (31 sites)

| # | Where | What |
|---|---|---|
| D1-D2 | `relay_client.rs:105-115, 205-258` | `DirectoryEntry{github_id, login, name, devices}`, `AccountIdentity{"github:<id>"}`, and the friends DTOs keyed by login. |
| D3-D4 | `relay_client.rs:702-725` | `POST /v1/directory/register` and `GET /v1/directory/{handle}`. |
| D5-D6 | `relay_client.rs:728-815` | Relay membership `upsert_member{account:"github:<id>", login}` and `join_via_invite{code, login}`. |
| D7 | `relay_client.rs:825-880` | `/v1/admin/*`. The client doc says it is gated by "a GitHub-admin allowlist". |
| D8-D10 | `relay_client.rs:900-1062` | `account/*` and `friends/*`; `post_workspace_invite{toLogin}`. |
| D11-D12 | `app/src/lib.rs:13035-13066, 12545-12550` | `register_directory`. It does nothing without a GitHub token. |
| D13-D14 | `app/src/lib.rs:13107-13230` | `invite_by_handle`: directory lookup, then `account_id_for`, then seal to the directory devices ([directory-trust](directory-trust.md)), then `upsert_member("github:<id>")`, then the inbox. |
| D15 | `app/src/lib.rs:13020-13033`; `crates/hive-runtime/src/directory_trust.rs` | `seal_plan` (verified versus unverified recipients). |
| D16-D22 | `app/src/lib.rs:13234-13655` | Pending invites, join, add or remove a member by handle, `social_ctx`, the friends commands. |
| D23 | `app/src/lib.rs:4324-4368`; `github.rs:201-300` | `import_github_teams` (`read:org`) maps GitHub team members to `account_id_for`. |
| D24 | `app/src/lib.rs:14451-14512` | `generate_agent_bootstrap`: handle, then directory, then `sub = "github:<id>"`, then `mint_hrt1_token`. |
| D25 | `app/src/lib.rs:4579-4581` | Relay admin calls authenticated with the GitHub token. |
| D26-D31 | `web/src/App.tsx:300-304`; `web/src/lib/ipc.ts:678-812`; `web/src/components/FriendsView.tsx:178-269`; `web/src/components/RightRail.tsx:1889, 2112-2141, 2230, 2347`; `web/src/components/settings/TeamSyncSection.tsx:409,462-484`; `web/dev/mock/*` | UI copy and calls: "Add by GitHub username", "Invite by GitHub handle", "Import GitHub org". |

**Relay side** (relay repo, all **[V]**):
- **Routes.** 16 routes, at `server.go:271-288`: `/v1/directory/register`, `/v1/directory/{handle}`, `/v1/account/{register,heartbeat,inbox,devices,visibility,invites}`, `/v1/friends`, `/v1/friends/presence`, `/v1/friends/{account}/devices`, `DELETE /v1/friends/{account}`, `/v1/friends/requests` (POST and GET), and `/accept` and `/reject`.
- **Auth.** Every one of them goes through `requireGitHub` (`server.go:1270-1290`). That reads `X-Ligus-Github-Token`, falling back to `X-Hive-Github-Token` (`server.go:1368`), and never the bearer, per `TestGitHubRoutesNeverUseRelayBearer`. It then calls `GET https://api.github.com/user` (`server.go:1330-1351`), caches the result by SHA-256 of the token (60 s for a valid token, 30 s for an invalid one; `hardening.go:31-32`), and keys the caller as `accountKey(id) = "github:%d"` (`store.go:160`).
- **Tables.** `relay_directory`, `relay_directory_devices`, `relay_accounts`, `relay_account_devices`, `relay_login_index`, `relay_inbox`, `relay_friend_requests` and `relay_friend_edges` (`postgres.go:180-229`). The same data is in the memory store (`memory.go:43-45`) and the JSON snapshot (`snapshot.go:70`).
- **What isn't GitHub-coupled.** The workspace, pairing, tunnel, membership and invite routes key on the `hrt1` `sub`, so they aren't tied to GitHub in code. ligus-auth (`relay/ligusauth/`) has no GitHub code: GitHub appears only as a Dex upstream (`deploy/compose/ligus-auth-dex/dex.yaml:17-23`).
- **Tests.** `github_token_test.go`, `hardening_test.go:52,64,145,172`, `server_test.go:20,193`, `store_test.go`, `postgres_test.go:109,137,190,210`, `snapshot_test.go`.

### 1.3 Messaging: DM ids (8 sites)

| # | Where | What |
|---|---|---|
| M1 | `app/src/lib.rs:13656-13670` **[V]** | `dm_room_for(a,b)` strips `github:`, sorts the two numeric ids, and returns `"dm-" + UUIDv5(DM_NS = "hivedmroompair01", "<lo>\0<hi>").simple()`. Anyone who knows two GitHub ids can compute the room, and from it the workspace id (`room_workspace_id`, `crates/hive-core/src/channel.rs:26-30`). The relay can therefore test any pair of known users for a DM. |
| M2 | `app/src/lib.rs:13709-13800` | `friend_open_dm`. The owner is decided by `"github:<a>" <= "github:<b>"`. |
| M3 | `app/src/lib.rs:13781` | The invite id is `UUIDv5(ws_id, "dm-invite")`, which is derivable too. |
| M4 | `app/src/lib.rs:13311-13330` | `adopt_dm_key` trusts the relay-stamped `from_login`. |
| M5-M8 | `app/src/lib.rs:602-605, 862-873, 13683-13695`; `web/src/lib/ipc.ts:812`; `FriendsView.tsx` | Tests, DTOs and UI. |

### 1.4 Repo work (22 sites)

| # | Where | What |
|---|---|---|
| R1-R2 | `crates/hive-cli/src/repos.rs:97-148, 191` | `parse_repo_spec` accepts only `owner/name` or `https://github.com/…` ("only github.com repositories can be cloned here"), and `git@github.com:`. |
| R3-R7 | `repos.rs:229-559` | `ForkLayout`, `UPSTREAM_REMOTE = "upstream"`, `check_push_target` (refuses pushes to upstream, including `ssh.github.com`), PR branch names, and the agent prompt "opened against upstream as drafts". These are forge-neutral apart from the hostnames. |
| R8-R9 | `crates/hive-cli/src/serve/repos_api.rs:1-24, 200-276` | A `GitHub` trait: `status, list, owners, clone_url, git_auth_args, create_pr, repo_meta`. This is the seed of the `Forge` trait. |
| R10-R14 | `repos_api.rs:278-445` | `GhCli` runs `gh api …`, `gh auth status --hostname github.com`, `gh pr create … --draft`, and the credential helper `credential.https://github.com.helper=!gh auth git-credential` (`:405-414` **[V]**). |
| R15-R20 | `repos_api.rs:489-1430`; `serve/api.rs:471-477` | The repo list (`{local, github:{…}}`), fork plan, clone and fetch jobs, the upstream push URL `DISABLED-push-to-fork-only`, pull, and `POST /v1/chats/:id/pr`. |
| R21 | `crates/hive-cli/src/mcp_config.rs:11-15`; `crates/hive-cli/README.md:140-175`; `deploy/ligus-serve/dashboard-team.toml.example:15,42-51` | **There is no read-only GitHub MCP wrapper in code.** GitHub MCP is generic user config (`npx @modelcontextprotocol/server-github`, with `GITHUB_PERSONAL_ACCESS_TOKEN` inherited from the worker env). The "read-only GitHub MCP" exists only in `docs/design/pr-test-sandbox.md:5`. |
| R22 | `docs/ios-app.md:58, 269-308` | The phone's view of the repo API. |

Repo work uses the **host's `gh` login**. The desktop never shells out to `gh`, and the app's device-flow token isn't used for repo work.

### 1.5 Other (24 sites; not identity, keep as is)

- **Copilot runtime:** `cli_detect.rs:177-185,284-291`, `executor/cli_home.rs:45,67`, `executor/mod.rs:279,300`, `provider/copilot.rs`, `provider/acp.rs:65-71`.
- **GitHub raw-URL fetchers** for skills and vaults: `remote_manifest.rs:7-57`, `hive-core/src/vault.rs:1-161`. These should become forge-neutral raw URLs in 2.x.
- **Guards** (`guard_scan.rs:24`, `guard.rs:18,276,360`, `review.rs:121,142`, `builtin_tools.rs:1240`). Add `.gitlab-ci.yml`, `.forgejo/workflows/`, `.gitea/workflows/`, `.tangled/workflows/` and `.build.yml`/`.builds/` (sr.ht) next to `.github/workflows/`. [PHASE0](phase0-specs.md) D.5 already lists `.gitlab-ci.yml`.
- **Env scrubbing** (`hive-core/src/process_util.rs:45-56,195,218`). Add `GITLAB_TOKEN`, `GITEA_TOKEN`, `FORGEJO_TOKEN`, `SRHT_*` and atproto app passwords.
- **Updater, CI, install and Homebrew:** stay on GitHub releases.

### 1.6 Docs to rewrite

- **Identity and discovery:** `README.md`, `SECURITY.md`, `docs-site/concepts/identity.md`, `concepts/security.md`, `concepts/headless-agents.md`, `deploy/solo.md`, `features/{collaborators,invites,onboarding,settings}.md`, `getting-started/first-launch.md`, `networking/self-host.md`, `ops/{deployment,managed-service,server-side-membership,tiering,troubleshooting}.md`, `reference/{env-vars,invite-payload,settings,wire-protocols}.md`, `security/{index,runbooks,threat-model}.md`, `docs/hive-relay-api.md`, `docs/hive-social-graph-plan.md`, `docs/security-hardening-plan.md`.
  - `docs/hive-relay-api.md:68-75` still says the relay falls back to the bearer. That fallback was removed in relay v0.2.0 (`relay/CHANGELOG.md:44-50`).
- **Repos:** `docs-site/ops/serve-repos.md`, `docs-site/reference/serve-api.md`, `docs/ios-app.md`, `docs/design/pr-test-sandbox.md`.

---

## 2. Identity model

### 2.1 Account (provider-independent)

- **`account_id`**: a random UUIDv4, created with the account key on first launch (`identity_store.rs:160-161`, already the case). It is never derived from anything public.
- **Account key**: Ed25519. Registered on each workspace config log with `AccountKeyRegistered` (unchanged). It certifies devices with `DeviceCertificate` (`hive-device-cert-v1`, unchanged).
- **More devices** join the account through the existing enrollment code (`hivedev1:`, `hive-device-enroll-v1`, `crates/hive-runtime/src/device_enroll.rs:46-51`). They never join by signing into a provider.
  - **[I]** Today two devices that sign into the same GitHub account each have their *own* account key under the same derived `account_id`. Registration is first-wins plus dispute, so the second device's `AccountKeyRegistered` is disputed in shared workspaces. Removing derivation removes this hazard.
- **Passkeys and recovery key**: unchanged ([PHASE0](phase0-specs.md) B.4).
- **Account fingerprint**: hex of the first 16 bytes of SHA-256 of the account signing key. This is the **existing `fk` computation** (`app/src/lib.rs:617-623`). It is shown as eight groups of four hex characters ("safety code") on profiles and invites, so people can compare out of band. No new crypto.
- **Profile**: `display_name` and `avatar_url` (a bounded `data:` URL) on `ActorIdentity`, already synced (`crates/hive-core/src/identity.rs:37-60`). `HumanAccount.handle` (`identity_store.rs:166`) becomes an optional, non-unique nickname. There is no global handle namespace, because Ligus has no server that could own one.
- **Git commit identity** (`git_email`) is a profile setting.
  - It is no longer overwritten by a GitHub email.
  - The Forges page offers "use this forge's noreply address" per connection: on GitHub, `<id>+<login>@users.noreply.github.com` **[I: format widely documented]**.
  - The commit identity can be per repository (Section 5.6).

**Code changes for 2.0** (Section 8, item ID-1):
- Delete the startup switch I15, the `github_login_*` identity path (I13-I14, I16), the churn workaround I17, and `adopt_account_identity`'s GitHub caller.
- Keep `adopt_account_identity` itself only if another provider path needs it. None does in 2.0, because the account id never changes after bootstrap.
- `ACCOUNT_NS` and `DM_NS` stay in the source as frozen wire constants (documented, unused by new code). Deleting them changes no bytes, but keeping them avoids a reader concluding the namespace is free for reuse.

### 2.2 Linked identities: what they are

A **linked identity** is an optional claim, "this Ligus account is also `<subject>` on `<provider>`". It has these properties:
- **Opt-in.** Off by default.
- **Published for each workspace.** It is an event on that workspace's config log, so only that workspace's members see it.
- **Proven on the linking device** and **re-verifiable by every other device**, directly against the forge or DID document. The relay is never involved.
- **Verification is never fold state.** The fold records the claim. Whether it currently verifies is a device-local view result, cached in a device-local table (`identity_link_checks` in `hive.db`). This keeps the fold network-free and env-free (review RC-8, [PHASE0](phase0-specs.md) §0 rule 4).

**Wire (2.0).** Two new `SessionEvent` kinds go on the config log. Tags are camelCase, and an older decoder folds them as `Unknown`, which is a no-op ([PHASE0](phase0-specs.md) §0 rule 3).

```rust
// tags: "identityLinkPublished", "identityLinkRemoved"
IdentityLinkPublished { link: IdentityLink }
IdentityLinkRemoved   { account_id: Uuid, link_id: Uuid }

pub struct IdentityLink {
    pub link_id: Uuid,                 // random v4
    pub account_id: Uuid,              // must equal the signer's certified account
    pub provider: String,              // "github" | "gitlab" | "forgejo" | "gitea" | "sourcehut" | "tangled" | "oidc" | "bitbucket" | "azure"
    pub host: String,                  // "github.com", "gitlab.example.com", "codeberg.org", "meta.sr.ht", "atproto"
    pub subject_id: String,            // stable id: GitHub/GitLab/Forgejo numeric id; atproto DID; sr.ht "~user" (no public numeric id)
    pub handle_at_link: String,        // display only; never used for lookup
    pub method: LinkMethod,            // see 2.3
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub statement: String,             // the exact signed statement bytes (UTF-8), for SshSig
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub proof: String,                 // method-specific: armored SSHSIG; at-uri+cid; attestation
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key_fingerprint: String,       // SHA256:… of the forge key that signed (SshSig), for fast matching
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,         // ≤ issued_at + 2 years; default 1 year
}
pub enum LinkMethod { SshSig, KeyPublished, AtprotoRecord, OidcAttestation, SelfAsserted }
```

**Authorization** (`authz`):
- Any member (Viewer and up) may publish or remove links **for their own account only**. The signer's certified `account_id` must equal `link.account_id`. Agents are denied, under the Agent tier.
- No presence is required: a link grants nothing.
- Admins may remove another member's link with `IdentityLinkRemoved`, for moderation.
- Size cap: 8 KiB serialized, otherwise the event is denied.
- At most 16 live links per account per workspace.

**Removing old claims.** A new `IdentityLinkPublished` with the same `(provider, host, subject_id)` supersedes the old one, in canonical order.

### 2.3 Verification methods by provider

| Provider | Primary method | Where verifiers fetch keys (anonymous) | Stable id | Fallback |
|---|---|---|---|---|
| GitHub | **SshSig** with any key the user has on GitHub (auth or signing) | `GET https://api.github.com/users/{login}/keys` and `/users/{login}/ssh_signing_keys` (both documented as anonymous: [keys](https://docs.github.com/en/rest/users/keys), [signing keys](https://docs.github.com/en/rest/users/ssh-signing-keys)) **[V]**. `github.com/{login}.keys` works too, but is undocumented **[V probe / I contract]**. | numeric id. `GET /user/{id}` gives the current login ([docs](https://docs.github.com/en/rest/users/users)) **[V]** | **KeyPublished**: add the Ligus account public key (as `ssh-ed25519`) to *SSH signing keys*. That grants no auth, because signing keys are a separate list ([docs](https://docs.github.com/en/authentication/managing-commit-signature-verification/about-commit-signature-verification)) **[V]**. |
| GitLab (.com and self-managed) | **SshSig** | `GET {base}/api/v4/users/{id}/keys`, by **id**, "does not require authentication"; includes `usage_type` ([docs](https://docs.gitlab.com/api/user_keys/)) **[V]** | numeric id; OIDC `sub` = id **[V]** | **KeyPublished** with `usage_type = signing` (introduced in 15.7) **[V]** |
| Forgejo, Gitea, Codeberg | **SshSig** | `{base}/{login}.keys`, anonymous. The JSON `/api/v1/users/{u}/keys` **needs a token** on both (`reqToken()`; gitea.com and codeberg.org return 401) **[V]**. | numeric uid. `GET /api/v1/users/search?uid=<id>` is anonymous **[V probe]** | None. Keys have no purpose split, so publishing the account key would grant **push access**. Don't. |
| SourceHut | **SshSig** (or a PGP signature, 2.x) | `https://meta.sr.ht/~{user}.keys` and `~{user}.pgp`, anonymous **[V probe]**. GraphQL needs a token **[V: 401 anonymous]** ([graphql](https://man.sr.ht/graphql.md)). | `~user` only. No anonymous numeric id **[V]** | None. sr.ht SSH keys are auth keys. |
| Tangled (atproto) | **SshSig** with a key from the user's `org.tangled.key.sshKey` records (plus legacy `sh.tangled.publicKey`) | Resolve the DID document (`https://plc.directory/{did}` or `did:web`), take `#atproto_pds`, then `GET {pds}/xrpc/com.atproto.repo.listRecords?repo={did}&collection=org.tangled.key.sshKey`. Anonymous, tested live **[V]**. | **DID**. Handles are mutable ([spec](https://atproto.com/specs/handle)) **[V]** | **AtprotoRecord** (2.x): an `ai.ligus.identity.link` record in the user's PDS, written through atproto OAuth `repo:ai.ligus.identity.link` ([permission spec](https://atproto.com/specs/permission)). The PDS accepts records of unpublished lexicons by default (`putRecord.validate` unset means "validate only for known Lexicons") **[V]**. |
| OIDC through ligus-auth (orgs; GitLab and Forgejo are OIDC providers too) | **OidcAttestation** [2.1] | ligus-auth tenant issuer key, pinned by a workspace rules extension | `uid` (hash of IdP issuer and subject; `relay/token.go` claims) | none |
| Bitbucket Cloud, Azure DevOps | **SelfAsserted** [2.x] | There is no public key list: Bitbucket returns 403 for other users' keys **[V probe]**; Azure DevOps has none **[I]**. | Atlassian `account_id`; Entra object id **[I]** | Shown as "self-asserted", never "verified". |

Why SshSig is the primary method everywhere:
- It needs **no forge write** and **no forge token**.
- It works with `ssh-agent`, hardware keys (`sk-ssh-ed25519`) and 1Password-style agents.
- It is the same procedure on every SSH forge.
- Verifiers only need anonymous reads.
- Developers on these forges already have SSH keys. On Tangled, pushing requires them **[V]**.

### 2.4 The statement (SshSig) and the one new string

The statement is UTF-8 and LF-terminated. Field order is fixed, there is no trailing whitespace, and the text ends with exactly one LF. A verifier rebuilds these bytes from the `IdentityLink` fields and requires byte equality with `link.statement` before checking the signature.

```
ligus-identity-link-v1
account: 6f1c2d3e-…-…                       (account_id, lowercase hyphenated)
account-key: ed25519:<base64url-nopad 32 bytes>
provider: github
host: github.com
subject: 1234567                              (subject_id)
handle: alice                                 (handle_at_link; informational)
issued: 2026-11-14T12:00:00Z                  (RFC 3339, UTC, seconds)
expires: 2027-11-14T12:00:00Z
nonce: <base64url-nopad 16 random bytes>
```

**Signature.** OpenSSH SSHSIG ([PROTOCOL.sshsig](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL.sshsig)) **[V]**, with:
- namespace **`ligus-identity-link-v1@ligus.ai`**;
- hash `sha512`.

It can be produced by:
- `ssh-keygen -Y sign -n ligus-identity-link-v1@ligus.ai -f <key or .pub with agent>` ([ssh-keygen(1)](https://man.openbsd.org/ssh-keygen)) **[V]**;
- the same command run by the user, who pastes the armored result. This is the path for hardware keys and cautious users.

**Why this string is unavoidable.** It is the only new string in this spec.
- SSHSIG requires a non-empty namespace. Its job is to stop a signature made for one purpose being reused for another **[V]**.
- Reusing `git` would let a link signature pass as a git object signature, and the reverse.
- Reusing the generic `file` namespace has a worse failure. Mallory gets Alice to "sign this file" with her GitHub key, where the file is a statement naming *Mallory's* Ligus account key. Mallory then publishes a valid-looking link "Mallory is github:alice".
- The zero-crypto alternative, KeyPublished, is unsafe on Forgejo, Gitea and SourceHut, where it grants push access.

**Format.** The namespace is a length-prefixed SSHSIG field passed on the `ssh-keygen` command line, so it cannot carry the `\0` terminator that [PHASE0](phase0-specs.md) §0 rule 2 asks for. Amend §0.1 with:

| String | Track | Purpose |
|---|---|---|
| `ligus-identity-link-v1@ligus.ai` (SSHSIG namespace; no `\0` by format) | F | Forge-key signature over an identity-link statement |

**KeyPublished.** No statement is needed. The verifier checks that the forge's *signing* key list (GitHub `ssh_signing_keys`; GitLab entries whose `usage_type` is `signing` or `auth_and_signing`) contains `ssh-ed25519 <account key>`. The event is signed by the device (`hive-envelope-v3`), whose certificate chains to that account key. That chain is the Ligus-to-forge direction.

### 2.5 How every device verifies

`crates/hive-runtime/src/identity_links.rs` is a pure core with a pluggable fetcher. It replaces the dormant `identity_verifier.rs` Option C and reuses its OpenSSH Ed25519 parser. Steps:

1. **Fold checks**, which are cheap and offline:
   - the signer's certified `account_id` equals `link.account_id`;
   - `statement` equals `render(link)`, byte for byte;
   - `account-key` equals the account key in force *at the event's position* (`RosterState`);
   - `expires_at > now` and `expires_at - issued_at` is at most 2 years.
2. **Signature check** (offline): the SSHSIG verifies over the statement with the namespace above. Allowed algorithms are `ssh-ed25519`, `sk-ssh-ed25519@openssh.com`, `ecdsa-sha2-nistp256` and `rsa-sha2-512` with 3072 to 16384 bits (see 2.8 for how RSA is verified). **[I]** The RustCrypto `ssh-key` crate covers SshSig verification for these. Confirm sk-key support before choosing it; it is a new dependency for `deny.toml`.
3. **Forge check** (online, direct to the forge, never through the relay):
   1. Resolve `subject_id` to the current handle (GitHub `/user/{id}`, GitLab `/users/{id}`, Forgejo `users/search?uid=`, DID document).
   2. Fetch the key list, by id wherever the API allows: GitLab, and atproto by DID.
   3. Re-resolve the id and require the same handle. This closes the window in which the handle could be renamed and reclaimed between the two calls.
   4. Require the signing key, or for KeyPublished the account key, to be in the list.
4. **Status** (device-local):
   - `verified(checked_at)`;
   - `stale` (the key is no longer listed: rotated or removed on the forge);
   - `unreachable(last_verified_at)`;
   - `expired`;
   - `invalid` (steps 1-2 failed: never shown as linked, and logged);
   - `self-asserted`.
5. **Freshness.**
   - Re-check when a profile is opened, at most once every 24 h per link. Run a background pass weekly.
   - A setting, "Contact forges to check linked identities", is on by default. When off, the UI shows "signature valid, not checked against the forge".

### 2.6 Revocation, expiry, rotation

| Event | Effect |
|---|---|
| User unlinks | `IdentityLinkRemoved` on every workspace where the link was published. Peers hide it at that position. |
| Forge key removed or rotated | Status becomes `stale`. The user re-signs with a current key, which publishes a new link that supersedes the old one. |
| `AccountKeyRotated` | Every link that names the old account key becomes `stale` (step 1, position-scoped). The app prompts the user to re-sign. KeyPublished also needs the new key added on the forge. |
| Device revoked | Links that device published stay valid if they verify, because the statement is account-level. To kill them, the user removes and re-publishes from another device. |
| Expiry | Default 1 year. The UI reminds the user 30 days before. |
| Forge account deleted | The id lookup returns 404, so the status is `stale`. |

### 2.7 Privacy

- Off by default.
- Each link is published to the workspaces the user picks: "Show my Codeberg identity in: [x] Work [ ] Bees".
- Every member of a workspace, including Viewers, can see its links. **[I]** Agents can see them too, through config reads (`ligus_read_*`), so say so in the UI.
- **Forge lookups reveal the viewer to the forge** (IP, timing). Hence the setting above and the 24 h cache.
- Avatars from forges are **copied** into the profile's `data:` URL at link time, only if the user chooses. Ligus never hot-links forge avatar URLs: they act as tracking pixels and reveal views.
- Contact records (Section 3) carry links only if the user also ticks "discoverable by this identity". The record is signed but not encrypted: the relays holding it and anyone with the contact id can read those links, and the opt-in says so.

### 2.8 Hardening from the linked-identities review (2026-11-14)

The adversarial review of the merged linked identities (decision: approve with risks, after the fixes below) tightened these points:

- **Agreed account key.** A device shows a person's links only from workspaces whose log registers the account key it agrees on: its own key for its own account, and the co-member directory's trusted key for anyone else (Section 7, S1 of the identity review). Links are merged by the identity they name (link id, forge, subject, method and signing key), so a reused link id can't replace a genuine link.
- **Handles.** A verified chip shows only a handle the forge confirmed; for atproto, only when `https://<handle>/.well-known/atproto-did` names the DID back. Otherwise it shows the stable id, with the claim in the tooltip. Handles and subject ids may not contain bidi controls or zero-width or other invisible format characters (the fold refuses them); forge-supplied names are stripped, and chips isolate the handle (`<bdi>`).
- **Who gets contacted.** Automatic checks never contact a host the link's owner chose: a PDS other than `bsky.social` or `*.bsky.network`, a `did:web` host, or a handle domain. Those are asked only on an explicit check (linking your own account, or **Check now** on a co-member).
- **Address guard.** The public-address guard also refuses NAT64, 6to4, IPv4-compatible, Teredo, site-local and discard addresses that carry or reach internal hosts.
- **Cache.** The 24-hour cache is keyed by the link, the workspace and the account key. A cached status is shown only after the offline checks pass, only while forge checks are on, and only within the TTL.
- **GitLab published keys** need usage type **Signing**, not GitLab's default **Authentication & Signing** (which grants push).
- **Removal.** Only the account itself, from a non-delegated device, removes its own links (an Admin may still moderate); a delegated host can't remove its owner's links.
- **RSA without the Marvin advisory.** `ssh-key` 0.6's `rsa` feature pulls `rsa` 0.9 (RUSTSEC-2023-0071). The feature is off: `ssh-key` still parses RSA keys and SSHSIG blobs, and Ligus verifies RSA itself with `rsa` 0.10. It rebuilds the PROTOCOL.sshsig signed data the way OpenSSH does (magic, namespace, empty reserved field, `sha512`, SHA-512 of the statement) and checks a PKCS#1 v1.5 SHA-512 signature exactly as long as the modulus. Accepted: `rsa-sha2-512` only (not `rsa-sha2-256` or SHA-1 `ssh-rsa`), moduli of 3072 to 16384 bits, an odd public exponent of at least 3 and at most 2^33 - 1, and an empty reserved field. Other key types still use `ssh-key`'s verifier. No advisory ignore is needed in `deny.toml`.
- **One form per proof.** `ssh-key` 0.6 accepts a length prefix larger than the field it frames, and the SSHSIG version isn't signed, so one signature had many byte forms. Every key type's proof must now be exactly framed (the public key in its canonical encoding) and version 1.

---

## 3. Discovery

### 3.1 What 2.0 uses, in order

1. **Invite link or QR** (`hivews1:`, unchanged, with `fk` and `inv`).
   - The QR is shown on the desktop and scanned on the phone or another desktop. Links travel over any channel the people already trust.
   - The UI shows the inviter's safety code (`fk`) next to the workspace name, so the joiner can compare it out of band.
   - No provider is involved.
2. **Co-members.**
   - Anyone who shares a workspace with you is already discoverable, *with verified keys*. Their `DeviceKeyAgreementPublished` events are signed and chained to their account key ([`directory-trust.md`](directory-trust.md) §"What 2.0 does").
   - Inviting a co-member to another workspace, or opening a DM with one, needs **no directory**. Seal to their verified KA keys and deliver through their contact drop box (3.3), or hand over a link.
3. **Verified forge identities** are shown on member profiles (Section 2). They help people recognise each other ("this is the Alice from Codeberg"). They are not a lookup key in 2.0.
4. **Contacts** (relay v0.3, additive) are a signed, opaque-id card plus a sealed drop box. They replace the GitHub directory, account and friends routes.

**Dropped in 2.0:**
- invite by GitHub handle;
- friend requests by handle;
- GitHub org/team import.

"Invite by verified forge handle" returns in 2.1 through the opt-in contact index (3.4). This needs the owner's decision (D4).

### 3.2 Contact record (client-verifiable, no new strings)

A contact has a **contact id `cid`**: 16 random bytes, base64url, chosen by the account. The record is a bundle of objects that already exist, so no new domain string is needed. This matches the deferred design in [`directory-trust.md`](directory-trust.md):

```json
{ "v": 1,
  "cid": "<cid>",
  "account": { "accountId": "...", "signingPublicKey": "<b64url>" },
  "devices": [ { "certificate": <DeviceCertificate>,            // hive-device-cert-v1
                 "kaPublication": <SessionEventEnvelope> } ],    // DeviceKeyAgreementPublished, hive-envelope-v3
  "profile": <SessionEventEnvelope>,                             // a MemberProfile/identity event signed by a certified device
  "links": [ <SessionEventEnvelope IdentityLinkPublished> ],     // only if the user opted in
  "issuedAt": "...", "expiresAt": "..."  }                       // expiresAt ≤ issuedAt + 30 days; re-published on heartbeat
```

- **Envelope workspace.** The envelopes carry `workspace_id = room_workspace_id("contact-" + cid)`. Verifiers check only the signature chain, not membership.
- **Where `cid` travels:**
  - a hidden `cid` field (skipped when empty) in `hivews1:` invites;
  - an additive `contact` field on `ActorIdentity` (`skip_serializing_if`), so co-members learn each other's drop box;
  - a "contact card" QR, which is just a `hivews1:`-style JSON with only `relay`, `cid` and `fk`. **[Owner D11]**: whether the contact card gets its own prefix (`ligusc1:`, a new wire id) or reuses `hivews1:` with no room.
- **Client checks**, all of which must hold:
  1. The account key fingerprint equals the `fk` the user got out of band (invite or QR), **or** the account is already pinned from a shared workspace.
  2. Each certificate verifies under the account key.
  3. Each envelope verifies under a certified device's key.
  4. The record has not expired, and its `issuedAt` is not older than the newest one seen for that `cid` (anti-rollback). The newest-seen time is kept per `cid` in `hive.db` (`contact_seen`), so it survives restarts.
  5. No device the record lists was revoked on a log this device holds; revoked devices are dropped, and a record with only revoked devices is refused.
  6. A link verifies only by Section 2.5.

  A record that fails (1) is shown as **unverified**, and sealing to it goes through the existing `withUnverifiedRecipientConfirm`.
- **Where records are read (2.0).** Before an invite is dropped into a co-member's box, the desktop reads the record at that `cid` (`contacts::accept_contact_record`, the same checks as `fetch_verified_contact`) and drops only when it verifies, is anchored to the account key the shared logs trust for that person, and names their account. A `cid` that holds someone else's record, a stale record, or none gets nothing; the inviter shares the link instead. The invite is still sealed to the keys from the config logs, never to the record's.
- **Size caps on reads.** A record read is capped at 256 KiB, a drop-box page at 4 MiB and a write or delete answer at 64 KiB; a longer body is refused without being read to the end.

### 3.3 Relay changes (relay v0.3, additive)

**New routes** under the new prefix `/v1/contacts/`. They go through the entitlement gate. Add the prefix to `relayPrefixes` (`relay/extension.go:51-54`); a downstream extension already using `/v1/contacts/` would collide, which is a check for the owner.

| Method | Path | Auth | Body or response |
|---|---|---|---|
| PUT | `/v1/contacts/{cid}` | First write: the body includes `writeKey` (32 random bytes, base64url) and the relay stores SHA-256 of it. Later writes need `X-Ligus-Contact-Key: <writeKey>`. | `{record}` (≤ 64 KiB, opaque to the relay) |
| GET | `/v1/contacts/{cid}` | gate only | `{record, updatedAt}` |
| DELETE | `/v1/contacts/{cid}` | contact key | deletes the record and the drop box |
| POST | `/v1/contacts/{cid}/drop` | gate only; rate-limited per IP and per `cid` (reuse `TestInviteRateLimit` limits) | `{blob}` (≤ 64 KiB, sealed client-side to KA keys with `hive-workspace-key-seal-v1`) |
| GET | `/v1/contacts/{cid}/drop?after=<seq>` | contact key | `{items:[{seq, blob, createdAt}]}`; same row cap as `relay_inbox` (`TestInboxRowCapEvictsOldest`) |

- **Write keys are per relay.** The device keeps one master write key and never sends it. Each relay gets `HKDF-SHA256(master, info = "ligus-contact-write-key-v1\0" + lowercased base URL)`, so a relay that sees its key in clear can't read, acknowledge or overwrite the same contact on another relay. One relay refusing the key doesn't make the device drop its `cid` everywhere: the device replaces the `cid` only when no relay holds the record any more.
- **Reclaim hold.** When a record expires, the relay keeps only its write-key hash and seq counter for 180 days (`HIVE_RELAY_CONTACT_RECLAIM_HOLD_DAYS`). During the hold only the old key can re-create the `cid`, so a lapsed contact id can't be taken over while co-members still deliver to it.
- **Seqs are monotonic.** A drop box's seqs never go backwards, also across expiry and re-creation: a re-created box starts after a time-based base, so each device's `after=` cursor and acknowledgements stay valid. The owner reads every page of the box.
- **Bodies are UTF-8.** The relay stores records and blobs byte-exact and answers `400` to a record or blob that isn't valid UTF-8 (the snapshot would otherwise rewrite it).
- **Flood eviction (R6).** When a box is over its row or byte cap, the relay evicts first the drops it has already served to the owner (returned by an owner-keyed read), oldest first, then the unread drops of the heaviest sender in that box (most bytes, then most rows; on a tie an unverified sender before a verified one). A sender is the relay-token subject when the request has one (counted as verified), otherwise the client address (IPv6 /64 or IPv4), kept only as a salted hash. A flood from a few addresses that know the `cid` evicts its own drops rather than an invite nobody has read yet. Relay test: `TestContactDropFloodKeepsUnreadInvites`.
- **The relay does no cryptography** on these routes. It cannot verify the record and doesn't need to: `cid` is random and unguessable, so squatting someone else's `cid` needs knowing it first, and clients verify everything.
- **Storage:** `relay_contacts(cid PK, write_key_hash, record, updated_at)` and `relay_contact_drop(cid, seq, blob, created_at)`, in the memory, Postgres and snapshot stores.
- **2.0 without v0.3.** A 2.0 desktop against a relay without contacts gets a 404 on `/v1/contacts/`. It then falls back to links and QR only, and says so: "this relay doesn't support contacts".
- **The GitHub routes:**
  - v0.3 registers them only when `LIGUS_RELAY_GITHUB_SOCIAL=1`. The default is off and the flag exists for 1.x clients. The `/v1/directory/` and similar prefixes stay reserved, so nobody can reuse the paths with different semantics.
  - v0.4 deletes the handlers, the `relay_directory*`, `relay_account*`, `relay_login_index`, `relay_inbox` and `relay_friend*` tables, and the GitHub verifier.
  - The 2.0 client never sends `X-Hive-Github-Token` or `X-Ligus-Github-Token`.
- **Relay membership** (`/v1/workspaces/{id}/members`, only when `HIVE_RELAY_MEMBERSHIP=1`) keeps keying on the `hrt1` `sub`. The client stops inventing `github:<id>`:
  - with ligus-auth, the `sub` is `<tenant>:<user>` (`relay/ligusauth/flows.go:953-957`);
  - with a desktop-minted token, it is `ligus:<account_id>`.

  `generate_agent_bootstrap` (D24) mints `sub = "ligus:<agent or box account id>"`.

### 3.4 Opt-in forge index [2.1; needs D4]

- **Routes:** `PUT /v1/contacts/{cid}/index {provider, host, subjectId}` (with the contact key) and `GET /v1/contacts/index/{provider}/{host}/{subjectId}`, which returns at most 8 `cid`s.
- **Verification.** The relay can't verify a claim, so the client verifies each returned record's link against the forge and drops any that don't verify.
- **Visibility.** The relay sees `(forge id, cid)` pairs for people who opted in, and nothing else.
- **Effect.** This brings back "invite @alice on Codeberg" without the relay ever holding a forge token.

### 3.5 What the client checks, summarized

| Source of a key | Trusted when |
|---|---|
| Shared workspace config log | The device is certified and not revoked at the position, and the founder pin holds (existing code) |
| Contact record | The `fk` matches an out-of-band value or a pinned account, and the chain verifies |
| Contact record matched only by forge identity | The link verifies against the forge **and** the user confirms the first contact. Shown as "verified via Codeberg: alice", never as "verified person". |
| Anything else | Unverified. Seal only after explicit confirmation (`withUnverifiedRecipientConfirm`, `web/src/lib/confirm.ts`) |

---

## 4. DMs and rooms

- **The room is random.** It is `"dm-" + uuid_v4().simple()` and is chosen by the initiator. `room_workspace_id(room)` stays as it is (`crates/hive-core/src/channel.rs:26-30`), and its input is now random.
- **The invite id is random.** It replaces `UUIDv5(ws_id, "dm-invite")` (M3).
- **Roles.** The initiator is the founder and owner. `founder_actor_id` is the initiator's account UUID, with `fk` pinned. This replaces the lexicographic `github:` comparison (M2).
- **Delivery.** The `hivews1:` invite, with `dm_account = <peer account_id>` (the field keeps its name; its content becomes a UUID string), is sealed to the peer's verified KA keys and either:
  - posted to the peer's contact drop box (3.3); or
  - shown as a link or QR.
- **The founder signs the invite (R1).** An invite's founder account and `fk` are public, so on their own they prove nothing about who sent it. The founder's device signs the invite's name, relay, room, `dm_account`, founder, `fk` and invite id, plus SHA-256 of the workspace key and of the delivered epoch keys and the signing time, under `ligus-invite-v1\0` ([PHASE0](phase0-specs.md) 0.1). The invite carries the signature as `sig`, with the device certificate and the account key. A reader checks that the key's fingerprint is `fk`, the certificate names the founder and verifies under that key, the device isn't revoked on its logs, and the signature verifies. Only then is the invite labelled "from <name>: signed by their device, and their safety code matches" (`known`). An invite that names a co-member and their `fk` without that signature is labelled "says it's from <name>" (`claimed`), from the drop box or pasted alike. Only the founder's device signs: an invite another Admin issues is `claimed`.
- **Accepting.** The peer accepts a DM only if the invite is signed by the founder's device and its founder account and `fk` match the account they know for that contact (or, for someone they share nothing with, after comparing the safety code). An unsigned DM invite is refused. This replaces trusting the relay-stamped `from_login` (M4).
- **A DM invite for a room already joined is refused,** even from a known co-member: a DM is always a fresh room, and accepting would rewrite that workspace's relay and key.
- **Writes wait for the verified founder.** A joiner pins the founder from the invite but writes nothing into the workspace until the config log holds a seed by the pinned founder, signed by a device certified under the pinned key. The desktop shows this as "Waiting for <name>'s founding to sync", with Send disabled, not as a send failure; it clears on its own.
- **Simultaneous opens.** If both sides open a DM at once, there are two DM workspaces for one pair. The rule is deterministic:
  - keep the one whose `(created_at, room)` is smaller, where `created_at` is the timestamp of the pinned founder's signed seed (no later event, skewed clock or forged DM can move it; a forged DM has none);
  - the other is archived, read-only, and linked from the kept one.

  Both sides compute the same answer. The kept DM is recomputed every time rather than accumulated, so a resolution that changes as logs sync never leaves both archived. **[Owner D12]**
- **Removed.** `dm_room_for` and its tests are deleted from new code paths. Keep the function only if a 1.x DM must be opened; 2.0 starts from scratch, so it is deleted.
- **Rule.** No room, workspace, chat or invite id may be derived from a public identifier: account id, account key, forge id, handle or email. Enforce it with a test that greps for `new_v5(` call sites against an allow-list. The config-log namespace (`channel.rs:16-20`) is allowed, because its input is the workspace id.

---

## 5. Forge abstraction for repo work

### 5.1 Where it lives

- `crates/hive-forge/` is a new crate. It is pure HTTP plus git argv builders, with no Tauri and no axum.
- It is used by `ligus serve` (`crates/hive-cli/src/serve/repos_api.rs`, which replaces the `GitHub` trait at `:200-276`) and later by the desktop.
- The existing fork logic (`repos.rs:229-376`: `ForkLayout`, `check_push_target`, the `DISABLED-push-to-fork-only` push URL) becomes forge-neutral. It takes hosts from the connection instead of the `github.com` literals.

### 5.2 Trait

```rust
pub struct ForgeId(pub String);                // connection id, e.g. "github.com", "gitlab:gitlab.example.com"
pub enum ForgeKind { GitHub, GitLab, Gitea /* also Forgejo, Codeberg */, Tangled, SourceHut, Bitbucket, AzureDevOps }

pub struct RepoRef { pub forge: ForgeId, pub owner: String, pub name: String }   // Tangled owner = DID; sr.ht owner = "~user"
pub struct RepoMeta {
    pub default_branch: String,
    pub parent: Option<RepoRef>,               // fork upstream
    pub is_fork: bool,
    pub private: bool,
    pub clone_https: String, pub clone_ssh: Option<String>,
    pub web_url: String,
    pub can_push: bool,
}
pub enum ChangeRequestKind { PullRequest, MergeRequest, TangledPull, EmailPatchset }
pub struct ChangeRequestDraft { pub base: RepoRef, pub base_branch: String, pub head: RepoRef, pub head_branch: String,
                                pub title: String, pub body: String, pub draft: bool }
pub enum ChangeRequestOutcome {
    Opened { number: String, url: String, draft_supported: bool },
    // Forges without an API to open one (SourceHut) or without credentials:
    Prepared { instructions: String, artifacts: Vec<PathBuf> },   // e.g. format-patch series + send-email argv
}
pub struct ChangeRequest { pub number: String, pub title: String, pub state: CrState, pub draft: bool,
                           pub author: String, pub head: String, pub base: String, pub url: String, pub updated_at: Timestamp }
pub struct ReviewComment { pub author: String, pub body: String, pub path: Option<String>, pub line: Option<u32>,
                           pub created_at: Timestamp, pub resolved: Option<bool> }
pub struct Issue { pub number: String, pub title: String, pub state: String, pub author: String, pub url: String, pub labels: Vec<String> }
pub enum CiState { Pending, Running, Success, Failure, Cancelled, Unknown }
pub struct CiStatus { pub state: CiState, pub checks: Vec<(String /*name*/, CiState, Option<String> /*url*/)> }

#[async_trait]
pub trait Forge: Send + Sync {
    fn kind(&self) -> ForgeKind;
    fn id(&self) -> &ForgeId;
    fn capabilities(&self) -> ForgeCaps;       // {draft_cr, fork_api, ci_status, review_threads, issues, api_open_cr}

    async fn whoami(&self) -> Result<Option<ForgeUser>>;                      // None = anonymous
    async fn list_repos(&self, scope: RepoScope, page: Page) -> Result<Vec<RepoRef>>;
    async fn owners(&self) -> Result<Vec<String>>;                            // user + orgs/groups
    async fn repo(&self, r: &RepoRef) -> Result<RepoMeta>;                    // default branch, parent/upstream, can_push
    async fn find_own_fork(&self, upstream: &RepoRef) -> Result<Option<RepoRef>>;
    async fn create_fork(&self, upstream: &RepoRef, into: Option<&str>) -> Result<RepoRef>;  // only on explicit user action

    fn clone_url(&self, r: &RepoRef, transport: Transport) -> String;
    fn push_remote(&self, layout: &ForkLayout) -> String;                     // always the fork when one exists
    fn git_auth_args(&self) -> Vec<String>;                                   // -c credential.<url>.helper=!ligus forge git-credential <id>

    async fn open_change_request(&self, d: &ChangeRequestDraft) -> Result<ChangeRequestOutcome>;
    async fn list_change_requests(&self, r: &RepoRef, f: CrFilter) -> Result<Vec<ChangeRequest>>;
    async fn change_request(&self, r: &RepoRef, number: &str) -> Result<ChangeRequest>;
    async fn review_comments(&self, r: &RepoRef, number: &str) -> Result<Vec<ReviewComment>>;
    async fn issues(&self, r: &RepoRef, f: IssueFilter) -> Result<Vec<Issue>>;
    async fn issue(&self, r: &RepoRef, number: &str) -> Result<Issue>;
    async fn ci_status(&self, r: &RepoRef, git_ref: &str) -> Result<CiStatus>;

    // identity links (Section 2): anonymous reads only
    async fn resolve_user(&self, subject_id: &str) -> Result<ForgeUser>;      // id -> current handle
    async fn public_keys(&self, subject_id: &str) -> Result<Vec<PublicKeyEntry>>; // {openssh, usage: Auth|Signing|Both|Unknown}
}
```

**Rules:**
- Writes are `create_fork`, `open_change_request` and pushes.
- Writes run only on the host, after a human action or a `ProposalApplied` under rules v2. They are never available to an agent.
- Agents get only the read methods, through the Ligus MCP server (5.5).

### 5.3 Per-forge implementation notes

**GitHub** (github.com and GHES via `base_url`). 2.0.
- **Read and write APIs** ([REST](https://docs.github.com/en/rest)):
  - `GET /repos/{o}/{r}` gives `default_branch`, `parent` and `permissions`;
  - `POST /repos/{o}/{r}/forks`;
  - `POST /repos/{o}/{r}/pulls` with `draft: true`;
  - `GET /pulls`, `/pulls/{n}/comments` and `/reviews`;
  - `GET /issues`;
  - CI: `GET /commits/{ref}/status` (combined) and `/check-runs`.
  - All **[V]**.
- **Fine-grained PATs can't use the Checks API** ([docs](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens)) **[V]**. Fall back to commit statuses and mark "checks unavailable with this token".
- **Auth, in order of preference:**
  1. The **existing `gh` login** on the host. Keep `GhCli` as a credential source: `gh auth token --hostname <host>` for the API, and the `gh auth git-credential` helper.
  2. **Device flow** (supported **[V]**: [docs](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps)) through a **Ligus GitHub App**, whose user-to-server tokens get fine-grained repository permissions. The alternative is an OAuth App, which needs the broad `repo` scope. **[Owner D5]**
     - Since 2026-08-14, OAuth apps can opt into expiring tokens (8 h access, 6-month refresh), the default for new apps ([changelog](https://github.blog/changelog/2026-08-14-multiple-redirect-uris-and-token-refresh-for-oauth-apps)) **[V]**.
     - PKCE has been supported since 2025-07-14 ([changelog](https://github.blog/changelog/2025-07-14-pkce-support-for-oauth-and-github-app-authentication/)) **[V]**.
  3. **A fine-grained PAT**, pasted.
- **Identity links:** anonymous key endpoints (2.3). Watch out: the old username can be claimed by anyone immediately ([docs](https://docs.github.com/en/account-and-profile/setting-up-and-managing-your-personal-account-on-github/managing-user-account-settings/changing-your-github-username)) **[V]**. Pin the numeric id.

**GitLab** (gitlab.com and self-managed, via `base_url`). 2.0.
- **APIs:**
  - `GET /api/v4/projects/:id` gives `default_branch` and `forked_from_project`;
  - `POST /projects/:id/fork`;
  - `POST /projects/:id/merge_requests` with `target_project_id` for cross-fork MRs. There is **no `draft` field on create**, so prefix the title with `Draft:` ([MR API](https://docs.gitlab.com/api/merge_requests/)) **[V/I]**;
  - `/merge_requests/:iid/discussions`;
  - `/issues`;
  - `/merge_requests/:iid/pipelines` and `/repository/commits/:sha/statuses`.
- **Auth:**
  - **Device flow** (`/oauth/authorize_device`; GA in 17.9 on .com and self-managed) **[V]** ([docs](https://docs.gitlab.com/api/oauth2/)). It needs an OAuth application **on each instance**. gitlab.com uses a Ligus-registered app. Self-managed instances need the admin or user to register one (an application id field).
  - **A PAT** with `read_api` + `read_repository`, or `api` + `write_repository` for MRs. Fine-grained PATs are GA in 19.2 **[V]** ([docs](https://docs.gitlab.com/auth/tokens/fine_grained_access_tokens/)).
  - **The `glab` CLI login** as a credential source **[I: `glab auth status`/token helper; check before use]**.
- **Identity links:** `/users/{id}/keys` by id, anonymous, with `usage_type` **[V]**. Names are reclaimable ([docs](https://docs.gitlab.com/user/project/repository/#repository-path-changes)) **[V]**.

**Forgejo, Gitea, Codeberg** (one implementation; `base_url`). 2.0.
- **APIs** (swagger: [gitea.com/api/swagger](https://gitea.com/api/swagger), [codeberg.org/api/swagger](https://codeberg.org/api/swagger)) **[V]**:
  - `GET /api/v1/repos/{o}/{r}` gives `default_branch` and `parent`;
  - `POST …/forks`;
  - `POST …/pulls`. **There is no draft field**: prefix `WIP:`, which is in `WORK_IN_PROGRESS_PREFIXES` **[V]**;
  - `/pulls/{index}/reviews` and `…/comments`;
  - `/issues`;
  - `/commits/{ref}/status` and `/actions/runs`.
- **Auth: a PAT is the 2.0 path.**
  - There is no device grant on Gitea or Forgejo **[V]** ([gitea](https://docs.gitea.com/development/oauth2-provider), [forgejo](https://forgejo.org/docs/latest/user/oauth2-provider/)).
  - On Forgejo, "scopes are not implemented for OAuth2 tokens" **[V]**, so an OAuth token is full user power. A PAT with `read:repository, read:issue` (read) or `write:repository, write:issue` (write) is strictly better. Forgejo's "specific repositories" tokens can narrow it further **[V]**.
  - **Loopback PKCE** works with `http://127.0.0.1` on any port **[V]**, but every instance needs an app registration. Gitea and Forgejo seed built-in public apps with fixed ids (for example, git-credential-oauth). **Ligus must not reuse another app's client id**, because that impersonates the app. So loopback OAuth is 2.x, on instances where the user registers a Ligus app.
  - The `tea` and `fj` CLIs are also credential sources **[I]**.
- **Identity links:** `{base}/{login}.keys` (anonymous), plus `users/search?uid=` **[V]**. Forgejo has an optional `USERNAME_COOLDOWN_PERIOD`, default 0 **[V]**. Pin the uid.

**Tangled** (`tangled.org` appview, any knot). **After 2.0.** Identity links are in 2.0.
- **Model** **[V]** (source `tangled.org/tangled.org/core` @ `2a8a0162`; [blog/forge](https://blog.tangled.org/forge); [upgrade guide](https://tangled.org/docs/upgrading/knots-and-spindles/); [records reference](https://tangled.org/docs/reference/records)):
  - **Lexicons.** v1.20.0 (2026-10-30) moved everything to `org.tangled.*`. The `sh.tangled.*` records are now legacy.
  - **Knots** are git servers (SSH, smart HTTP, LFS). Each one acts as the PDS for every repo it hosts. Each repo has its own `did:plc`, and its state is stored as signed objects under `refs/cobs/…`.
  - **Pulls.** A PR is an `org.tangled.track.ticket` with `targetBranch`, plus immutable `org.tangled.review.patch` records `{ticket, base, head, source?}`, one per **round** ("version"). They are written only through the knot XRPC `org.tangled.review.submitPatch`, which fetches the head from the source repo, so PRs from forks work. The old model was text patches in the author's PDS (`sh.tangled.repo.pull`; [blog/pulls](https://blog.tangled.org/pulls)).
  - **Forks:** `org.tangled.repo.createRepo` with `source`, plus `forkSync`.
  - **CI:** spindles run `.tangled/workflows/*.yml`.
  - **Reads:** the read-only indexer **bobbin** at `api.tangled.org` ([docs](https://tangled.org/docs/api/bobbin/)), and the knot XRPC `org.tangled.git.*` ([docs](https://tangled.org/docs/api/knot/)).
- **Auth:**
  - **SSH push** uses `org.tangled.key.sshKey` records with `usages` containing `auth`.
  - **HTTPS push** uses an atproto inter-service JWT (`lxm=org.tangled.git.receivePack`) **[V]**.
  - **Writes** (submitPatch) need **atproto OAuth**: PAR, DPoP and PKCE are mandatory, the client id is a metadata URL, and native apps use a reverse-domain custom scheme or loopback ([spec](https://atproto.com/specs/oauth)) **[V]**. Or an app password (that's what the third-party tangled-mcp uses) **[V]**.
  - Ligus would host `https://ligus.ai/oauth/atproto-client.json` **[I]**.
- **Why after 2.0:**
  - the lexicons are two weeks old;
  - several methods are still `temp` or `unspecced`;
  - the project shipped breaking wire changes in v1.7, 1.9, 1.13-1.16 and 1.20 **[V]**;
  - an official CLI and SDKs are only announced **[V]**.
- **Plan:**
  - 2.1: read-only (clone over SSH or HTTPS, list tickets and patches through bobbin, spindle status).
  - 2.2: submitPatch through atproto OAuth. Pin to `org.tangled.key.sshKey`, `track.ticket` and `review.patch`, and avoid `temp`/`unspecced` NSIDs.

**SourceHut** (`*.sr.ht`, or self-hosted via `base_url`). **After 2.0** (2.1). Identity links are in 2.0.
- **Model:** a change request is a **patchset emailed to a list** (`git send-email`; [git.sr.ht docs](https://man.sr.ht/git.sr.ht/), [git-send-email.io](https://git-send-email.io)). lists.sr.ht GraphQL can read patchsets and set their status, but **cannot submit one**: there is no mutation for it **[V]**.
- **`open_change_request`** returns `Prepared`. Ligus:
  - runs `git format-patch --cover-letter -v<N> base..head` into the chat's worktree;
  - shows the `git send-email --to=<list>` argv, if the user has configured SMTP in git;
  - or links git.sr.ht's "Prepare a patchset" web flow.

  Ligus never holds SMTP credentials. **[I]**
- **Reads:**
  - `lists.sr.ht` `Patchset{status, version, supersedes}`;
  - `builds.sr.ht` `JobStatus`;
  - `todo.sr.ht` tickets;
  - all through GraphQL at `https://<svc>.sr.ht/query` with a PAT ([oauth](https://man.sr.ht/meta.sr.ht/oauth.md)), with grants such as `git.sr.ht/REPOSITORIES:RO lists.sr.ht/PATCHES:RO builds.sr.ht/JOBS:RO` (the schema spells it `REPOSITORIES`) **[V]**.
- **OAuth is confidential-clients only.** The docs tell native apps to use a PAT **[V]**.
- **Billing:** creating repos, lists or builds needs a paid account ([billing FAQ](https://man.sr.ht/billing-faq.md)) **[V]**.

**Bitbucket Cloud** [2.x, on demand].
- **Auth:** API tokens. App passwords stopped being creatable on 2025-09-09 and stop working on 2026-06-09 ([Atlassian](https://www.atlassian.com/blog/bitbucket/bitbucket-cloud-transitions-to-api-tokens-enhancing-security-with-app-password-deprecation)) **[V]**. No device grant, and PKCE is unclear **[I]**.
- **Draft PRs:** the `pullrequest.draft` boolean **[V]** ([spec](https://dac-static.atlassian.com/cloud/bitbucket/swagger.v3.json)).
- **CI:** commit and PR statuses, Pipelines.

**Azure DevOps** [2.x, on demand].
- **Auth:**
  - Entra device code flow for resource `499b84ac-1321-427f-aa17-267ca6975798` **[V]** ([docs](https://learn.microsoft.com/en-us/azure/devops/integrate/get-started/authentication/entra-oauth)).
  - Legacy Azure DevOps OAuth is deprecated: no new registrations since April 2025 **[V]**.
  - Org-scoped PATs. Global PATs retire on 2027-01-27 ([devblog](https://devblogs.microsoft.com/devops/retirement-of-global-personal-access-tokens-in-azure-devops/)) **[V]**.
- **Draft PRs:** `isDraft` **[I]**.

### 5.4 Credentials: where they live

- **`SecretStore`** ([PHASE0](phase0-specs.md) B.2; `crates/hive-runtime/src/secret_store.rs`): scope `"forge"`, name `<forge id>:<purpose>`, where purpose is `read` or `write`.
  - **macOS:** Keychain, `ThisDeviceOnly`.
  - **Linux desktop:** Secret Service.
  - **Headless serve or worker:** systemd-creds.
  - **Kubernetes:** a mounted Secret.
- **Never** in `settings.json`, `hive.db`, synced events, invites, contact records, logs or the relay. Test: `no_forge_token_leaves_device` (Section 7).
- **Ownership.** The device or box that does git work for a repo binding (the hosts and bindings model) holds that forge's tokens. A phone never holds a forge token. It drives the box (5.7).
- **Refreshes are serialized.** OAuth refreshes are serialized per data dir by an exclusive lock on `.forges.lock`, across serve, the agents' forge tools and the git credential helper. The refresher re-reads the saved expiry first, and the new expiry is saved once while the lock is held, so a rotating GitLab refresh token is never spent twice and nobody is signed out (test `refreshed_expiry_is_persisted_once`).
- **Device sign-in after removal.** Each device sign-in carries a random id; removing the connection ends it. Tokens are saved only while the connection exists and that sign-in is still the pending one (test `device_poll_after_remove_stores_nothing`).
- **Internal hosts.** A connection may be marked `internal` on one device (stored in `forges.toml`, never synced; `POST /v1/forges {internal}`, the Settings > Code hosts checkbox, or `ligus forge add --internal`). Without the mark, a base URL that is or resolves to a loopback, private, link-local (cloud metadata), CGNAT or other non-public address is refused when added, and every request uses a resolver that only returns public addresses (DNS rebinding finds nothing). Caveats: behind `HTTP(S)_PROXY` or `ALL_PROXY` the proxy resolves the target, and git's own pushes look the host up themselves.
- **Agents never read the forge CLIs' logins.** The sandbox denies `~/.config/gh` (except for the Copilot CLI, which may sign in through it), `glab-cli`, `tea` and `hub` alongside `~/.ssh`, `~/.netrc` and `~/.git-credentials`.
- **Tests never reach a real forge.** `hive-forge` refuses real hosts in test builds and whenever `LIGUS_FORGE_DENY_REAL=1`; tests use loopback fakes only.
- **Git transport:**
  - HTTPS uses `-c credential.<base>.helper=` (reset) plus `-c credential.<base>.helper=!ligus forge git-credential <forge-id>`. That reads the write token from the store at push time. It generalises `repos_api.rs:405-414`.
  - SSH uses the user's agent.
- **Separate read and write tokens.** The read token is for MCP tools and listing. The write token is for push and opening change requests. With a single token (the `gh` login), Ligus still exposes only read tools to agents.
- **Agent environment.**
  - Extend the scrub list (`process_util.rs:45-56`) with `GH_TOKEN`, `GITHUB_TOKEN`, `GITLAB_TOKEN`, `GITEA_TOKEN`, `FORGEJO_TOKEN`, `CODEBERG_TOKEN` and `SRHT_TOKEN`, unless the user explicitly passes one to a named MCP server.
  - Copilot's own login (O1-O3) is a runtime credential, not a forge credential. It stays as it is.
- **The app's GitHub device-flow token** (I11-I12, scope `read:user user:email`) is removed. Repo work uses the forge connection's token instead.

### 5.5 MCP: forge data for agents

- **Recommended: read-only forge tools in the Ligus MCP server** ([PHASE0](phase0-specs.md) D.3, in-process, over a bridge socket). They are implemented on `Forge` read methods, so they work the same on every forge:
  - `ligus_forge_list_prs {repo?, state?}`
  - `ligus_forge_read_pr {repo?, number}` (with review comments and CI)
  - `ligus_forge_list_issues`
  - `ligus_forge_read_issue`
  - `ligus_forge_ci_status {ref}`

  `repo` defaults to the chat's bound repo. The binding reads `remote.<name>.url` from git config; `insteadOf` and `pushInsteadOf` rewrites are never followed, so a rewrite can't point the agents' tools at another repo (test `binding_ignores_rewritten_remote`). The token never enters the sandbox, and results are size-capped like the other D.3 reads. The tool-loop runtimes fold them into `ligus_read {what: "forge.pr" …}` to stay within the 16-tool cap (D.3.3).
- **Optional community servers.** These stay as user config, through the existing `mcp_config.rs` path. Recommend running them read-only:
  - **GitHub:** `github/github-mcp-server` with `--read-only` ([repo](https://github.com/github/github-mcp-server)) **[V]**. For remote, the `X-MCP-Readonly` header or `/readonly`. `X-MCP-Lockdown` "is not a security boundary" **[V]**.
  - **GitLab:** the built-in `{base}/api/v4/mcp` (GA in 19.5; Free tier from 19.2; an admin enables it; OAuth DCR or a fine-grained PAT; legacy PATs rejected) **[V]** ([docs](https://docs.gitlab.com/user/gitlab_duo/model_context_protocol/mcp_server/)). It has no read-only flag, so use a read-only fine-grained PAT **[I]**.
  - **Gitea:** `gitea.com/gitea/gitea-mcp` with `-r`/`GITEA_READONLY` **[V]**. Forgejo compatibility is unconfirmed **[I]**.
  - **Forgejo:** community `goern/forgejo-mcp`, with no read-only flag **[I]**.
  - **Tangled:** third-party `tangled-mcp` (reads anonymous; writes with an app password) **[V]**.
  - **SourceHut:** none found.
  - **Azure DevOps:** `microsoft/azure-devops-mcp`, read-only not mentioned **[I]**.
  - **Bitbucket:** Atlassian Rovo MCP **[I]**.
- **Correction to the brief.** There is no "read-only GitHub MCP wrapper" in the 2.0 RC code (R21). The design doc `docs/design/pr-test-sandbox.md:5` is superseded by the tools above.

### 5.6 Commit identity

- Each binding has a commit identity: the profile's `git_email` by default, or the connection's noreply address.
- Commits made on a teammate's behalf keep using `ActorIdentity.git_email` (`identity.rs:44-47`). That's unchanged.

### 5.7 Serve endpoints (phone and desktop)

These are additive. The existing `/v1/repos*` and `/v1/chats/:id/pr` shapes stay, so the shipped phone app keeps working, and the `github` key in `GET /v1/repos` stays as an alias of the GitHub connection.

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/forges` | Connections on this box: `{id, kind, baseUrl, user, authSource: gh\|token\|oauth, scopes, hasWrite}`. Never a token. |
| POST | `/v1/forges` | Add `{kind, baseUrl}`. Admin of the box only. |
| POST | `/v1/forges/{id}/auth/device` | Start the device flow (GitHub, GitLab). Returns `{userCode, verificationUri, expiresIn}`. The phone opens the URI and the box polls. |
| GET | `/v1/forges/{id}/auth/device` | Poll status |
| PUT | `/v1/forges/{id}/token` | Paste a PAT `{purpose: read\|write, token}`. It travels phone → host over the tunnel, which is host-authenticated and opaque to the relay **[I: confirm that `hive-tunnel/1` payloads are E2E between phone and host]**. Stored in the box's `SecretStore`. |
| DELETE | `/v1/forges/{id}` | Removes the connection and its tokens, and ends a pending device sign-in |
| POST | `/v1/forges/{id}/test` | Needs write standing and presence like the other writes. Answers `{ok, user, userId}` or `{ok:false, error}` with a fixed message per error kind, never the host's response body. |
| GET | `/v1/repos?forge=<id>&owner=…` | Multi-forge listing. Without `forge`, today's response plus `forges:[…]` |
| POST | `/v1/repos/clone` | Gains `forge` (default `github.com`) |
| POST | `/v1/chats/:id/pr` | Unchanged route. The response gains `kind` (PR, MR, pull or patchset) and `prepared` for SourceHut. A plain checkout's push (after `pushurl` and `pushInsteadOf`) must go to origin's own repository and host, or the answer is 409 and nothing is pushed (test `non_fork_push_target_must_equal_origin_fetch_repo`). |

- **Presence.** Add, remove, test, token and device routes hand a box write access to code. They need presence ([PHASE0](phase0-specs.md) B.4 serve flow) on a box acting for its owner (delegated) and on any workspace running rules v2. Until the serve passkey flow lands, the accepted proof is a request through the tunnel from a phone the owner marked as theirs; anything else gets 403 `presence_required`. The local `ligus forge` CLI doesn't go through HTTP.
- **Device sign-in status.** Starting a device sign-in, and reading a pending sign-in's user code back (`GET .../auth/device`), work only through the tunnel or on loopback, never on a plain-HTTP network bind: anyone who saw the code could approve it with their own forge account first. **Caveat:** a reverse proxy on the same machine makes every request look like loopback, so don't front `serve` with a local proxy that is reachable from the network.

---

## 6. UI and UX

### 6.1 Onboarding (no GitHub anywhere)

1. **Name and picture.** The picture is optional, cropped locally and stored as a `data:` URL.
2. **Your account on this device.** The account key and this device's certificate are generated silently, and the safety code is shown. "Already use Ligus on another device? Enroll this one" leads to the `hivedev1:` flow.
3. **Passkey.** Register the first passkey and show the recovery words ([PHASE0](phase0-specs.md) B.4.5-B.4.6).
4. **Workspace.** Create one, or join with a link or QR.
5. *(Skippable)* **Connect a code host**: GitHub, GitLab, Codeberg, Forgejo or Gitea (custom URL). This is repo access, not identity. The copy says so: "Ligus doesn't use this to sign you in."

### 6.2 Settings

**Settings → Profile**
- name, avatar, nickname, safety code, commit email;
- **Linked identities**: a list with status chips (Verified / Stale / Self-asserted / Not checked) and the workspaces each is shown in;
- *Link an identity*:
  1. pick the provider;
  2. pick the host (for self-managed);
  3. enter the handle; Ligus resolves the stable id and shows "@alice (id 1234567)";
  4. choose a key: Ligus lists the keys the forge publishes for that account that match keys in your `ssh-agent`, or offers to show the command to run yourself;
  5. sign;
  6. preview the statement;
  7. pick the workspaces;
  8. publish.

  GitHub and GitLab also offer the alternative "Add my Ligus key to the forge as a signing key", with copyable `ssh-ed25519 …` text.
- **Privacy toggles:** "Contact forges to check identities", and "Discoverable by linked identity" (2.1).

**Settings → Code hosts** (replaces the GitHub section)
- For each connection: host, user, auth source (`gh` CLI / device sign-in / token), read and write status, which device or box holds it, Test, and Remove.
- "This token stays on this device. It is never synced and never sent to your relay."

**Settings → Account (removed):** the GitHub client-id paste, and the "git email locked while signed in" rule.

### 6.3 People and DMs

- **People panel:**
  - "Invite by link / QR".
  - "Invite a member of another workspace". This picks from co-members, seals to verified keys, and delivers through their drop box.
  - In 2.1, "Find by verified identity".
- **Friends view becomes Contacts:** people you've exchanged contact cards with or share a workspace with, each with a "Message" action.
- **Removed:** all "GitHub username" copy (D28-D30, `WorkspaceRail.tsx:233`).

### 6.4 Repo picker

- **Sources:** "This device", one per connection (with icon and host), and "Paste a URL". The forge is detected from the host; an unknown host prompts "Which kind of forge is this?" and saves the answer as a connection.
- **Fork-aware flow** (unchanged logic, forge-neutral): "You can't push to upstream: use your fork (found) / create a fork".
- **"Open change request" button** label by forge: "Open draft PR" (GitHub, Bitbucket), "Open draft MR" (GitLab), "Open WIP PR" (Forgejo/Gitea), "Submit pull (round N)" (Tangled), "Prepare patchset" (SourceHut).

### 6.5 Phone

- **Code hosts screen,** through the serve endpoints in 5.7:
  - lists the box's connections;
  - can start a device sign-in (the user approves in Safari, the box gets the token);
  - can paste a token.
- **Repo picker:** gains a forge filter.
- **PR result:** shows the kind and the link.
- **The phone never stores forge tokens.** The phone's own identity (P4/P5) and ligus-auth sign-in (the iOS sign-in note in the iOS app repo) are separate. Neither involves a forge.

### 6.6 Docs pages

**New pages:**
- `concepts/identity.md` (rewritten): account key, devices, passkeys, safety code, linked identities.
- `features/linked-identities.md`: per-forge steps, statuses, privacy.
- `features/code-hosts.md`: connecting each forge, tokens and scopes, `gh`/`glab` reuse, self-managed URLs.
- `features/contacts-and-dms.md`.
- `reference/identity-link-statement.md`: the exact statement and namespace, plus a verification recipe with `ssh-keygen -Y verify`.
- `reference/relay-contacts-api.md`.
- One `forges/<forge>.md` per forge: github, gitlab, forgejo-gitea-codeberg, tangled (2.1), sourcehut (2.1).

**Updated pages:** every page listed in 1.6.

---

## 7. Security analysis (adversarial)

| # | Attack | Outcome under this design | Required test |
|---|---|---|---|
| S1 | **Handle reuse.** Alice renames `alice` to `alice2`; Mallory registers `alice`. | Links pin `subject_id`, the numeric id or DID. Verification resolves id to handle, fetches keys, and re-resolves (2.5 step 3). Mallory's `alice` has a different id, so Alice's link still points at her and the UI shows "now @alice2". Old screenshots saying "@alice" mislead humans; the UI always shows the id beside the handle. SourceHut has no public numeric id, so `~user` reuse is a residual risk. **[I]** Whether sr.ht allows name reuse is unknown. Mark sr.ht links "verified by name". | `link_pins_subject_id_not_handle`, `rename_between_resolve_and_fetch_is_rejected` (TOCTOU), `sourcehut_link_marked_name_bound` |
| S2 | **Statement replay.** Mallory takes Alice's published statement and attaches it to Mallory's account. | The statement names `account` and `account-key`. Both must equal the signer's certified account and its key at that position. | `prop_link_bound_to_account_and_key`, `link_replayed_by_other_account_invalid` |
| S3 | **Cross-protocol signature.** Mallory gets Alice to sign something with her forge key. | The dedicated namespace means only an `ssh-keygen -Y sign -n ligus-identity-link-v1@ligus.ai` invocation yields a valid proof. Ligus signs only statements for its own account key and shows a preview. | `sshsig_wrong_namespace_rejected` (`git`, `file`, empty), vectors from `ssh-keygen` (ed25519, sk-ed25519, ecdsa, rsa-sha2-512) |
| S4 | **Key rotation on the forge** (key removed after compromise). | The status becomes `stale` at the next check. The attacker who holds the old key can make new statements, but they fail the forge check once the key is removed. Between removal and the next check, a cached `verified` can persist for up to 24 h. | `removed_forge_key_turns_stale`, `cache_ttl_bounds_staleness` |
| S5 | **Compromised or malicious forge** (or PDS or PLC operator for Tangled). | The forge can list any key for any user, so it can make links "verify" for any user on it. **A linked identity proves "this forge says so", nothing more.** Ligus never grants membership, roles, or sealing trust from a link alone (3.5). For Tangled, read through `com.atproto.sync.getRecord` proofs and the PLC audit log in 2.x **[I]**. | `link_never_grants_membership_or_seal_trust` (authz plus seal-plan table test) |
| S6 | **The relay as attacker** (contacts). | It can withhold, delay or replay old contact records, and enumerate the cids it has seen. It cannot forge records (signature chain) or read drops (sealed). Rollback is bounded by `expiresAt` ≤ 30 days and the newest-seen `issuedAt` kept locally. It cannot substitute KA keys for co-members, which come from config logs. For first contact, the `fk` from the QR or link anchors the account. | `contact_record_rollback_rejected`, `contact_record_wrong_fk_unverified`, `relay_substituted_ka_key_not_sealed_to` (extends `directory_seal_plan_prefers_verified_devices`) |
| S7 | **The relay sees forge tokens.** | 2.0 never sends one: the header code is deleted. Repo tokens are used only between the host and the forge. | `relay_client_never_sends_forge_headers` (a request recorder asserts that no `X-*-Github-Token` and no `Authorization` other than the relay bearer ever reaches the relay), `no_forge_token_leaves_device` (property test: random token values never appear in `settings.json`, `hive.db`, synced envelopes, invites, contact records or logs) |
| S8 | **Cid squatting or drop spam.** | A cid is 128 random bits, so it can't be squatted without knowing it. Drops are rate-limited and capped, and the owner can rotate the cid. Garbage drops fail to unseal and are discarded. | relay: `TestContactWriteKeyRequired`, `TestContactDropRateLimit`, `TestContactDropReadNeedsKey`, `TestContactRoutesAdditive` (existing `/v1` bodies byte-identical) |
| S9 | **Derivable DM ids** (today's M1). | Rooms and invite ids are random. | `no_room_or_invite_id_derived_from_public_ids` (allow-list test over `new_v5(` call sites), `dm_room_is_random`, `simultaneous_dm_converges` |
| S10 | **Privacy leaks.** | (a) Forge lookups reveal the viewer: a setting plus the cache. (b) Links are seen by every member and by agents: per-workspace opt-in and UI disclosure. (c) Forge avatars are never hot-linked. (d) The forge index (2.1) tells the relay which forge account owns a cid: opt-in, documented. (e) `git_email` is synced as today, with a warning in Settings. | `links_absent_from_unselected_workspaces`, `forge_checks_disabled_makes_no_requests`, `no_remote_avatar_urls_rendered` (web) |
| S11 | **Agent misuse of forge write access.** | The agent tier has no forge write tool. Write tokens never enter the sandbox (scrub list). Pushes and PRs happen only after `ProposalApplied` or an explicit human action. | `agent_tools_have_no_forge_writes`, `sandbox_env_has_no_forge_tokens`, `pr_open_requires_human_or_applied_proposal` |
| S12 | **Device-flow phishing** (user code relayed by an attacker). | Device flow runs only when the user starts it. The box displays the code to the user over the authenticated tunnel. Ligus never accepts a code typed in from elsewhere. GitHub and GitLab show the app name; users are told to check it. | UI copy review; `device_flow_only_user_initiated` |
| S13 | **Self-asserted links** (Bitbucket, Azure) mistaken for verified. | A distinct chip, and never used for discovery. | `self_asserted_never_shown_verified` |
| S14 | **Fold divergence from network state.** | Verification status is device-local and never folded. | Extend `fold_ignores_env` to stub the forge fetcher with differing answers, and assert identical projections. |
| S15 | **Downgrade:** a 2.0 client against a v0.2 relay. | No contacts, so links and QR only. No GitHub header. | `client_degrades_without_contacts_routes` |

**Identity security review, 2026-11-14 (residuals fixed for 2.0).** These come from the review of the identity core, forges and linked identities (numbered R and S there, distinct from the table above).

| # | Attack | Outcome | Test |
|---|---|---|---|
| R1 | A drop or pasted invite names a co-member as founder and copies their public `fk`. | Labelled `known` only with the founder's device signature (Section 4); otherwise "says it's from". An unsigned DM invite is refused. | `forged_drop_not_labelled_known`, `invite_signature_survives_encoding_and_binds_fields` |
| R6 | ~6 IPs that know a `cid` flood its drop box to evict real invites. | Served drops go first, then the heaviest sender's unread drops (3.3). | relay `TestContactDropFloodKeepsUnreadInvites` |
| S1 | An Admin of a workspace they founded registers their own key (and device) under a co-member's account id. | The key trusted for an account is the founder pin of a workspace it founded, else the earliest-pinned log that registers it undisputed. A log registering another key contributes nothing for that account (listed as disputed there); only all logs disputing it is a conflict. | `co_member_key_conflict_from_attacker_founded_log` |
| S2 | A left workspace keeps vouching; a co-member stamps someone else's account with their own drop box. | Leaving drops the founder pin and the redeemed invite. A stamped contact id counts only from an event that verifies under a certified, non-revoked device of that account. | `left_workspace_stops_contributing_co_members`, `forged_stamp_cannot_set_contact` |
| S3 | Bidi overrides, zero-width or control characters make a name read as someone else's. | Stripped wherever a name is shown (desktop DTOs, co-members, invites, guard approvals, serve API, notifications); look-alike names among co-members are flagged. Signed events keep their bytes. | `display_name_bidi_controls_stripped`, `look_alike_co_members_flagged_and_names_cleaned`, `name_and_handle_sanitizers_agree` |
| S4 | Contact records read unchecked; rollback across restarts; revoked devices; unbounded bodies. | 3.2: verified reads before every drop, persisted newest-seen, revoked devices dropped, capped bodies. | `contact_newest_seen_persists_for_anti_rollback`, `contact_record_revoked_device_refused`, `contact_response_body_capped` |

Forge residuals from the same review:

| # | Attack | Outcome | Test |
|---|---|---|---|
| S5 | GitLab rotates the refresh token on use; serve, the agents' tools and the git helper refresh at once and one of them signs everyone out. | One refresh at a time per data dir under a file lock; the new expiry is saved once (5.4). | `refreshed_expiry_is_persisted_once` |
| S6 | An `insteadOf` rewrite in a checkout's git config points the agents' forge binding at another repo. | The binding reads raw `remote.<name>.url` (5.5). | `binding_ignores_rewritten_remote` |
| S7 | A base URL at an internal or metadata address turns the box into an SSRF probe; `/test` echoes the host's body; `/test` and other writes skip presence on some hosts. | Non-public addresses need a device-local `internal` mark and requests resolve to public addresses only; `/test` answers a status and is gated like the other writes; presence on delegated and rules-v2 hosts (5.4, 5.7). | `internal_addresses_need_the_internal_mark`, `forge_test_is_gated_internal_marked_and_status_only`, `forge_changes_need_presence_on_delegated_and_rules_v2_hosts` |
| S8 | A plain checkout's `pushurl` or `pushInsteadOf` sends the PR push (with the write token) to another repo or host. | `check_push_target` on the non-fork path too (5.7). | `non_fork_push_target_must_equal_origin_fetch_repo` |
| S9 | A device sign-in completes after its connection was removed and stores a token nobody sees. | Tokens are saved only while the connection and that sign-in exist (5.4). | `device_poll_after_remove_stores_nothing` |

**Residual risks:**
- A forge, PDS host or PLC operator can lie about keys (S5).
- git looks up a forge host itself, so a hostname re-pointed to an internal address after it was added could still receive the write token through the credential helper. Behind a proxy, the proxy resolves forge hosts.
- Presence on forge routes is the owner's phone through the tunnel, not yet a passkey assertion.
- The co-member key rule is trust on first pin. If the only workspace you share with someone was founded by an attacker who registered a key under their account id, or the attacker's workspace was pinned before any honest one, you see the attacker's key and safety code for them. Comparing the safety code out of band is the check; a founder pin from a signed invite outranks it.
- An invite from an Admin who isn't the founder is always "says it's from".
- An Admin writes a member's roster entry, including its contact id; a stamped contact from the account's own device takes precedence, but until one exists the roster's is used (delivery to a wrong `cid` is caught by the record check in 3.2).
- First contact without an out-of-band `fk` is trust-on-first-use.
- The relay sees metadata (cids, drop timing, sizes).
- SourceHut links are name-bound.
- The KeyPublished method puts the Ligus account public key on GitHub or GitLab, which links the Ligus account to the forge account *publicly*. That is fine for an identity link, but the UI must say so.

---

## 8. Implementation plan

Sizes: S is about 1-3 days, M is about 1-2 weeks, L is longer. "Relay" marks items in the `ligus-ai/relay` repo (v0.3).

| # | Item | 2.0? | Relay? | Size | Depends on |
|---|---|---|---|---|---|
| ID-1 | **Identity decoupling.** Delete I13-I17 and I15's switch, and remove `github.rs` identity use. Onboarding without GitHub. Profile (name, avatar, nickname, safety code). Commit email setting. Remove the GitHub section from Settings. | yes | no | M | none (rebases on Track B passkey onboarding) |
| ID-2 | **Token locality.** Delete `with_github_token` and `authed()`'s header (`relay_client.rs:295-366`), the `LIGUS_RELAY_GITHUB_TOKEN` CLI path, and the app `github_token` secret. Add the `SecretStore` "forge" scope. Extend the env scrub list. Tests S7. | yes | no | S | B2 secret store (landed) |
| ID-3 | **Random DM ids and DM via invite.** Delete `dm_room_for`. Random room and invite id. Founder = initiator. `dm_account` holds a UUID. Accept rule on `fk`. Simultaneous-open rule. Tests S9. | yes | no | M | ID-1 |
| ID-4 | **Remove the GitHub social client paths.** directory, account, friends, invite-by-handle and team import (D1-D31). Relay membership `sub` becomes `ligus:<account>` or the ligus-auth sub. `generate_agent_bootstrap` gets a new sub. | yes | no | M | ID-1 |
| ID-5 | **Co-member invites and DMs** using verified config-log KA keys (reuse `directory_trust::signed_ka_keys`). Delivery by link or QR now, and the drop box when present. | yes | uses RL-1 if present | M | ID-3, ID-4 |
| FG-1 | **`hive-forge` crate and `Forge` trait.** Port `GhCli` as a credential source. Generic `ligus forge git-credential` helper. Forge-neutral `ForkLayout`/`check_push_target`. `parse_repo_spec` for any host. | yes | no | M | ID-2 |
| FG-2 | **GitHub REST implementation** (token, `gh`, device flow through the Ligus GitHub App or OAuth App per D5). Draft PRs, checks fallback. | yes | no | M | FG-1 |
| FG-3 | **GitLab implementation** (.com and self-managed; device flow plus PAT; `Draft:` MRs; pipelines). | yes | no | M | FG-1 |
| FG-4 | **Forgejo/Gitea/Codeberg implementation** (PAT; `WIP:` PRs; statuses and actions). | yes | no | M | FG-1 |
| FG-5 | **Serve endpoints** (5.7), repo picker across forges, phone screens (iOS PR), presence on token and connection changes. | yes | no | M (+ M iOS) | FG-1..4, B3 |
| FG-6 | **Ligus MCP read-only forge tools** (5.5). | yes | no | S-M | D2, FG-1 |
| LK-1 | **Identity links core.** `IdentityLinkPublished`/`Removed` kinds (reserve the slots in the A1 list), authz rule, fold into `RosterState.links`, statement renderer, SSHSIG verification, device-local check cache, verification for GitHub, GitLab, Forgejo/Gitea/Codeberg, **SourceHut and Tangled** (key fetchers only). Tests S1-S5, S10, S13-S14. | yes (owner D1, D2) | no | M-L | A2 authz, FG-1 (key fetchers) |
| LK-2 | **Link UI** (Settings flow, agent key matching, chips, per-workspace publish). | yes | no | M | LK-1 |
| RL-1 | **Relay contacts routes** (3.3) in memory, Postgres and snapshot stores. Relay tests S8. `/v1/contacts/` prefix reserved. | client tolerant in 2.0; relay v0.3 | **yes** | M | none |
| RL-2 | **Deprecate the GitHub social routes** (`LIGUS_RELAY_GITHUB_SOCIAL`, default off in v0.3; delete in v0.4). README and CHANGELOG. | relay v0.3 | **yes** | S | none |
| CT-1 | **Client contacts:** publish the record, heartbeat, drop box poll, contact card QR, anti-rollback store. | yes if RL-1 ships with 2.0, else 2.1 | uses RL-1 | M | RL-1, ID-5 |
| DOC-1 | Docs (6.6). | yes | n/a | M | all of the above |
| LK-3 | **ligus-auth `OidcAttestation`.** `POST /t/{tenant}/attest` signs `{uid, display claims, account key, device id}` with the issuer key. Needs a **new** domain string (`ligus-auth-identity-v1\0`). Workspace rules extension pins tenant issuer keys. | 2.1 | ligus-auth | M | Track E |
| CT-2 | **Opt-in forge index** and "find by verified identity". | 2.1 | **yes** | S + M | RL-1, LK-1 |
| FG-7 | **SourceHut:** read (GraphQL with PAT), `Prepared` patchsets, builds status. | 2.1 | no | M | FG-1 |
| FG-8 | **Tangled read** (bobbin, knot XRPC, clone, spindle status). | 2.1 | no | M | FG-1 |
| FG-9 | **Tangled write** (atproto OAuth client: PAR + DPoP + client metadata on ligus.ai; `submitPatch`; forks). Plus the AtprotoRecord link method. | 2.2 | no | L | FG-8 |
| FG-10 | Bitbucket and Azure DevOps (token only, self-asserted links). | 2.x on demand | no | M each | FG-1 |
| FG-11 | Loopback-PKCE OAuth for Forgejo/Gitea instances where the user registers an app. | 2.x | no | S | FG-4 |

**Dependency order** (merge queue, after [PHASE0](phase0-specs.md)'s A2 and B2):

```
ID-2 → ID-1 → ID-4 → ID-3 → ID-5
ID-2 → FG-1 → {FG-2, FG-3, FG-4} → FG-5 → FG-6
A2 + FG-1 → LK-1 → LK-2
RL-1, RL-2 (relay repo, any time) → CT-1
DOC-1 last
```

**Conflict hot spots.** `app/src/lib.rs` is touched by ID-1, ID-3, ID-4 and ID-5. Keep the edits as calls into new `hive-runtime` modules (`identity_links.rs`, `contacts.rs`, `dm.rs`). `events.rs` gets the two new kinds in the A1 reserved list.

**Proof tests at the cut:**
- S1-S4, S7, S9, S10, S11, S13, S14, S15 (client);
- S8 and `TestContactRoutesAdditive` (relay, if v0.3 ships);
- `signature_contexts_are_frozen` still green, plus the new namespace pinned in a sibling test, `link_namespace_is_frozen`.

---

## 9. Owner decisions needed

| # | Decision | Recommendation |
|---|---|---|
| D1 | Add the SSHSIG namespace `ligus-identity-link-v1@ligus.ai`, amending [PHASE0](phase0-specs.md) §0.1 as a namespace with no `\0` | **Yes.** It is the only safe universal method (2.4). |
| D2 | Add `identityLinkPublished` and `identityLinkRemoved` to 2.0, so their wire is frozen at the cut | **Yes.** It is cheap, and avoids a 2.0/2.1 decoding gap. |
| D3 | Relay v0.3 with `/v1/contacts/*`, shipped alongside 2.0? | **Yes** if the relay can be released with 2.0. Otherwise 2.0 runs on links, QR and co-members, and contacts arrive in 2.1. |
| D4 | Drop invite-by-handle and friends in 2.0, and bring back "find by verified identity" through the opt-in index in 2.1 | **Yes.** 2.0 starts from scratch, and the old feature relied on the relay seeing GitHub tokens. |
| D5 | GitHub repo auth: register a **Ligus GitHub App** (fine-grained, device flow, expiring tokens) instead of an OAuth App (`repo` scope). Keep `gh` reuse either way. | **The GitHub App.** It is the owner's action to register; it needs the client id baked as today. |
| D6 | Register a Ligus OAuth app on gitlab.com for the device flow. Self-managed uses a PAT or an admin-registered app. | Yes |
| D7 | Forgejo/Gitea/Codeberg: PAT only in 2.0 (no device grant; Forgejo OAuth has no scopes) | Yes |
| D8 | Tangled and SourceHut repo work after 2.0 (2.1 reads, 2.2 Tangled writes). Their identity links are in 2.0. | Yes |
| D9 | Bitbucket and Azure DevOps on demand only | Yes |
| D10 | Ligus-provided read-only forge tools in the Ligus MCP server as the default. Community servers are allowed as user config. | Yes |
| D11 | Contact card: a new `ligusc1:` prefix, or reuse `hivews1:` with an empty room | Reuse `hivews1:` (no new wire id) unless the UI needs a distinct prefix |
| D12 | Simultaneous-DM rule: keep the smaller `(created_at, room)` and archive the other | Yes |
| D13 | Default of "Contact forges to check linked identities" | On, with the 24 h cache |
| D14 | Relay GitHub routes: off by default in v0.3, deleted in v0.4 | Yes. The hosted relay deployment must redeploy the open base, never the gated binary. |
| D15 | KeyPublished method on GitHub and GitLab (puts the account key publicly on the forge) | Offer it as the fallback only, with the disclosure copy |

## 10. Corrections to the brief

- **The brief says "Read-only GitHub MCP wrapper usage".** There is none in code; it exists only as a design note (R21).
- **The brief names Tangled's `sh.tangled.*` lexicons.** They are legacy since 2026-10-30. The current namespace is `org.tangled.*`, and repo data lives on the knot.
- **The brief puts Tangled at tangled.sh.** It moved to tangled.org around October 2025; tangled.sh redirects. Some infrastructure (`knot1.tangled.sh`) still uses `.sh` **[V]**.
- **Bitbucket app passwords** are already dead (2026-06-09). Only API tokens remain.
