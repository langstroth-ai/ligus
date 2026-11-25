# Identity and data model v2 (design note)

Status: design, not implemented; owner decisions pending (section 15).
Target: Ligus 0.4 for the parts marked 0.4 in section 14, later releases for
the rest. Surfaces: the desktop, `lt` (ligus-term), the `ligus` CLI, `ligus
serve` and `ligus worker`, the phone apps, and one additive relay route
(`langstroth-relay`). Ligus is short for *ligustica*, the Italian honeybee.

Citations are `file:line` into this repository at `origin/main` `58cb0e067`,
into ligus-term at `3a4bf58` (prefixed `lt:`) and into langstroth-relay at
`3c6a742` (prefixed `relay:`). When this note and the code disagree, the code
wins.

The direction is the one the owner approved: **one source of truth per
concept**. Every "who" is a typed id, every name and picture is resolved at
render time from one signed record, and every device knows which person it is
from the log, with a local file only as a cache.

## 1. What is wrong today (code-verified)

**Names and pictures have no authority.**

- A rename or a new avatar changes `identity.json` and the live author only
  (`app/src/lib.rs:12001-12011`, `set_avatar_sync` `:12065-12069`;
  `ChatService::set_author_display_name` and `set_author_avatar`,
  `crates/ligus-runtime/src/chat_service.rs:597-613`). No event is written.
  The roster keeps the name and picture from `MemberAdded` forever, because
  `ensure_self_member` matches by id or account and never adds again
  (`chat_service.rs:1499-1506`).
- Three authors for one person: the desktop stamps its local name, a phone
  action stamps the roster record (`phone_person`,
  `crates/ligus-runtime/src/chat_service/phone_sign.rs:77-99`), and a linked
  computer stamps the snapshot in `computer-link.json` taken at approval time
  (`LinkState::record_link`, `crates/ligus-runtime/src/computer_link.rs:545-566`).
- Content freezes names: `ChatMessage.author` is a `String`
  (`crates/ligus-core/src/chat.rs:83`), `MessageReaction.actor_display_name`
  (`chat.rs:64`), `HumanAnswered.by` is "Display name of who answered"
  (`crates/ligus-core/src/flow/mod.rs:494-496`), and attachments carry
  `author_name` (`crates/ligus-core/src/attachment.rs:246-260`).
- Every human event stamps the whole `ActorIdentity`, avatar included, up to
  about 96 KB (`validate_avatar_data_url`, `app/src/lib.rs:12017-12029`;
  the stamp is `self.author.clone()`, `chat_service.rs:867-871`). Message
  avatars are read from the message's own `actor_identity`
  (`app/src/lib.rs:2546`). The prefix check accepts any `data:image/`,
  including SVG.
- The co-member directory takes a person's name from whichever workspace it
  iterates first (`crates/ligus-runtime/src/directory_trust.rs:301-307`), and
  the desktop's own profile prefers the local file over the live author
  (`app/src/people.rs:121-125`).

**Device names never sync.** My devices names a device from its host record's
label, else a phone label this desktop remembers in `settings.json`, else
"Device" and eight hex digits (`device_label`, `app/src/lib.rs:5410-5417`;
`phone_labels`, `:502`). A device that never registered a host (a computer
approved but never redeemed, a phone) is "Device 376b5d72" everywhere else.

**"Which person am I" has two answers.**

- `LinkState.person`, a local file, picks the author. The CLI honours it only
  in the workspaces it was linked in (`crates/ligus-cli/src/main.rs:353-365`).
  lt acts as the person in every workspace, linked there or not
  (`lt:src/backend/real/mod.rs` `open_core`, the `link_state(..).person`
  match).
- `device_enroll::standing()` reads the certificate fold. Among members whose
  account certified this device it picks the one matching `prefer` by actor
  id **or by display name, ignoring case**, else the lowest account id
  (`crates/ligus-runtime/src/device_enroll.rs:222-258`).
- A linked install keeps its own pre-link account. `ligus whoami` and
  `ligus status` print it, and any code that reads `stored.actor()` or lt's
  `Core.identity` instead of the author acts as that phantom member.

**Any member can disturb someone else's device with a certificate.** A
`DeviceCertificateAdded` is signed by the certifying account's key over any
device public key (`DeviceCertificate::issue`, `crates/ligus-core/src/crypto.rs:492`).
Its floor is Contributor, and it needs presence only when the certifying
account holds a passkey (`crates/ligus-core/src/authz/rules.rs:33-37`). Device
public keys are public: they are in every certificate on the log and in every
link request. Today a foreign certificate:

1. **stops a box**: `standing()` returns `OwnerDevice(<the certifier>)` and
   `ligus serve` blocks all writes (`crates/ligus-cli/src/serve/mod.rs:579-585`);
2. **breaks a phone**: `phone_person` needs exactly one member among the
   phone's holders, and a phone never vouches, so every certifier holds it
   (`phone_sign.rs:86-95`; `crates/ligus-core/src/authz/timeline.rs:59-63`);
3. **blocks a link**: `issue_computer_link` refuses a device any other
   account certified, and `check_computer_link` refuses when "this device is
   certified for someone else" (`crates/ligus-runtime/src/chat_service/computer_link.rs:121-131,183-185`);
4. could, through the display-name match above, **redirect** which person a
   device picks;
5. **borrows a person**: `DeviceLinks::from_config` maps any Admin whose
   account certified another member's host device to that member
   (`crates/ligus-core/src/device_link.rs:47-72`), so `links.person(Mallory)`
   becomes Ana and Mallory's requests count as Ana's own turns on her private
   agents (`crates/ligus-runtime/src/agent_access.rs:155-170`);
6. **inherits a former member**: `retired_aliases` gives a removed member's
   history and agents to any current member who certifies a device that once
   signed as them (`device_enroll.rs:266-302`);
7. **keeps a removed member reading**: a removed member's device stays a
   recipient of later workspace keys while any current member's certificate
   names it (`rotation_recipients`, `timeline.rs:247-258`);
8. **makes a phone unrevocable by its person**: a phone never vouches
   (`timeline.rs:358`), so every certifier holds it, `account_owns_device`
   is false for its person (`:199-205`), and a revoke must go strictly down
   the ranks over every holder (`crates/ligus-core/src/authz/mod.rs:1369-1405`).
   If the certifier ranks at or above the person, the person can't revoke
   their own stolen phone.

Part of the fold copes: a device "holds" only the accounts it vouched for
with its own signature when several accounts certified it
(`RosterTracker::vouched`, `timeline.rs:342-380`; `account_holds_device`,
`:188`). Everything that reads bare certification instead of holders (the
local "who am I" code, items 5 to 7) or that has no vouch to read (phones,
item 8) is exposed.

**Linking leaves debris.**

- The computer's previous member is only shown (`previous_member`,
  `chat_service/computer_link.rs:56-62`); it stays in the roster, so People
  shows two people for one human. A Contributor can't remove it: removal has
  an Admin floor and always needs presence (`crates/ligus-core/src/authorization.rs:207`,
  `rules.rs:18-20`).
- A computer approved on the desktop but never redeemed keeps a valid
  certificate forever. The owner has one ("Device f3b686ca").
- Host records of removed members stay in the host list.

**Ids with two meanings.**

- "Workspace" is both the room string and `UUIDv5(room)`
  (`crates/ligus-core/src/channel.rs:26-30`). `sync_cursor`, `pushed` and
  `parked_bodies` key by the room string (`crates/ligus-runtime/src/event_store.rs:674-689`),
  so a cursor from one relay is reused against another relay that has the
  same room name.
- Four ids for "who": `ActorIdentity.id` (a string), `account_id`
  (`Option<Uuid>`), `signer_device_id`, and `WorkspaceMember.id` beside
  `actor.id` (`crates/ligus-core/src/identity.rs:44-50,117-131`); the authz
  signer uses `Uuid::nil()` for a missing account.
- An agent on a message is `responder_id`, `related_agent_id`,
  `agent_persona_id` or the author name (`chat.rs:91-107`); the default agent
  has no id.
- The signed `sequence` is drafted from the local row place
  (`next_row_sequence`, `event_store.rs:1114-1125`); `UNIQUE(session_id,
  sequence)` spans workspaces (`:589`). (`equivocations` is
  `UNIQUE(event_id, envelope_json)` (`:756-765`); the envelope JSON names
  its workspace, so that key is already per workspace.)

**Storage.**

- Three connection-list formats: desktop `settings.json`, `workspaces.json`
  (`crates/ligus-runtime/src/connection_settings.rs`) and the CLI's
  single-room `config.toml` (`crates/ligus-cli/src/main.rs:97-152`).
- Five code-folder bindings: desktop `workspace_dirs` and legacy
  `workspace_root`, `repo_checkouts` and `chat_checkouts`
  (`crates/ligus-runtime/src/event_store/repo_tables.rs:16-33`), the CLI's
  `repo-bindings.toml`, and lt's `checkouts` in `lt-state.json`.
- Secrets in SQLite: `join_redeem_keys` and `issued_invite_keys` hold seeds
  (`event_store.rs:636-649`), and the event-id migration's backups copy them.
  `WorkspaceCredential::Inline` puts a model API key on the synced config log
  where every member, Viewers included, can read it
  (`crates/ligus-core/src/workspace_runtime.rs:10,29`).
- No `PRAGMA user_version`, so a downgrade can't be detected.
- Relay blobs expire after 30 days while the log keeps the attachment forever
  (`relay:internal/relay/blobs.go:75`), so files silently rot.
- The relay's own roster (`LANGSTROTH_RELAY_MEMBERSHIP=1`,
  `relay:internal/relay/members.go`) is a second membership model keyed by
  token subject that no client reads (`crates/ligus-runtime/src/relay_client.rs:247-257`).

## 2. One source of truth per concept

| Concept | Authority | Caches (never consulted when the authority answers) | Rendered by |
|---|---|---|---|
| Who wrote an event | the envelope's `actor_stamp`, bound by authz to the member and the signing device | payload "who" fields (hints) | `Who` (section 4) |
| A person's name and picture | the newest `ProfileUpdated` for the account on the workspace's config log | roster `actor.display_name` and `avatar_url` (from `MemberAdded`), stamps, `LinkState`, `PhoneBinding`, `identity.json` | `name_of(Who)` |
| A device's name | the newest `DeviceLabelSet` for (device, account) | host label, `phone_labels`, `phones.json` | `device_name(device, account)` |
| Which person this device is, per workspace | the certificate fold plus this device's own signatures (section 7) | `LinkState.links` | `me(workspace)` |
| Role, kind, membership | the config-log roster (unchanged) | `PhoneBinding.role` | unchanged |
| A workspace | `workspace_id` (uuid) | the room string, only as a relay address | everywhere |
| A local code folder | `repo_checkouts` and `chat_checkouts` in `ligus.db` | none after migration | section 10.2 |
| A secret | the secret store | none | never |

## 3. Compatibility model

Every change falls in one of three classes. The class decides how it ships.

**Class R, render-only events.** New event kinds whose fold output only feeds
display: `ProfileUpdated` and `DeviceLabelSet`. Why every build agrees on
everything else, as verified in the code:

- **Storage.** A 0.3 build decodes them as `SessionEvent::Unknown`
  (`crates/ligus-core/src/events.rs:570-577`), stores the envelope byte for
  byte and re-pushes it unchanged (`crates/ligus-core/src/envelope_wire.rs:208`;
  test at `crates/ligus-runtime/src/sync_engine.rs:1876-1902`).
- **No-op fold.** `Unknown` applies nothing (`events.rs:1261`). Its Owner
  floor (`authorization.rs:343`) is for authoring only; in the fold a
  config-log `Unknown` from a member falls through to Allow
  (`authz/mod.rs:986`), from a phone or a non-member it is denied. Either
  way only the verdict record and the session's `updated_at` (`events.rs:2326`)
  can differ from a 0.4 build that denies a malformed one.
- **Payload-agnostic structure.** The lamport rule reads only ids and
  lamports (`crates/ligus-core/src/lamport.rs:159-200`), chains and
  watermarks only `prev` and the signer (`crates/ligus-core/src/watermark.rs:68-120`),
  copies only the preimage (`crates/ligus-core/src/copies.rs:55-57`), and
  `forks_chain` ignores the kind (`event_store.rs:1029-1045`). A later event
  naming a class R event as `log_head` or `prev` is judged alike everywhere.
- **Verdict-independent vouching.** `RosterTracker::vouched` counts any
  envelope a device signs for an account, whatever its kind or verdict, after
  checking its signature (`timeline.rs:342-380`). So a `DeviceLabelSet` that
  a device signs (7.6) changes who holds that device **identically** on 0.3
  and 0.4. This is the one place a class R event reaches access, and it is
  consistent by construction.

Two obligations follow. 0.4 must never refuse a class R kind **at ingest**
(that would make it a lamport witness on 0.4 and a row on 0.3); malformed
content is denied in the fold only. And every new `ChatSession` field is
`skip_serializing_if` empty, because snapshots are signed over that form
(`crates/ligus-core/src/session.rs:295-304`). They need no rules change and
ship in 0.4. An older peer keeps showing the stale roster name, which is the
behaviour today.

**Class L, local-only.** How a device picks its own person, local storage,
migrations, render-time resolution. Nothing on the wire changes, so there is
nothing for other peers to disagree about.

**Class G, rule-gated.** Anything that changes what the fold allows or denies:
`MemberRetired` as a real removal, refusing inline credentials, the
`WorkspaceMember.id` invariant, inert hosts of removed members. These are
written to the fold behind **rules version 3** (`RULES_SUPPORTED` and the
read-only switch, `crates/ligus-core/src/authz/mod.rs:64-72,183-186`). Until
an Owner sets `WorkspaceRulesSet{version: 3}` (Owner floor, presence), a 0.4
build folds exactly as 0.3 does, and events whose effect is gated are inert
on every build (a 0.4 build may use them for display only, as class R). Once
an Owner sets version 3, an older build makes that workspace read-only on its
device, the existing fail-closed path, and shows "update Ligus". So no two
builds that both write ever fold a gated event differently.

Rules for every class:

- **No wire id is renamed.** New kinds, new optional fields (`serde(default)`,
  skipped when empty) and new signature contexts only. Existing contexts
  (`hive-device-cert-v1`, `ligus-link-request-v1`, ...) stay byte for byte.
- An additive field on an existing event is a **hint** unless the event is
  under class G. A 0.4 fold never lets a hint change an authz outcome under
  rules 2.
- Every migration follows `migrate_event_ids_per_workspace`: probe, back up
  with `VACUUM INTO`, rewrite in one transaction, remove a partial backup on
  failure (`event_store.rs:207-214,320-345`). Section 11.

## 4. "Who" is a typed id

### 4.1 The `Who` type

```rust
// crates/ligus-core/src/who.rs (new, not on the wire)
pub enum Who {
    Account(Uuid),   // a person or a service member: their account id
    Agent(Uuid),     // a workspace agent: its agent id
    Legacy(String),  // an actor id with no account (pre-account content)
}
impl ActorIdentity { pub fn who(&self) -> Who }   // Agent kind -> Agent(id), account_id -> Account, else Legacy(id)
```

- The authority for an event's author is its `actor_stamp.actor` after authz:
  the stamp's account must be the member's, and the signing device must be
  certified for it (`authz/mod.rs:389-407`). The fold copies `Who` from the
  stamp into each folded record that has a "who" (messages, reactions, votes,
  refinements, answers, attachments, asks).
- `Signer` in authz takes `Who` instead of `Uuid::nil()` for a missing
  account. Comparisons of people go through `Who`, never through a name and
  never through `actor.id` alone.
- `WorkspaceMember.id` and `actor.id` are the same string for every member
  written by current code. Class G (rules 3): `MemberAdded` is denied when
  they differ, or when a human or service member's `actor.id` isn't its
  `account_id` as a string. Older members with a mismatch keep working
  through an alias map built at fold time (`DeviceLinks`,
  `crates/ligus-core/src/device_link.rs:47-111`, extended).

### 4.2 Payload fields

Writers in 0.4 keep every legacy field, so older peers render as today, and
add typed hints:

| Record | Legacy field (kept) | New hint (additive) | Authority |
|---|---|---|---|
| `ChatMessage` | `author: String`, `actor_identity` | none needed | stamp |
| `MessageReaction` | `actor_id`, `actor_display_name` | `actorAccount?: Uuid` | stamp |
| `ProposalApproval` | `actor_id` (already must equal the author, `authz/mod.rs:746-749`) | none | stamp |
| `ProposalRefinement` | `actor_id` | none | stamp |
| `HumanAnswered` | `by: String` (name) | `byAccount?: Uuid` | stamp (already a Human stamp only, `authz/mod.rs:294`) |
| `SharedAttachment` | `author`, `author_name`, `author_account?` | none | stamp |

- A fold under rules 2 ignores a hint that disagrees with the stamp, and the
  renderer uses the stamp. Under rules 3 a mismatch is denied (class G).
- **Not in phone-drafted events.** A phone signs only preimages that
  re-encode exactly with the fields it knows (`crates/ligus-core/src/phone_action.rs:183-196,270-291`),
  so a 0.4 box drafting `HumanAnswered{byAccount}` for a 0.3 phone would be
  refused. A box adds hints to an event it drafts for a phone only when that
  phone advertises it reads them (a `hints: 1` capability in its requests).
- The desktop's `ChatMessage.actor_identity` stops carrying `avatar_url` and
  `key_agreement_public` on new messages, and the stamp's actor stops
  carrying `avatar_url` (decision O2). Pictures come from `ProfileUpdated`.
  A 0.3 peer then shows initials for new messages from 0.4 senders and keeps
  the roster picture in People.

### 4.3 Rendering

```
name_of(who, ws):
  Account(a): newest ProfileUpdated(a).displayName in ws
              ?? roster member with account a .display_name
              ?? the frozen payload name, marked "(former member)" when a isn't a member
  Agent(id):  roster agent name ?? "Ligus" for the default agent
  Legacy(s):  roster member with actor.id == s ?? the frozen payload name
picture_of(who, ws): same chain with the avatar, else initials on the account colour
```

- **Legacy content** (only names): signed events since 0.1 all carry a stamp
  with an account, so the stamp resolves them. Payload-only names remain for
  `Legacy` actors, for messages whose stamp a device couldn't keep (a sealed
  epoch it can't open), and for the `reply_to_message_id: None` fallback that
  correlates turns by author name (`chat.rs:100-106`). Those render the
  frozen name in a muted style and are never used to decide "is this me" or
  any access. The author-name fallback for turn correlation stays as is for
  legacy turns only.
- **Agent context and exports** (prompts, `ligus.facts.v1`, MCP tools)
  resolve names at build time with the same function.
- Mentions keep their text forms (`@Name#N`, `@handle`); typed mention ids are
  later (section 14).

## 5. `ProfileUpdated`

### 5.1 Shape

```jsonc
// config log, kind "profileUpdated"
{
  "kind": "profileUpdated",
  "profile": {
    "accountId": "6c1e…",          // must equal the stamp's account
    "displayName": "Ana Ruiz",     // optional: absent = unchanged
    "avatar": {                    // optional: absent = unchanged
      "contentType": "image/webp", // image/png | image/jpeg | image/webp
      "data": "<base64, decoded <= 16 KiB>",
      "sha256": "<hex of the decoded bytes>"
    },
    "clearAvatar": false,          // true = remove the picture (avatar must be absent)
    "issuedAtMs": 1764000000000    // author clock; only for the cross-workspace directory
  }
}
```

### 5.2 Who may write it

Allowed when all hold, judged against the running roster in canonical order
like every config-log event (`authz/mod.rs:8-11`):

1. The stamp's actor is a **Human or Service** member (current, or removed
   with the event before its cut's watermark), and `profile.accountId` is the
   stamp's `account_id`.
2. The signing device is positively certified for that account at this
   position (the fold checks it itself, not only ingest: the generic author
   check denies only a known device, `authz/mod.rs:401-404`), not revoked,
   **not delegated and not a phone**. A linked computer or a phone can't
   rename its person: a stolen one would otherwise rename them in every
   workspace and all past content at once. lt and the CLI point to the
   desktop ("Change your name on your desktop").
3. Floor Viewer, no presence. An agent never writes one; an agent's name and
   picture stay on `AgentUpserted`.
4. Deterministic content checks (denied otherwise), pinned so every build
   agrees: `displayName` is at most 256 bytes, **equals
   `clean_display_name(displayName)`** (`crates/ligus-proto/src/names.rs:43`,
   whose `is_hidden_name_char` list, `:19-41`, is frozen for this check as
   `PROFILE_NAME_RULES_V1` and copied, not referenced, so a later edit of the
   cleaner can't fork the fold), and is 1 to 64 Unicode scalar values; the
   avatar's `contentType` is one of the three, its decoded length is at most
   16 KiB, its leading bytes match the type (PNG, JPEG `FF D8 FF`,
   RIFF/WEBP), and `sha256` matches. SVG is never accepted.

### 5.3 Fold

`ChatSession.profiles: BTreeMap<Uuid, Profile>`. Each field is
last-writer-wins in the full fold order (`(lamport, event_id)`,
`events.rs:815-821`, then the copy order of `crates/ligus-core/src/copies.rs:26-30`),
so every device converges. A removed member's profile
stays for rendering history.

### 5.4 Who reads it

- **Roster and People**: `name_of` and `picture_of` (4.3). The roster's own
  `actor.display_name` is a fallback only.
- **Stamps**: unchanged on the wire (the actor still carries a
  `display_name`, refreshed from the fold on each draft), but no renderer in
  0.4 reads a name from a stamp when a profile exists.
- **Linked computers**: `LinkState` stores ids only; the name shown is the
  profile (fixes the frozen snapshot).
- **Phones**: `/v1/me` and `/v1/profile` on `ligus serve` return the profile;
  the phone refreshes `PhoneBinding.displayName` on every foreground and
  every push (`HostProfiles.swift`), so it is a cache.
- **Phone actions**: `phone_person` stamps the roster actor with the profile
  name.
- **Directory** (`directory_trust.rs`): per account, the profile with the
  greatest `(issuedAtMs, event_id)` across the pinned workspaces where the
  account's key fingerprint matches, with `issuedAtMs` clamped to the time
  this device first stored the event plus 5 minutes, so a far-future value
  can't pin a name. This is device-local display, not a fold (fixes the
  first-iterated workspace).
- **Desktop `my_profile`**: the fold's profile for `me(ws)`, then the live
  author, then `identity.json`.

### 5.5 Writing it

- Desktop rename and avatar change (`set_display_name_sync`, `set_avatar_sync`)
  write `identity.json` as now and then append one `ProfileUpdated` to the
  config log of **every workspace** where this account is a current member.
  A workspace that is offline gets it on the next sync.
- On first start of 0.4, a non-delegated desktop writes one `ProfileUpdated`
  for its account in a workspace only when the fold there has **no profile
  yet** for the account, and only after a full config-log sync, from its
  current `identity.json` name and avatar. A stale laptop that starts later
  finds a profile and writes nothing. lt, the CLI and boxes never write one
  on first start.
- `ligus profile set --name ... [--avatar FILE]` (new, `--json`) for headless
  use.

### 5.6 Abuse limits

- Content checks (5.2) are in the fold.
- Authoring throttle in every client: at most one per minute and 20 per day
  per account and workspace; the 21st is refused with exit 1.
- Bounded harm: a member who bypasses the throttle can append about 22 KiB
  per event to the **config** log, which every authoring path folds
  (`chat_service.rs:866`), so this is costlier than chat spam. It is bounded
  by the 16 KiB cap and handled by removal. Moving avatar bytes to a
  hash-referenced blob is the later fix, once blob retention (10.5) can be
  relied on. A later
  `ProfileHidden{accountId}` (Admin, presence) lets an Admin blank an abusive
  name or picture for everyone without removing the member (section 14).
- **Impersonation** is a rendering problem: names are not identities. Names
  that collide after case folding and confusable skeletons (UTS #39,
  render-side only, never in the fold, so table versions can't fork it) are
  checked across members, **former members, service members, agents and the
  reserved "Ligus" and "Hive"**; every colliding person shows `Name#N` and
  their key fingerprint chip. Agent prompts and turn attribution
  (`attributed_turns`, `chat_service.rs:3817-3875`) get the disambiguated
  form, so a person renamed "Ligus" never reads as the default agent. The
  role chip comes from the roster.

## 6. `DeviceLabelSet`

### 6.1 Shape

```jsonc
// config log, kind "deviceLabelSet"
{ "kind": "deviceLabelSet",
  "label": { "deviceId": "f3b6…", "accountId": "6c1e…", "label": "Ana's MacBook Air" } }
```

`label` is 0 to 48 Unicode scalar values after `clean_display_name`, with the
same forbidden characters as 5.2; empty clears it.

### 6.2 Who may write it

The stamp's account is `accountId`, the device `deviceId` is certified for
`accountId` and not revoked, and either:

- **the device names itself**: the envelope's `signer_device_id` is
  `deviceId` (any Ed25519 device; boxes too); or
- **a rename from My devices**: the signer is a certified, non-delegated,
  non-phone device of `accountId` (the person's desktop renaming their phone
  or their lt).

Floor Viewer, no presence. Labels are keyed by **(deviceId, accountId)**, so a
member who certified someone else's device key for themselves can only label
it under their own name: their label never shows next to the device's real
person.

### 6.3 Fold and rendering

- `ChatSession.device_labels: BTreeMap<(Uuid, Uuid), String>`, last writer
  wins in canonical order.
- `device_name(device, account)`: the label for (device, account), else the
  host label, else the phone label this desktop remembers, else "This device"
  or "Device" and eight hex digits. A device name is always shown with its
  person ("Ana's MacBook Air" under Ana in People, "MacBook Air (Ana)" in a
  host list), never alone.
- The desktop's `phone_labels` become a one-time source: on first start of
  0.4 the desktop writes a `DeviceLabelSet` for each phone it labelled.
- My devices gets **Rename** (desktop, lt `r`, `ligus devices rename DEVICE
  NAME`); a box's label is set by `ligus serve --label` at start.
- A device publishes its own label as the **first event of a link** (7.6),
  which also vouches for the person.

## 7. "Me" on a linked device

### 7.1 Principle

**A device acts for the account it chose, proved by its own signatures or
its own redeem. A certificate published by anyone else can add an account
the device could choose; it never removes, blocks or redirects the device's
choice.** And no "who am I" decision ever reads a display name.

### 7.2 The binding, per workspace

```rust
// crates/ligus-runtime/src/device_binding.rs (new; replaces standing())
pub enum Binding {
    Own,                      // acts as this install's own account (never linked anywhere)
    Linked { account: Uuid }, // acts as the person, delegated
    NotLinkedHere,            // linked as a person's device, but not in this workspace
    Pending { account: Uuid },// linked here per the cache, certificate not synced yet
    PersonRemoved { account: Uuid }, // certified, but the person is no longer a member here
    Revoked,
    Ambiguous(Vec<Uuid>),     // this device signed as two people here: a person must pick
}
pub fn binding(ws, device, local_account, cache: Option<&WorkspaceLink>, log) -> Binding
```

1. If this device is revoked in `ws`: `Revoked`.
2. `certified` = undisputed accounts other than `local_account` with a valid
   certificate for this device in `ws` (`timeline.current().devices`).
3. `chosen` = the accounts this device itself signed for in `ws`: envelopes
   whose `signer_device_id` is this device, whose stamp names the account,
   **and whose signature verifies under this device's own public key**. The
   `signer_device_id` index alone is not enough: ingest stores trust
   bootstrap kinds from a not-yet-certified signer without checking the
   signature (`crates/ligus-runtime/src/envelope_verifier.rs:977-982,1018-1035`;
   `identity_verifier.rs:209`), so anyone can plant a row "signed by" this
   device. The check re-verifies each candidate, as `RosterTracker::vouched`
   does (`timeline.rs:376`).
4. With a cache entry for `ws` (`LinkState.links`, 7.4) naming account `a`:
   `Linked{a}` if `a` is in `certified`, else `Pending{a}` (read-only, "your
   desktop's approval hasn't reached this computer yet", the existing
   `NOT_ON_RELAY_YET`).
5. No cache entry for `ws`: the only account the binding may adopt is the
   **install's one person** (`LinkState.person`, one per install,
   `computer_link.rs:545-553`; or, when the whole file is lost, the single
   account in `chosen ∩ certified` over all of this install's workspaces).
   If that account is in `chosen ∩ certified` for `ws`, `Linked` with it and
   the cache entry is rewritten (a lost entry heals itself). Any other
   account, verified or not, is never adopted. If the file is lost and two
   accounts qualify, `Ambiguous`: writes blocked, and the person chooses by
   account in `ligus devices use ACCOUNT` or lt's prompt.
6. Otherwise, if this install is linked anywhere (`LinkState.links` not
   empty): `NotLinkedHere`. Else `Own`.
7. A `Linked` account that is no longer a member of `ws` gives
   `PersonRemoved` (read-only).
8. Certificates in `certified` that are neither the cache's nor in `chosen`
   are **foreign**: they change nothing above. My devices and `ligus devices
   status` show one quiet line, "Ben's account also certified this
   computer's key. That gives Ben nothing over it.", with Details.

`Own` for an install that is a box (`StoredIdentity.service`) is final: a box
never becomes `Linked` (owner-device mode is removed).

### 7.3 Who uses it

| Caller | Today | v2 |
|---|---|---|
| CLI `open_service_as` (`main.rs:340-365`) | `linked_in(room)`, else own | `binding()`: `Linked` acts as the person, `NotLinkedHere` refuses with `linked_device` (exit 6) and "link this computer in this workspace", `Pending` is read-only |
| lt `open_core` | `LinkState.person` in every workspace | `binding()` per workspace on every write and `activate`; `NotLinkedHere` shows the workspace read-only with **Link here** |
| `ligus serve` `refresh_standing` (`serve/mod.rs:559-596`) | blocks writes on `OwnerDevice` | only `Revoked` blocks; a foreign certificate logs one line and is shown in `ligus serve status` |
| `phone_person` (`phone_sign.rs:77-99`) | exactly one holder among members | each phone request names its account in the request envelope, **outside** the signed `PhoneAction` (which is `deny_unknown_fields`, `phone_action.rs:44`), and only when the box's `/v1/me` advertises `phoneAccount: 1`; the phone already holds it in `PhoneBinding`. The box checks the phone is certified for it, unrevoked, and that account is a human member. Foreign certificates are ignored |
| `issue_computer_link`, `check_computer_link` | refuse any other certifier | refuse only when the device **chose** another account (point 3), or was revoked; foreign certificates are ignored and shown |
| `device_standing(ws, prefer)` | `prefer` matches a display name | removed; `prefer` is gone |
| `likelyMe` hint (`web/src/lib/likelyMe.ts`) | matches display names | stays a hint only; when the request carries a `previous` proof, the desktop names the old member exactly (8.3) |
| `DeviceLinks::from_config` (`device_link.rs:47-72`) | any Admin who certified a host device maps to its owner | an account maps to a host's owner only when the device itself **signed as that account** (verified, as in 7.2 point 3) and the owner also certified it. The case this map exists for still maps: a computer that was its own member and is now the person's linked host signed as that old member. An Admin who merely certified someone's host, including the removed owner-device box mode, no longer maps. Class L: read by `agent_access`, dispatch and notify on each device, never by the fold |
| `retired_aliases` (`device_enroll.rs:266-302`) | any certifier of a device that signed as a removed member inherits it | only an account the device itself vouched for |
| `rotation_recipients` (`timeline.rs:247-258`) | a removed member's device stays a recipient while any current member certifies it | stays a recipient only while a current member **holds** it. Computed in core but read only by the rotating device (`crates/ligus-runtime/src/keyring.rs:355`), so it is class L and ships in 0.4; the rotating desktop also warns ("Ben's certificate still names Eve's old device; revoke it") |

### 7.3a Phones a foreign certificate pins

A phone never vouches, so items 5 and 8 of section 1 can't be fixed by
vouching, and log order can't decide either: an author can name an older
`log_head` (`crates/ligus-core/src/lamport.rs:300-306`), so a later
certificate can sort first. The phone's person is bound to **a signature the
phone makes once, at enrollment**:

- The phone signs an **enrollment statement** with its P-256 key:
  `"ligus-phone-enrol-v1\0" ‖ workspace_id ‖ account_id ‖ device_id`, inside
  its `ligusphone1` or reverse-QR request (additive field). The phone learns
  the account and workspace from the desktop's side of the exchange, shows
  "Add this phone for Ana in Hive Lab?" and signs only after the person
  confirms. The enrolling desktop copies it into the certificate event as an additive
  `phoneConsent` field of `DeviceCertificateAdded` (a hint under rules 2).
- **Rules 3 (class G)**: a P-256 certificate counts only when its
  `phoneConsent` verifies under the certified key and names the certificate's
  account and this workspace. The phone's person is that account, it may
  always revoke the phone whatever the ranks, and a certificate of the same
  key for another account doesn't count. **Legacy phones** (enrolled before
  0.4, no consent) keep counting while theirs is the only P-256 certificate
  of that key; a 0.4 phone app also signs the statement for its existing
  enrollment, and the person's desktop re-publishes the same certificate with
  `phoneConsent` (re-publishing a certificate already in force needs no
  presence, `rules.rs:29-37`). A statement made once at enrollment
  can't move the hold later, so a stolen phone still can't take its person's
  hold away (the reason phones never vouch).
- **0.4 (class L)**: desktops write `phoneConsent` when the phone sends it;
  `phone_person` prefers the account its consent names; and when a phone key
  has certificates from two accounts, the person's desktop shows an Inbox row
  ("Ben also certified your phone's key. If you lose the phone, an Owner must
  revoke it."), naming the Owner. Desktops don't refuse to certify an
  already-certified key, which would let a racing certificate block a
  genuine enrollment.

A phone's device id derives from its key, so re-enrolling after a
revocation needs a new key, as today. A foreign certificate on a computer
still makes it contested for revocation (R5, `timeline.rs:192-205`) until it
vouches, which the 0.4 redeem does at once (7.6).

## 8. Linking lifecycle

### 8.1 Certify on acknowledgement (live path)

This amends `docs/design/code-match-approval.md` (5.2, 6.3) and keeps its
checks 1 to 6. The certificate and the workspace keys reach their
destinations only after the requester has acknowledged, and nothing is ever
held signed but unpublished:

1. The codes match and the person chooses **Codes match**.
2. The desktop **builds and seals the grant now** and keeps it in memory
   (sealing is randomized, so it can't be rebuilt later to the same bytes),
   then sends **`Offer`**: `{rid, grantDigest, person, approverDeviceId}`
   signed by `approverSigKey` under `"ligus-match-offer-v1\0"`, sealed to
   `requesterKa`. `grantDigest` is the SHA-256 of that sealed grant; the
   grant itself (with the workspace and epoch keys) is **not** sent yet. If
   the workspace key rotates before step 5, the exchange aborts and starts
   again.
3. The requester checks the `Offer` signature and that `person` and the
   approver match the `Open`, then posts **`Ack`**: `{rid, grantDigest}` signed
   by its device key under `"ligus-match-ack-v1\0"`, sealed to `approverKa`.
4. The desktop verifies the `Ack` against the request's device key and the
   digest, runs the passkey ceremony, and drafts, signs and publishes the
   `DeviceCertificateAdded` in one step, exactly as today. The signer chain
   and the lamport are drafted fresh, so no other config write can stale it
   (`forks_chain`, `event_store.rs:1029-1071`).
5. Only then does `Result{approved, grant, grantDigest}` carry the sealed
   grant. The requester checks that the grant hashes to the acknowledged
   digest, runs checks 1 to 6 (check 5 finds the certificate on the log),
   redeems, and publishes its vouch (7.6).
6. No `Ack` within 2 minutes: nothing is signed, nothing reaches the log, no
   key leaves the desktop. The Inbox keeps one row for a day ("Linking
   build-1 didn't finish. Nothing was added.").

A member who races an `Open` (code-match 6.4) now gets an `Ack` and
publishes a certificate for themselves before the computer's final "Link as
Mallory? No". That leaves a foreign certificate on the computer, which
`binding()` ignores (7.2). The message count (claim, open, reveal, offer,
ack, result) stays within the mailbox's 10 per request.

What remains: a requester that acknowledges and then vanishes after step 5
holds the keys and a certificate it never used. 8.2 catches that. No new log
event and no rules change: every peer folds the same `DeviceCertificateAdded`
it folds today. Box admissions keep their own lifecycle (`JoinApproved` with
a 1-hour redeem, `AdmissionClosed` by an Admin's device,
`crates/ligus-core/src/admission.rs`).

### 8.2 Approved, never finished (paste path and old certificates)

The paste path can't tell the desktop when a computer redeems, and old
certificates like the owner's "Device f3b686ca" exist. A desktop marks a
device of its own account **never finished** when all hold:

- its certificate for this account is delegated (published by another
  device, `timeline.rs:595-599`);
- the device has signed nothing on any log of the workspace
  (`idx_events_by_signer`);
- the certificate's `issued_at` is more than 45 minutes old (request 15 plus
  grant 30, `computer_link.rs:54-56`) by this desktop's clock.

My devices shows it as "Approved on <date>, hasn't written anything yet"
(a 0.3 computer that redeemed and only reads looks the same, so the wording
doesn't claim more) with
**Remove**, and the Inbox shows one row once. Remove is the existing
`DeviceRevoked` with presence (`rules.rs:26`: every revocation, self-revoke
included, needs it), so a silent automatic revoke is impossible and is not
proposed (decision O4). `ligus devices list --json` reports
`"state": "never-written"`. Only signatures that verify under the device's
key count (the planted rows of 7.2 point 3 don't clear the state).

### 8.3 Retiring the duplicate member: `MemberRetired`

When a computer that was already a member (its own account, `A_old`) links
as the person (`A_new`), it retires `A_old` itself at redeem, with no extra
passkey ceremony:

```jsonc
// config log, kind "memberRetired"; signed by the computer's device key,
// stamped as A_old (the computer holds A_old's account key and is its full device)
{ "kind": "memberRetired",
  "retirement": {
    "memberId": "<A_old actor id>",
    "accountId": "<A_old>",
    "successorAccountId": "<A_new>",
    "deviceId": "<the computer>",
    "signature": "<A_old account key over 'ligus-member-retire-v1\\0' ‖ ws ‖ A_old ‖ A_new ‖ deviceId>",
    "watermark": [ ... ]   // A_old's signers' heads, as MemberRemoved carries
  } }
```

The computer writes it only when all hold, and the fold checks the same:

- `A_new` is a current human member and has certified `deviceId` (the link
  happened), and the computer has vouched for `A_new` (7.6);
- `A_old`'s rank is not above `A_new`'s, `A_old` is not the last Owner and
  not the founder (`authorization.rs:518-530`, `authz/mod.rs:1334-1340`);
- `A_old` holds no passkey (presence for `A_old` can't be given from this
  computer); otherwise the computer shows "Your earlier member <name> needs an
  Admin to remove it" and the desktop's People offers Remove to Admins.

Effects:

- **Rules 2 (class R)**: display only, and conservative. 0.4 People folds
  `A_old` under `A_new` ("Ana, also as zaz-lt until <date>") for Contributors
  and Viewers, but **never hides it from Admins**, who see it as a separate
  member marked "retired, still a member until rules 3". Content `A_old`
  signed **before** its `MemberRetired` renders as `A_new` with "(earlier on
  build-1)"; anything else signed as `A_old` **after** it (a later
  `MemberRetired` for the same retirement excepted) renders as `A_old`,
  unfolds the merge everywhere, and raises an Inbox row for Admins ("zaz-lt
  retired but kept writing"). Votes by `A_old` count as they do today, and
  People shows them under `A_old`. Authz is unchanged on every build.
- **Rules 3 (class G)**: a `MemberRetired` at a position after the
  `WorkspaceRulesSet{3}` is also a removal of `A_old` with its watermark,
  judged like a self-leave (floor Viewer, no presence because `A_old` holds
  none, the account-key signature is the authority). One before that position
  stays display-only forever, so nothing already folded under rules 2 is
  judged again; the computer writes a fresh one after rules 3 if it still
  needs to. The host whose id is `deviceId` passes to `A_new` (the person
  certified that device with presence); `A_old`'s other hosts become inert
  (8.4). No role moves to `A_new`.
- Afterwards the computer marks `A_old` retired in `identity.json` and signs
  as it again only to repeat the same `MemberRetired` once rules 3 is set.

The desktop's approval sheet names what will happen: "Link build-1 as your
computer. It was <old name> here; that member will be retired."

### 8.4 Hosts of removed and retired members

- **0.4 (class L)**: host lists, the agent host picker and `list_hosts`
  hide hosts whose owner isn't a current member or whose device is revoked.
  The desktop's workspace settings show Admins "3 hosts from former members"
  with **Clean up**, which publishes `WorkspaceHostRemoved` (presence, one
  ceremony per host).
- **Rules 3 (class G)**: the fold drops a host from the projection when its
  owner is removed or retired, so agents bound to it become unbound
  everywhere at once.

## 9. Ids

| Item | 0.4 | Later |
|---|---|---|
| Workspace in sync tables | New tables `sync_cursor_v2(relay_origin, workspace_id, last_fetched_seq)`, `pushed_v2(relay_origin, workspace_id, event_id)`, `parked_bodies_v2(relay_origin, workspace_id, seq, body, priority)`. The room string is used only to build relay URLs. Migration in 11 | Workspace ids that don't collide when two relays share a room name (needs a new id derivation and a new wire field; not planned) |
| The four "who" ids | `Who` (4.1); `Signer` without `Uuid::nil()` | `MemberAdded` id invariant under rules 3 |
| Agent on a message | One reader, `agent_of(message) -> Option<Uuid>`: `responder_id`, else `related_agent_id` parsed, else `agent_persona_id`, else the roster agent with that name (legacy only). Writers unchanged | A well-known id for the default agent written as `responder_id` (old peers treat `None` as the default agent today, so this waits for rules 3) |
| `sequence` vs row place | Documented: the signed `sequence` is informational (no fold rule reads it, `crates/ligus-core/src/crypto.rs:169` signs it, `events.rs:767` compares it) | Store rebuild: a `place` column with `UNIQUE(workspace_id, session_id, place)`, the signed sequence drafted per (signer, log), the `UNIQUE(session_id, sequence)` dropped (11, migration M5) |
| Equivocations | Nothing: the stored `envelope_json` names its workspace, so `UNIQUE(event_id, envelope_json)` is already per workspace. Reads filter by workspace (`idx_equivocations_log`) | none |
| `join_approvals` mixing member ids, device ids and `invite:<uuid>` (`crates/ligus-core/src/guard.rs:116-118`) | none | a typed key in the fold (not on the wire) |
| Advisory `scope` on `AccountKeyRotated` and `DeviceRevoked` | none (authz uses `session_id`) | none |

## 10. Storage

### 10.1 One connection list

`workspaces.json` (`connection_settings.rs`) becomes the only format, in each
app's own data dir; secrets stay in the secret store under
`<scope>:ws-<uuid>-{token,key,ek}`.

- The desktop imports `settings.json` `workspaces[]`, `sync_room` and
  `joined_rooms` once, copies each secret to the new id, reads it back, and
  only then marks the entry imported. The old keys stay in `settings.json`
  and the old secret ids stay in the store through 0.4, so a downgrade still
  finds them; 0.5 deletes them.
- The CLI imports `config.toml`'s single room as one entry; `config.toml`
  keeps `relay_url` and `room` for a downgrade.
- Later than 0.4 (decision-free, scheduling only).

### 10.2 One code-folder binding table

`repo_checkouts` and `chat_checkouts` in `ligus.db`, plus a new
`workspace_folders(workspace_id, path)` for a folder bound to the workspace
before any repo is known. Desktop `workspace_dirs` and `workspace_root`, the
CLI's `repo-bindings.toml` and lt's `checkouts` are imported once and then
only read as a fallback through 0.4. Later than 0.4.

### 10.3 Secrets out of SQLite and off the log

- **0.4**: `join_redeem_keys` and `issued_invite_keys` move to the secret
  store (`redeem:<ws>`, `invite-key:<ws>:<invite>`). 0.4 reads the store
  first, then the table. Migration M2 copies each row and reads it back on
  every start, but **deletes a row only once its invite or admission is
  closed or expired on the log**, so a 0.3 process still running on the same
  data dir (a serve loop beside a one-shot CLI, `event_store.rs:1053-1058`)
  and a downgrade both still find every live seed. Deletion runs with
  `PRAGMA secure_delete = ON` followed by `wal_checkpoint(TRUNCATE)`, so the
  seed doesn't survive in free pages or the WAL.
- A **person invite** (`JoinApproved{subject: "invite:<id>", redeemKey}`,
  `events.rs:359-365`) has no expiry and nothing closes it but its
  redemption. Its rows are deleted on use as today (`take_issued_invite_key`,
  `clear_join_redeem_key`, `event_store.rs:2378-2385,2428`), and otherwise 7
  days after M2 first copied them. Box admissions use their expiry and
  `AdmissionClosed`.
- M2 copies on every start and **overwrites the store when the table row
  differs**, so a seed a concurrent 0.3 process writes after the upgrade is
  never shadowed by a stale copy.
- **0.4**: every `*.bak-ws-event-ids-*` backup holds these seeds. M2 lists
  them and `ligus doctor` warns with the paths and "delete once you no longer
  need the backup". Nothing deletes a backup by itself.
- **0.4**: authoring a `WorkspaceCredential::Inline` is refused in every
  client ("store the key on the hosts that run the agent"). The desktop flags
  each workspace runtime whose log history holds an inline key: "This key was
  visible to every member. Rotate it at the provider." The log is immutable,
  so the old key can't be erased.
- **Rules 3**: the fold denies runtime events with `Inline`.

### 10.4 Database schema version

`PRAGMA user_version` is set after the probes run: 0 (none) on a database
made before 0.4, then `1` once M1 has run (M2 runs on every start whatever
the version, because its deletions wait for invites to close). A build that opens a
database with a higher version than it knows opens it read-only and says
"this data was written by a newer Ligus; update or use another data dir"
(exit 3 for the CLI). 0.3 doesn't check it, so the first downgrade it
protects against is from 0.5 to 0.4.

### 10.5 Blob retention tied to attachments

- **Relay (additive route)**: `POST /v1/workspaces/{id}/blobs/{blob}/retain`
  (beside the existing blob routes, `relay:internal/relay/blobs.go:28-30`),
  for a member of the room on a gated relay (any client on an open one), sets
  the blob's `expires_at` to now plus the room's retention, **never past its
  creation time plus an operator maximum** (`BLOB_MAX_AGE_DAYS`, default 365),
  so renewals can't defeat an operator's retention. Retained blobs still count
  against the room quotas (`blobs.go:82-84`); the desktop shows Admins the
  room's blob use and the largest files when it passes 80%. Unknown blob:
  404. Old relays: 404, and clients keep today's behaviour.
- **Clients**: the sharer's devices and every Admin's desktop renew each
  live (not `AttachmentDeleted`) attachment of the workspace when it is past
  a third of its life, at most once a day per blob, with jitter. Deleting an
  attachment deletes the blob (unchanged).
- An attachment whose blob is gone shows "This file expired on the relay"
  instead of a broken download.
- Decision O9 picks this over relay-side "never expire bound blobs".

### 10.6 The relay roster

Deprecated for Ligus workspaces in 0.4 docs (`LANGSTROTH_RELAY_MEMBERSHIP`
stays a relay feature for other clients). Ligus never reads it, and enabling
it can drift from the signed roster. A relay can't verify the signed roster
(it holds no keys, by design), so "derived from the signed roster" would mean
trusting an Admin device's export, which is later and only if an operator
asks (decision O10).

## 11. Migrations

| # | When | What | Safety |
|---|---|---|---|
| M2 | 0.4, every start, **first** | Seeds to the secret store (10.3) | Runs before any backup is taken, so no new backup holds a seed it has already moved. Per-row copy and read back; rows deleted only after their invite closes, with secure delete |
| M1 | 0.4 start, after M2 | Sync tables to `*_v2` keyed by `(relay_origin, workspace_id)`: `workspace_id = room_workspace_id(room)`, `relay_origin` from the connection that names the room | One backup (event-id migration pattern). A cursor and pushed set are copied **only when exactly one connection names the room**; otherwise the `_v2` state starts at 0 (refetch is safe, re-push is deduplicated by the relay's idempotency key, `crates/ligus-runtime/src/sync_engine.rs:335-337`). Old tables kept unchanged for a downgrade. A `parked_bodies` row whose room matches no single connection stays in the old table and is drained by the old code path |
| M4 | after M1 | `PRAGMA user_version = 1` | After M1 succeeds |
| M5 | later | `events` place split (section 9) | The event-id migration's own steps, including `reclaim_after_migration` |
| M6 | 0.4 first start | One `ProfileUpdated` and the `DeviceLabelSet`s from `phone_labels` (5.5, 6.3) | Log appends, only from a non-delegated desktop, after a full config sync, and only where no profile exists yet |
| M7 | every start | `computer-link.json` gains `links` from `person` and `workspaces` whenever `person` is set and `links` is missing or older | A 0.3 `save()` drops `links` (no catch-all field, `computer_link.rs:469-479`), so this re-runs after any downgrade. Only for workspaces where `binding()` sees the certificate; others become `Pending` |

(There is no M3: `equivocations` needs no rebuild, section 9.)

**Concurrent processes.** One data dir is one device, but not one process:
`ligus serve` and one-shot CLI commands share `ligus.db`
(`event_store.rs:1053-1058,1463-1468`). 0.4 takes an advisory lock file
(`ligus.db.lock`): shared for normal use, exclusive for M1. A 0.4 process
that can't get it exclusive skips M1 for this start. A 0.3 process takes no
lock, so M1 is written to be safe beside one: it only adds tables and copies,
never alters the old ones, and M2 never deletes a live seed. The release
notes still say to restart `ligus serve` after upgrading the CLI.

## 12. Mixed versions

| Situation | 0.3 peer | 0.4 peer |
|---|---|---|
| `ProfileUpdated`, `DeviceLabelSet` on the log | Stores, folds no-op; shows roster names, "Device xxxx" | Shows profile and labels |
| A 0.4 message without an avatar in `actor_identity` | Initials for that message | Profile picture |
| `MemberRetired`, rules 2 | No-op; shows both members | Folds the old member under the person for non-Admins; authz identical to 0.3 |
| `MemberRetired`, rules 3 | Workspace read-only, "update Ligus" | Removal |
| Live link, 0.4 computer and 0.4 desktop | n/a | Certify on `Ack` (8.1) |
| 0.3 computer, 0.4 desktop (paste) | Redeems as today, publishes no vouch | Certificate at approval; "hasn't written anything yet" until it writes (8.2) |
| 0.4 computer, 0.3 desktop (paste) | Approves as today | Redeems, publishes its vouch |
| A foreign certificate on a 0.3 box | Box blocks writes (today's bug) | Box ignores it |
| Reaction or answer with a hint that disagrees with its stamp | Ignores the field | Ignores the hint under rules 2 |
| A 0.3 build opening a 0.4 database | Works: new tables ignored, old tables intact, `links` dropped on its next save (M7 restores it), every seed of a live invite still in its table; seeds of invites issued by 0.4 are only in the secret store, so 0.3 can't build those codes and says the invite is missing (verified by test M2t) | n/a |
| 0.4 phone, 0.3 box | Box has no `phoneAccount` capability; the phone sends nothing new | Today's behaviour, including the foreign-certificate DoS on that box |
| 0.3 phone, 0.4 box | Phone sends no `hints` capability | Box drafts without hints; picks the person by the phone's single holder as today, else asks the phone to update |
| 0.4 box drafting for a 0.4 phone | n/a | Account named outside the action; hints included |

## 13. Threats

| # | Threat | Defence | Residual |
|---|---|---|---|
| T1 | A member sets their name or picture to look like another member | Names are never identities (section 4); colliding names show `Name#N` and the key chip; the role chip is from the roster; profile content checks | A careless reader, as with any chat |
| T2 | A member writes a `ProfileUpdated` for someone else's account | Denied: `accountId` must be the stamp's, and the signer certified for it (5.2) | None |
| T3 | A member who certified Ana's device key labels it "Ana's phone" | Labels are keyed by (device, account) and shown under the account (6.2) | None |
| T4 | A member certifies a box's, phone's or computer's key to stop, redirect or borrow it | `binding()` and the phone request use the device's own verified choice; alias maps and key recipients read holders, not certificates; foreign certificates only show (7.2, 7.3) | Phones until rules 3 (7.3a): an Owner may have to revoke |
| T4c | A 0.3 Admin's desktop rotates the key | It still seals to a removed member's device that another member certified (`keyring.rs:352-355` in 0.3) | Until every Admin desktop is on 0.4 |
| T4b | A member plants rows "signed by" a device to make it adopt them | `chosen` re-verifies signatures, and only the install's one person is ever adopted (7.2 points 3 and 5) | None |
| T5 | A lost or tampered `computer-link.json` | It is a cache: rebuilt from the device's own signatures; a cache naming an account that never certified the device gives `Pending`, never write access (7.2) | Someone with write access to the data dir owns the device anyway |
| T6 | A stolen old account key retires its member into someone else | The successor must have certified the device and the device must have vouched for the successor; no role moves (8.3) | The thief can make the old member leave, which the thief could do as that member anyway once self-leave exists |
| T7 | Keys or a certificate reach a requester that never finishes | `Offer` and `Ack` come first; the passkey, the certificate and the keys only after the `Ack` (8.1) | A requester that acks and vanishes: 8.2 |
| T7b | A stolen linked computer renames its person everywhere | `ProfileUpdated` only from non-delegated desktops (5.2) | It can still post as the person until revoked, as today |
| T8 | Profile spam bloats the log | Fold size checks, client throttle, Admin removal; later `ProfileHidden` (5.6) | A malicious member until removed, as with messages |
| T9 | A migration loses sync state or secrets | Backups, keep old tables, per-row secret moves, read-back before delete (11) | Backups hold secrets until the person deletes them (doctor warns) |
| T10 | Two builds fold the same log differently | Class G only behind rules 3, where older builds go read-only; class R never feeds authz (3); test X1 | None while the class rules hold |

## 14. Priorities

**0.4**

1. `ProfileUpdated` and render-time resolution (4.3, 5), desktop rename and
   avatar emit it, phone and directory read it, avatars out of stamps and
   messages.
2. `DeviceLabelSet`, Rename in My devices, the vouch at redeem (6, 7.6).
3. `binding()` replacing `standing()`; the foreign-certificate fixes in serve,
   phones and linking; per-workspace scope in lt; no display-name matching
   (7.2, 7.3).
4. `LinkState` v2, the phantom account on the link path, `whoami` (7.4,
   7.5).
5. Certify on `Ack` in the live path, together with code-match approval; the
   "hasn't written anything yet" state (8.1, 8.2).
6. `MemberRetired` written at redeem and rendered (rules 2 effect); host
   filter and Clean up (8.3, 8.4).
7. M2, M1, M4: seeds out of SQLite, sync tables by uuid and relay,
   `user_version`; refusing new inline credentials (9, 10.3, 10.4).
8. The holders-based fixes for `DeviceLinks`, `retired_aliases` and key
   recipients, and the phone refusal and Inbox row (7.3, 7.3a).
9. Blob retain route and renewals (10.5); relay roster deprecated in docs
   (10.6).

**0.5 and later**

- Rules version 3 (class G set): `MemberRetired` as removal, a phone counts
  only with its signed enrollment statement and its person may always revoke
  it, inline credentials denied, `MemberAdded` id invariant, hint mismatch denied, inert
  hosts of removed members, a default-agent id. Offered to Owners once the
  devices they see are on 0.4 or later (decision O6).
- One connection list (10.1), one binding table (10.2), the events place
  split (M5), deleting the dormant local account seed and the old secret ids.
- `ProfileHidden`, phone self-rename and profile edits from the phone,
  typed mention ids, synced read markers across a person's devices.

## 15. Owner decisions

| # | Question | Recommendation |
|---|---|---|
| O1 | Avatar limits | **16 KiB decoded, PNG, JPEG or WebP only, no SVG**, checked in the fold; hash-referenced blobs later |
| O2 | Stop carrying the avatar on every stamp and message | **Yes.** 0.3 peers show initials on new messages until they update |
| O3 | Who may rename a device, and who may change a person's profile | **Devices: the device itself and the person's own desktops; profiles: the person's desktops only** (not a linked computer or a phone) |
| O4 | Unredeemed approvals | **`Offer` and `Ack` before the passkey, the certificate and the keys in the live path; "hasn't written anything yet" with a passkey Remove for the rest.** No silent auto-revoke (every revoke needs presence) |
| O5 | Retire the duplicate member automatically at redeem | **Yes, when safe** (rank not above the person's, not the last Owner or the founder, no passkey on the old account); otherwise ask an Admin. Display-only until rules 3 |
| O6 | When to offer rules version 3 | **From 0.5, Owner-triggered**, with a readiness list: devices still on older builds, and phones that have neither a consent nor a sole certificate; older builds then go read-only on that workspace |
| O7 | A linked computer in a workspace it wasn't linked in | **Read-only with Link here** (one passkey per workspace); no automatic multi-workspace link in 0.4 |
| O8 | The phantom account's seed | **Keep it dormant through 0.4** for downgrades, delete in 0.5 |
| O9 | Blob retention | **Client renewals through a retain route**, rather than relay blobs that never expire |
| O10 | The relay roster | **Deprecate for Ligus workspaces**; no signed-roster export unless an operator asks |
| O11 | Inline model keys on the log | **Refuse new ones in 0.4, deny under rules 3, and prompt rotation** of keys already there |
| O12 | A phone someone else also certified | **The phone's own signed enrollment statement names its person.** 0.4 warns and follows it; rules 3 counts only certificates with it, and the person may always revoke |

## 16. Tests (executable, required before merge)

| # | What | Where |
|---|---|---|
| P1 | `ProfileUpdated` for another account, from a revoked, delegated or phone device, by an agent, with a name that `clean_display_name` would change, over 256 bytes, an SVG, a 17 KiB image, wrong magic bytes or a wrong sha256: all denied; last writer wins per field in canonical order regardless of arrival order (property test over permutations) | `ligus-core` authz and fold tests |
| P2 | Rename on the desktop reaches the roster, People, a linked lt, a phone's `/v1/me` and the directory in every shared workspace; old snapshot names never return | `ligus-runtime` multi-device test |
| P3 | The directory picks the newest profile across workspaces, not the first iterated | `directory_trust` tests |
| L1 | `DeviceLabelSet` by the device itself and by the person's desktop allowed; by a delegated device for another device, by a phone, or by a foreign certifier for the person's key denied; a foreign certifier's label shows only under the certifier | `ligus-core` |
| L2 | My devices shows the synced label for a phone and an lt on another desktop of the same person; "Device xxxx" only when nothing was ever set | `app` tests, web Vitest |
| B1 | A foreign certificate on a running `ligus serve` doesn't block writes; revocation still does | `ligus-cli` serve tests |
| B2 | A foreign P-256 certificate on Ana's phone: her phone actions still sign as Ana; the box refuses a phone request naming an account the phone isn't certified for | `phone_sign` tests |
| B3 | A foreign certificate on a computer's key before approval: the link still completes; after the computer vouched for Ana, a second person's approval is refused | `computer_link` tests |
| B4 | `binding()`: lost cache heals from signatures; a cache naming an uncertified account gives `Pending` and no writes; two vouched accounts give `Ambiguous`; no code path takes a display name (the signature has none) | `device_binding` tests |
| B5 | lt in a workspace it wasn't linked in is read-only and offers Link here; it never writes as the person there | lt backend tests |
| B6 | `ligus whoami --json` on a linked computer reports the person per workspace and `localAccount.retired: true` | `ligus-cli` |
| K1 | Certify on `Ack`: no passkey prompt, certificate or key material before a valid `Ack`; an `Ack` signed by another key or for another digest is refused; nothing is left after 2 minutes without one; a config write by the approver during the wait doesn't affect the later publish; the grant hashes to the acknowledged digest | `approval_tests` with code-match |
| K2 | "Never finished": a delegated certificate, no signatures, 46 minutes old shows the state; one signed event clears it | `app` tests |
| K3 | `MemberRetired`: allowed only with the account-key signature, a certified and vouched successor, rank not above, not last Owner or founder, no passkey; under rules 2 the fold's authz output is byte-identical with and without it; under rules 3 it removes with the watermark and drops the hosts | `ligus-core` |
| X1 | **Mixed-version fold**: a pinned 0.3 `ligus-core` (git dependency at the `v0.3.0` tag) and the 0.4 core fold the same generated logs, with every new kind and hint interleaved (including malformed class R events and ones signed by phones and non-members), under rules 2; every projection except `profiles`, `device_labels` and `updated_at` is equal, every verdict of every pre-existing kind is equal, and `holders` is equal | `crates/ligus-core/tests/mixed_version.rs` |
| X1r | The same at runtime level: a pinned 0.3 `ligus-runtime` ingests, stores and re-pushes the class R events unchanged; nothing becomes a lamport witness or held on either build because of its kind | `crates/ligus-runtime/tests/mixed_version.rs` |
| X2 | Rules 3 set on the log: the 0.3 core reports read-only; the 0.4 core applies the gated rules; a `MemberRetired` before the rules-3 position stays display-only; an event from a 0.3 device whose frontier predates rules 3 folds identically on both builds | same |
| X4 | 0.4 phone with a 0.3 box, and 0.3 phone with a 0.4 box: every phone action still signs and posts; no `deny_unknown_fields` refusal; no hint in a preimage the phone can't re-encode | `serve` tunnel e2e tests, phone tests |
| B4b | Planted rows: an envelope with this device as signer, a foreign stamp and a garbage signature, plus a real foreign certificate, in a workspace this install isn't linked in: `binding()` stays `NotLinkedHere`, the cache is unchanged, "hasn't written anything yet" isn't cleared; two planted accounts don't cause `Ambiguous` | `device_binding` tests |
| F2a | Mallory (Admin) certifies Ana's desktop key: Mallory's turn on Ana's private agent is refused and isn't counted as Ana's; dispatch and push don't treat Mallory as Ana | `agent_access`, `turn_dispatch` tests |
| F2b | A member who certifies a removed member's device inherits no alias, history or agents | `device_enroll` tests |
| F2c | After Eve is removed, a rotation doesn't seal to Eve's device despite Ben's certificate | `keyring_tests` |
| B7 | Ben (Admin) certifies Ana's phone key: Ana gets the Inbox row; `phone_person` follows Ana's `phoneConsent`; under rules 3 Ana's revoke of her phone is allowed and Ben's certificate doesn't count | `ligus-core`, `app` tests |
| B7c | A phone enrolled before 0.4 (no consent, sole certifier) keeps working after rules 3; after its 0.4 re-consent it keeps working when a second account certifies its key | `ligus-core`, phone tests |
| B7b | Ben's certificate of Ana's phone key is backdated (its `log_head` older than Ana's certificate, so it sorts first): under rules 3 Ana stays the phone's person; a certificate whose `phoneConsent` names another account or workspace doesn't count | `ligus-core` |
| M1d | Two connections naming the same room on two relays: no cursor is copied; both relays resync from 0 and no event is skipped | `event_store` migration tests |
| M1c | A 0.3 serve process writing while a 0.4 CLI starts: no seed of a live invite is deleted; M1 leaves the old tables usable | `deploy/tests` with the pinned 0.3 binary |
| M7d | Downgrade to 0.3, which saves `computer-link.json` without `links`, then back to 0.4: `links` is restored and the binding is the same | `computer_link` tests |
| X3 | A grant from a 0.4 desktop redeems on a 0.3 computer, and a 0.3 grant on a 0.4 computer | `computer_link` compat fixtures |
| M1t | Each migration: the backup exists and opens; a forced failure part way leaves the database as before and removes the partial backup; a second run is a no-op; a 0.3 binary opens the migrated database and syncs | `event_store` migration tests, `deploy/tests` |
| M2t | Seeds: copied and read back; a row is kept while its invite is open and deleted once it closes; a failing secret store leaves the row; after deletion no seed text appears in the database file or its WAL (byte scan); a 0.3 binary on the migrated data dir still redeems an invite issued before the upgrade; a person invite never redeemed loses its row after 7 days; a row a 0.3 process rewrites overwrites the store copy | `event_store` tests |
| M4t | A database with `user_version` above the build's opens read-only; the CLI exits 3 | `event_store`, `ligus-cli` |
| S1 | A new inline credential is refused in the desktop, CLI and MCP paths; the rotation notice shows for a log that has one | `app`, `ligus-cli` |
| R1t | Retain route: renews for a member, 404 for an unknown blob or on an old relay; clients renew live attachments and not deleted ones | `langstroth-relay`, `attachment_blob` tests |
| I10 | No authoring path in lt or the CLI reads `local_actor()` or `Core.identity` for the author (a source-level test, like the existing lint tests) | `ligus-cli`, lt |

## 17. Review

One adversarial reviewer, read-only, three passes against the code at
`58cb0e067` (lt `3a4bf58`, relay `3c6a742`).

**First pass: Revise.** The class R and class G compatibility model held at
the fold level (class G is enforced: 0.3 accepts `WorkspaceRulesSet{3}` and
every authoring path refuses once `read_only()`). The rest did not.

| # | Finding | Severity | Resolution |
|---|---|---|---|
| F1 | `binding()`'s "chosen" set could be forged: ingest stores trust-bootstrap kinds from an uncertified signer without checking the signature, so anyone could plant rows "signed by" a device and make it adopt them | Must-fix | `chosen` re-verifies each signature under the device's key; only the install's one person is ever adopted (7.2). Test B4b |
| F2 | The foreign-certificate class was wider than listed: `DeviceLinks` let an Admin borrow another member's identity for private agents, `retired_aliases` handed out a removed member's history, `rotation_recipients` kept a removed member's device reading | Must-fix | Section 1 items 5 to 7; all three read verified signatures or holders, not certificates (7.3). Tests F2a to F2c |
| F3 | A foreign certificate made a phone unrevocable by its own person | Must-fix | 7.3a; test B7 |
| F4 | A held, pre-signed certificate would be refused by `forks_chain` after any other config write, and the grant already carried the keys | Must-fix | 8.1 rewritten: `Offer` and `Ack` first, then passkey and publish, then the keys. Test K1 |
| F5 | Phone and box mixes broke: `PhoneAction` is `deny_unknown_fields`, and phones refuse preimages with fields they don't know | Must-fix | Account named outside the action behind a capability; no hints in phone-drafted events unless advertised (4.2, 7.3); section 12 rows; test X4 |
| F6 | The class R reasoning was wrong (0.3 allows, not denies, a member's `Unknown`; vouching reads class R events) | Should-fix | Section 3 restated; no ingest refusal of class R kinds; X1 adjusted, X1r added |
| F7 | A stolen linked computer could rename its person everywhere and in all past content | Should-fix | Profiles only from non-delegated, non-phone devices with positive certification (5.2). T7b |
| F8 | The name rule was weaker than the cleaner and collisions were under-scoped | Should-fix | `raw == clean(raw)` with a frozen rule set, byte cap, collisions across agents, former members and reserved names (5.2, 5.6) |
| F9 | First-start profile emission let a stale laptop win | Should-fix | Only where no profile exists, after a full sync, from a desktop (5.5, M6) |
| F10 | `MemberRetired` under rules 2 hid a live member; rules 3 semantics were open; hosts | Should-fix | Never hidden from Admins, post-retirement content stays `A_old`'s, no retroactive effect, the computer's host passes to the person (8.3). Test X2 |
| F11 | Migrations: backups copied seeds, deleted seeds lingered, a cursor could be copied to the wrong relay, concurrent 0.3 writers, `links` lost on downgrade, M3 unnecessary | Should-fix | M2 first, secure delete and WAL truncate, single-connection cursor rule, lock and concurrency notes, M7 re-runs, M3 dropped (10.3, 11). Tests M1d, M1c, M7d, M2t |
| F12 | Config-log bloat from avatars | Should-fix | 16 KiB cap; blob-referenced avatars later (5.6, O1) |
| F13 | Endless blob renewals; wrong route path | Should-fix | Absolute maximum age; route under `/v1/workspaces/{id}/blobs` (10.5) |
| F14 | `issuedAtMs` could pin the directory | Should-fix | Clamped to first-stored time plus 5 minutes (5.4) |
| N | Judging position, full fold order, "never finished" wording, a removed person's binding, signed snapshot form, idempotency | Nit | Fixed in 5.2, 5.3, 8.2, 7.2, section 3; idempotency-key lifetime to be checked during implementation |

**Second pass: Approve with risks for 0.4, one Must-fix for rules 3.**

| # | Finding | Severity | Resolution |
|---|---|---|---|
| R1 | "First certificate in fold order is the phone's person" can be steered by naming an older `log_head` | Must-fix | The phone signs a one-time enrollment statement; under rules 3 only certificates carrying it count (7.3a). Test B7b |
| S1 | Person-invite seeds have no close event on the log | Should-fix | Deleted on use or after 7 days (10.3); M2t covers it |
| S2 | 8.3 forbade the repeated `MemberRetired` it asked for | Should-fix | Exempted (8.3) |
| N1 | `DeviceLinks` for an uncontested host; store copy shadowing a newer table row | Nit | The device must have signed as the mapped account; M2 overwrites (7.3, 10.3) |

**Third pass: Approve with risks**, no Must-fix. Folded in: phones enrolled
before 0.4 keep counting under rules 3 while they have a sole certificate,
and can re-consent (7.3a, O6, test B7c); the phone shows the person and
workspace before signing its statement.

**Residual risks.**

- Until rules 3, a member with a passkey can certify someone's phone key and
  leave it revocable only by an Owner; 0.4 warns the person.
- A rotation authored on a 0.3 Admin desktop still seals to a removed
  member's device that someone else certified (T4c).
- 0.3 peers show stale names, "Device xxxx" labels and initials on new
  messages until they update.
- A malicious member can grow the config log with profile events until an
  Admin removes them (T8).
- A requester that acknowledges a link and then disappears holds the keys
  and an unused certificate until the person removes it (8.2).
- A pending person invite older than 7 days loses its seed on a downgrade.

