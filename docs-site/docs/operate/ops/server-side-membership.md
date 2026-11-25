# Server-side workspace membership

The open-source relay can keep a roster for a workspace and refuse reads and
writes from callers who aren't on it. It is off unless the operator turns it
on with `LANGSTROTH_RELAY_MEMBERSHIP=1`, and even then it only applies to
workspaces someone has **claimed**.

> **Deprecated for Ligus workspaces (Ligus 0.4).** Ligus never reads the
> relay roster: every device enforces the signed roster in the workspace
> log. A relay roster on a Ligus workspace is a second list that can drift
> from it (someone removed in Ligus can stay on the relay's list, or the
> other way round), and the relay can't check one against the other because
> it holds no workspace keys. Gate a Ligus relay with signed tokens and
> workspace scope instead. Relay membership stays a supported feature for
> other clients.

## Why a relay-side roster

The relay is content-blind: it forwards end-to-end encrypted envelopes. The
workspace's real membership lives in the signed roster that every device
folds and enforces. Removing a member re-keys the workspace, so they can't
*read* new traffic, but without a relay-side check a removed member's client
could keep *writing* ciphertext into the room for the other devices to drop.

A relay-side roster closes that gap: the relay identifies the caller and
refuses writes (and reads) from accounts that aren't members of a claimed
workspace. The roster is deliberate, bounded metadata (account ids and coarse
roles), never content.

## How it works

**Identity.** The relay keys the roster on the caller's token subject (`sub`),
so it needs a signed `hrt1` token to know who is calling: either the relay's
access gate uses signed tokens (`LANGSTROTH_RELAY_TOKEN_PUBKEY`), or
`LANGSTROTH_RELAY_MEMBERSHIP_PUBKEY` names an issuer key whose tokens identify
callers while access to the relay itself stays open. A caller with no
identity can't claim or change a roster.

**Membership is opt-in per workspace.** A workspace with no roster is
*unclaimed* and behaves exactly like the open relay. The first identified
**claim** makes the caller `owner` and turns enforcement on for that
workspace:

```text
POST   /v1/workspaces/:id/members/claim      -> caller becomes owner (409 if claimed)
GET    /v1/workspaces/:id/members            -> list (members only)
POST   /v1/workspaces/:id/members            -> add / set role {account, login, role} (admin+)
DELETE /v1/workspaces/:id/members/:account   -> remove (admin+)
```

**Roles** mirror the client's: `viewer < contributor < admin < owner`.
Every member, `viewer` included, may write envelopes; `admin` and above
manage the roster and invites; only an `owner` may grant or change `owner`,
and the last owner can't be removed or demoted.

**Enforcement.** On a claimed workspace, any member may read and may write
`envelopes`, `presence`, `candidates` and blobs; posting a key rotation
(`keyring`) needs at least `contributor`; anyone else is refused. Viewers
must be able to write: a Viewer publishes its key-agreement key, its read
state and its attachments as envelopes, and without its key-agreement key it
never receives a key rotation. The app still decides what a Viewer may
author, so a Viewer's writes are checked by every device, whatever the relay
accepts.

Relays before v0.3.2 required `contributor` for every write, which refuses a
Viewer's envelopes on a claimed workspace, so a Viewer there never gets the
rotated key. Run v0.3.2 or later if your workspaces have Viewers.

**Removing someone = remove + rotate.** `DELETE …/members/:account` stops
their access on the relay at once. Removing an entry from **People → Relay members** in the app does three things in order: removes it on the relay,
removes the matching member from the workspace roster, then rotates the
workspace key to the members who remain, so they also lose the key to new
traffic. The app matches a person by their account (`ligus:<account id>`)
and a box by the admission the bootstrapping desktop issued with its
`ligus-box:<id>` identity, never by name. If the removal from the roster
fails, the key isn't rotated and the app says so. An entry the app can't
match to a workspace member is still removed from the relay and the key
rotated, but the app warns that it may still read new traffic: remove its
member under Members (**Remove & revoke**). See
[Admitting a box](admitting-a-box.md#revoking-or-replacing-a-box).

## How it composes with access tokens

Two independent checks:

| Check | Question |
|---|---|
| **Access token** (`Authorization: Bearer`) | may this client use the relay at all? See [Self-hosting a relay](../networking/self-host.md#access-control) |
| **Membership** | is this account a member of *this workspace*, with what role? |

The signed roster in the workspace stays authoritative for the app: a relay
roster only decides what the relay forwards. Governance events from outside
the signed roster are refused by every client, whatever the relay accepts.

## Where it lives

All of it is in the open-source relay (`members.go` in the relay
repository): the roster store (memory, snapshot or Postgres), the claim and
roster routes, and a built-in guard on the relay's generic write and read
guard seams. Rosters persist in the relay's store like the rest of its state.

## Planned

- Client wiring: the desktop People rail driving the relay roster routes
  when a team workspace is created.
- Capability grants (for example `remove_member`, `rotate_key`) carried in a
  signed token's `caps`, for finer rules than role rank alone.
