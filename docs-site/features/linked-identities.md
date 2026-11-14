# Linked accounts

A **linked account** tells the members of a workspace that you are also
someone on a code forge: "this Ligus account is also `alice` (id 1234567) on
GitHub". It helps people recognize each other. It is optional, off until you
add one, and you choose the workspaces it is shown in.

A link proves one thing: **the forge says so**. Every device that shows it
checks it with the forge itself. A link never lets anyone into a workspace,
never gives a role, and never makes Ligus trust a key for sealing: those come
only from the workspace's own signed log and your safety code.

## Supported forges

| Forge | How Ligus checks it | Stable id | Fallback |
|---|---|---|---|
| GitHub (and GitHub Enterprise added on your device) | `GET /user/{id}` → login, `/users/{login}/keys` and `/ssh_signing_keys`, then `/user/{id}` again: the login must not change during the check | numeric user id | add your Ligus key as a **signing key** |
| GitLab (gitlab.com and self-managed) | `GET /users/{id}/keys`, by id | numeric user id | add your Ligus key with usage type **Signing** |
| Codeberg, Forgejo, Gitea | `GET /api/v1/users/search?uid={id}` → login, `{base}/{login}.keys`, then the uid search again | numeric user id | none: keys there grant push access |
| SourceHut | `https://meta.sr.ht/~{user}.keys` | none public: links are **bound to the name** | none |
| Tangled | DID document (`plc.directory` or `did:web`) → its PDS → `com.atproto.repo.listRecords` of `org.tangled.key.sshKey` (and, best effort, the legacy `sh.tangled.publicKey`) | atproto DID | none |

Every request is anonymous (no token, no cookie), goes straight to the
forge (never through your relay), follows no redirect, gives up after 10
seconds or 256 KiB, and goes only to the forges listed above or the
self-managed ones you added on that device. Tangled support follows the
`org.tangled.*` lexicon that replaced `sh.tangled.*` on 2026-10-30; it is best
effort while that lexicon settles.

## Link an account

Settings → **Profile** → **Linked accounts** → **Link an account**:

1. Pick the forge and type your username (SourceHut: `~name`; Tangled: your
   handle or DID). Ligus asks the forge for your stable id and shows
   "alice (id 1234567)".
2. Tick the workspaces to show it in. Optionally also put it on this
   device's contact card (people who have your card see it).
3. Ligus drafts one **statement** per workspace. Each names your account,
   your account key, that workspace, the forge, the stable id and a validity
   of one year. See [the statement reference](../reference/identity-link-statement.md).
4. Sign each statement with an SSH key the forge lists for you:
    - **With your ssh-agent:** Ligus lists the agent's keys that the forge
      also lists; pick one (a hardware key asks for a touch).
    - **Yourself:** copy a statement and run

      ```sh
      ssh-keygen -Y sign -n ligus-identity-link-v1@ligus.ai -f ~/.ssh/id_ed25519 < statement.txt
      ```

      then paste what it prints.
5. **Publish.** Ligus checks the signature and asks the forge once, then
   publishes the link to each workspace's log.

The namespace `ligus-identity-link-v1@ligus.ai` is what keeps this safe:
an SSH signature made for git commits or for "sign this file" never passes as
a link, and a link signature never passes as one of those.

Accepted keys: `ssh-ed25519`, `sk-ssh-ed25519@openssh.com`,
`ecdsa-sha2-nistp256`, `sk-ecdsa-sha2-nistp256@openssh.com` (a security key's
signature must carry user presence) and RSA of at least 3072 bits
(`rsa-sha2-512`).

### The signing-key fallback (GitHub and GitLab)

Instead of signing, you can add your **Ligus account key** to the forge as a
*signing* key; Ligus gives you the `ssh-ed25519 …` line to paste. It goes on
the signing list, not the SSH (push) keys, so it grants no access. But it
puts your Ligus key on your public forge profile: **anyone can see that your
Ligus account and that forge account are the same person**, for as long as
the key stays there. The app says this before you choose it. It is not
offered on Codeberg, Forgejo, Gitea or SourceHut, where a listed key can push.

## What people see

Links show as chips beside a person on **People**, and in your own list:

| Chip | Meaning |
|---|---|
| **verified** | This device asked the forge (within the last 24 hours) and it lists the key that signed the link, under the stable id. The chip shows the forge's *current* handle with the id beside it. |
| **verified by name** | SourceHut: the forge lists the key under that name, but SourceHut has no public stable id, so a reused name could match. |
| **stale** | The forge no longer lists the key (removed or rotated), the forge account is gone, or your account key changed since you signed. Re-link to fix it. |
| **expired** | The link's year ran out. |
| **unverified** | Not checked yet, the forge couldn't be reached, forge checks are off on this device ("signature valid"), or the forge isn't one this device asks. |
| **self-asserted** | A forge without a public key list (2.x). Never shown as verified. |

A link whose statement or signature doesn't check out isn't shown at all.

## Privacy

- **Off by default**, and per workspace: a link is an event on that
  workspace's log, so only its members (Viewers included) see it. **Agents
  in that workspace can read it too.**
- **Checking reveals you to the forge.** When your device checks a
  co-member's link, the forge sees your IP address and the time. Ligus asks
  at most once a day per link (a cache on your device), and only when you
  open People or your profile. Turn **Contact forges to check linked
  identities** off in the same section to never ask; links then show
  "signature valid" and are never marked verified.
- **No forge images.** Ligus never loads a forge avatar or page for a link;
  chips are text.
- **Check results stay on your device.** They are never synced: another
  member's device reaches its own answer from the forge.

## Removing and renewing

- **Unlink** removes the link from every workspace it is shown in, and from
  your contact card. An Admin can remove a member's link (moderation), never
  one of a member above them.
- Linking the same forge account again replaces the old link.
- If you remove the key from the forge, the link goes stale within a day on
  every device (at once for anyone who presses **Check now**).
- A link lasts one year (two at most); re-link before it expires.

## Self-managed forges

Under Linked accounts, add a GitLab, Forgejo, Gitea or GitHub Enterprise by
its `https://` address. This is device-local: a co-member's link that names a
forge your device doesn't know shows as "not checked", so nobody can make
your device contact an address you didn't add.
