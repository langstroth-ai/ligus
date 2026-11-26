# Admitting a box

A `ligus serve` box or a `ligus worker` box joins a workspace as **its own
service member**. It is never a person's device and never signs as one.
An Admin admits it once, with a passkey, from the desktop, the phone or a
headless Admin. The box then joins by itself, with no person at the box.

## What a box is allowed to do

| | A box (service member) |
|---|---|
| Role | At most **Contributor**. Raising it higher is refused on every device. |
| Votes | Count at its role, but **never** as a person's approval: a human gate (dearth mode, a locked-down chat, an agent's own proposal) still needs a person. |
| Passkeys | It holds none and never carries presence, so nothing that needs a passkey can come from it. |
| Membership | It removes, demotes and admits no one, and revokes no device but its own. |
| Applying | Only to fulfil a person's apply request naming it ([Apply on a host](serve-box.md#apply-on-a-host)), once. There is no auto-apply. |
| Questions and lockdowns | Only a person answers an agent's question or clears a lockdown. |
| Revocation | An Admin revokes its device or removes it like any member. Once revoked it gets no later workspace key. |

A box redeems an **admission**: a single-use invite whose terms say it
admits one service member, until it expires (at most 7 days after it was
issued). The admission names a **redeem key** whose secret half travels only
inside the admission code. The box adds itself, signed by its own device key,
with that key's signature over the workspace, the invite, its member id and
its device. So the invite id, which every member can read on the workspace
log, admits no one by itself: only the holder of the code can redeem it, and
only for its own device. A member re-adding itself can't spend it. A box can't
join with a person's invite, and a person can't join with a box admission.

Every Admin's desktop **closes** an admission once it has settled: as soon as
it sees the admission redeemed (naming that redemption), or ten minutes after
it lapsed unredeemed (naming none), and only once it has pulled everything the
relay holds, so a box whose redemption is still on its way isn't closed out. After that, every other redemption of it is
refused wherever it sorts in the log, including one backdated to look as if
it came before the expiry, or one a thief of the code slots in before the
box's. Closing needs no passkey (it only narrows who may join), and a headless
Admin closes from the CLI with `ligus admit close`.

!!! warning "A box is never a person's device"
    A box never signs as a person: `ligus serve enroll`,
    `LIGUS_SERVE_OWNER` and the chart's `serve.owner` stop with an
    error. Admit the box as described here instead. A box whose
    device key a person's desktop still certifies refuses to start: revoke
    it (**Settings → Profile → My devices → Revoke**) and start it from a
    fresh data dir.

## Start-up: sync first, never found

Both boxes start the same way:

1. The data dir is marked as a box (a service member). This can't be undone.
2. An admission from `LIGUS_ADMISSION_FILE`, or one kept with `ligus admit
   redeem`, is stored for the box to redeem, unless the box is already a
   member.
3. The box **syncs first**. With no copy of the workspace (an empty room, a
   relay it can't reach), it refuses to start and writes nothing. A box never
   seeds a workspace or names itself founder or Owner.
4. It adds itself through the admission. Without one, it prints its join
   request and exits; under systemd or Kubernetes it retries on restart.

## Live: the approval address and a code to compare {#code-match}

From 0.4 a box sends its join request straight to the workspace's Admins,
and both screens show the same short code. There is nothing to paste and no
fingerprint to read out.

1. **Get the approval address.** An Admin prints it (any member can):

    ```sh
    ligus admit address
    ```

    or copies it on the desktop: workspace menu, **Admit a box…**, **Copy**
    beside the approval address. It looks like `ligusaddr1:…`. The address
    **is not a secret and grants nothing**: it only lets a box put a request
    in the Admins' Inboxes, so it can go in a provisioning script or a note.

2. **On the box**, send the request to it:

    ```sh
    ligus admit request --to 'ligusaddr1:…' --label build-1
    ```

    It prints the join request and its fingerprint straight away (an older
    desktop can still [paste it](#paste)), then `Waiting for an approver…`.

3. **An Admin opens it.** The request is in every Admin's Inbox, "A box
   asks to join: “build-1”", and reaches their phones as a push through a paired box.
   Opening it shows a code, and the box prints the same one:

    ```text
    Code: K7Q4 M9X2  (the approver's screen must show the same)
    ```

    On the desktop the sheet **Admit a box?**, with “build-1” beneath it,
    shows the code large, on one line. Compare it with the box. **Codes match** becomes available after 3
    seconds and asks for your passkey; the passkey prompt names the box and
    the code (`Admit box “build-1”, code K7Q4 M9X2`). **Don't match** ends
    the request. You need to be an Admin.

4. **The box joins.** At a terminal it first asks `Does the approver's
   screen show this code? [y/N]`. Then, before it keeps anything, it checks
   that the admission is sealed to this box, is for the workspace and
   founder the address names, and comes from the device that showed the
   code. It syncs and looks for the approval **on the real workspace log**,
   signed by an Admin's device with a passkey, retrying for up to 2 minutes
   while it arrives. Only then does it keep the admission, as `ligus admit
   redeem` would. Start `ligus serve` or `ligus worker` and it joins as its
   own member.

**A headless box** (a systemd unit, a script with no terminal) asks nothing:
the check on the log is what lets it accept, and the Admin's match and
passkey are the human step. A device that can open the request but can't
approve it (not an Admin's, or revoked) can make the box print a code, but no
Admin's screen shows that code and no Admin-signed approval reaches the log,
so the box never joins through it.

The code comes from both devices' keys and a random number from each side,
each fixed before the other side's is seen, so anyone in between (the relay
included) makes the two screens show different codes. It proves the two
screens are in the same exchange. It doesn't prove whose box is at the other
end: approve only a box you are setting up yourself, never a code someone
reads out to you.

The code **is not a secret**. It may be on screen while you share it, and it
appears in the box's output and in `--json` (a `match-code` check). It is good
for one attempt and for 5 minutes after the Admin opens the request. A
request waits in the mailbox for 15 minutes.

### When the codes don't match

Whichever side says so (**Don't match** on the desktop, `N` at the box), the
request ends. Nothing is granted and nothing is written to any log. The box
prints `the codes don't match: nothing was granted` and exits 1; the Admin's
Inbox keeps `Request from build-1 didn't match. Nothing was granted.` for a
day. Someone may have tried to intercept the request: check the box, start
again on a trusted network, and tell an Admin if it happens twice. A box
stops after 3 requests that ended without an approval in 15 minutes, and
says why.

When a second Admin opens a request another is already checking, their sheet
says who ("Ben's desktop is checking this request"). A request opened by a
device that can't approve it is flagged as a possible interception, with
**End this request**.

### Exit codes for `--to`

From the [shared table](../reference/cli-conventions.md#exit-codes):

| Code | Meaning |
|---|---|
| 0 | approved; the admission is kept |
| 1 | the codes didn't match, or the request was declined, ended or expired, or the approval wasn't found on the workspace log, or 3 requests ended within 15 minutes. Nothing was kept |
| 2 | a damaged address, or one whose relay isn't `https://` |
| 3 | the relay has no approval mailbox: paste the request instead (`fix` names the command) |
| 4 | nobody approved within `--wait` (15 minutes by default), or the address is stale: ask an Admin for the current one |
| 6 | the relay refused the request (check the relay access token) |
| 7 | the relay is unreachable, busy or its mailbox is full: retry |

### Reset address

If the address leaks and requests you don't want arrive, an Admin makes a new
one:

```sh
ligus admit address --reset
```

The old address stops taking requests at once; nothing about the workspace
changes, and no key is rotated. Give the new address to whoever sets up boxes.
A workspace key rotation also gives a new address, but members keep reading
the previous two, so an address in a script still works for a while; past
that the box exits 4 and asks for the current one.

### On the phone

Through a box it is paired with, the phone gets a push, "build-1 wants to join
*workspace*". Opened from the push, it asks **Which code does the box show?**
and offers three codes and **None of these**. Tap the code the box shows, then
confirm with Face ID. A wrong code or **None of these** ends the request, and
nothing is granted. Opened from the phone's Inbox instead, it shows the one
code with **Codes match** and **Don't match**, as the desktop does. After 3
requests in 10 minutes the pushes stop and the Inbox shows one row, "Several
join requests".

The phone is the approver: it makes its own random number, computes the code
and signs with its own device key. The box it goes through only carries the
messages, so it can't change the code without the phone noticing. An older
box doesn't offer this, and the phone keeps the [paste flow](#from-the-phone).

### A headless approver

An Admin without a desktop at hand approves from their own CLI identity (never
from a box), confirmed with their recovery key, like `ligus admit issue`:

```sh
ligus approvals list
ligus approvals approve 3f2a…c9 --recovery-words-file ./recovery-words
```

`ligus approvals list` shows each live request: its id, kind, label,
fingerprint and who is checking it. `ligus approvals approve` opens the
request (the box then prints its code), asks `Type the code the requesting
device shows:`, and approves only when what you type equals the code this
device computed (case, spaces and I, L or O for 1 and 0 are forgiven). A wrong
code ends the request. The code exists only once the request is opened, so
it can't be given in advance: without a terminal, `--code -` reads it from
stdin after the open; with neither it exits 4.

## Paste: the box's join request {#paste}

The live path falls back to this when the relay has no approval mailbox (exit
3) or the Admin's desktop is older than 0.4. `ligus admit request --to` has
already printed the join request for it.

On the box:

```sh
ligus admit request --label build-1
```

It prints a join request (`ligusjoin1:…`) and a fingerprint such as
`6D85-7BCB-0AEB-91F2-3C4A`. The request holds only public keys: the box's account id,
its device key and its key-agreement key. The fingerprint covers all of them, so
a request swapped on the way to you (another key to seal the code to, another
account) shows a different fingerprint.

On the desktop, open the workspace menu (the workspace name at the top of the
sidebar) and choose **Admit a box… → From the box's join request**. The same
entry is in the command palette (⌘K, "Admit a box") and on the right-click
menu of the workspace in the rail. Paste the request, choose **Check**,
compare the fingerprint with the one the box printed, then choose **Admit this
box** and confirm with your passkey. You need to be an Admin.

!!! note "The fingerprint stays for one release"
    With no live channel (an older box, a relay without the approval
    mailbox) the fingerprint compare is how you know the request wasn't
    swapped, exactly as before; a short code is never shown for a pasted
    request alone. On the live path, 0.4 still shows the fingerprint under
    **Details** in the sheet and in `--json`; a later release drops it there.
    A 0.4 desktop given a pasted request that is still waiting in the
    mailbox runs the code match for it instead.

The desktop gives you a code (`ligusadmit1:…`) that is **sealed to that box's
key**: only that box can open it, and the admission is bound to that box's
account and device key, so only a redemption that box signs counts. It is
redeemable for one hour. On the box:

```sh
ligus admit redeem 'ligusadmit1:…'
ligus serve        # or: ligus worker --label build-1
```

The box keeps the relay, room and workspace key the code carries (unless it
already has them), syncs, and joins as its own member.

## From the phone, by paste {#from-the-phone}

The phone admits a box with its own device key and your passkey, through a
box it is paired with. The box it goes through is a service member that can't
admit anyone itself: it drafts the admission for the phone to sign, and checks
the phone's signature before appending it. It can hold an admission back, but
it can't forge or change one.

The phone needs its own enrolled device key (a P-256 key in the Secure
Enclave, enrolled by one of your desktops with your passkey), and you need to
be an Admin. The flow: paste the box's join request; check the box's label
and fingerprint; confirm with Face ID (your passkey); the phone signs the
admission with its device key and gets the sealed code to hand to the box.
The serve API side is `POST /v1/admissions/prepare`, `/presence` and
`/commit`, over the relay tunnel or loopback only (the answer carries the
code); see the [serve API](../reference/serve-api.md#admissions).

!!! note "Phone app support"
    The serve endpoints ship in 0.1. The phone app's screen for them comes
    with its own device key; until your phone has one, admit boxes from the
    desktop.

## Kubernetes and other headless boxes: an admission Secret

A pod has no terminal to paste a request into, so the Admin makes a
**bearer** admission instead. It isn't bound to a box until the first box
redeems it. This path is unchanged in 0.4: the chart doesn't send a request
to an approval address, and an admission Secret is how a pod joins.

1. On the desktop: **Admit a box… → For Kubernetes**. Choose how long it stays
   redeemable (24 hours by default, at most 6 days), then **Make an admission
   code**, and confirm with your passkey. Make one per box: each is single use.
2. Store each code in a Secret in the workspace's namespace:

    ```sh
    kubectl -n ligus-ws-acme create secret generic acme-serve-admission \
      --from-file=admission=./serve-admission.txt
    kubectl -n ligus-ws-acme create secret generic acme-build-1-admission \
      --from-file=admission=./build-1-admission.txt
    ```

3. Name them in the chart's values:

    ```yaml
    serve:
      admission:
        existingSecret: acme-serve-admission
    worker:
      enabled: true
      instances:
        - name: build-1
          admission:
            existingSecret: acme-build-1-admission
    ```

Each box reads its code from `LIGUS_ADMISSION_FILE` (a file mounted `0400`,
read-only, which agent processes can't read and the sandbox preflight never
sees), redeems it at start-up and joins. **Delete the Secret once the box has
joined.** The volume is optional, so the pod keeps running, and a restart
doesn't need the code again.

!!! danger "A bearer admission is a secret"
    Until an Admin's device has closed it, whoever holds the code can join as
    a box, and the code also carries the workspace key. Keep it in a Secret,
    never in a values file or a ConfigMap, give it a short lifetime, and
    delete it after use. Watch **People** for a service member you didn't
    expect, and remove it. Never pass a bearer code on the command line:
    `ligus admit redeem` takes it only from `--file PATH` or standard input
    (`-`), so it never shows in `ps` or shell history.

Outside Kubernetes the same file works: put the code in a file only the box's
user can read and set `LIGUS_ADMISSION_FILE` to its path, or run
`ligus admit redeem --file PATH` once.

## A headless Admin (CI, scripts)

`ligus admit issue` issues an admission from a person's own CLI identity (never
from a box), confirmed with that person's **recovery key** instead of a
passkey:

```sh
ligus admit issue --recovery-words-file ./recovery-words --ttl 2h --out serve.code
ligus admit issue --recovery-words-file ./recovery-words --request 'ligusjoin1:…'   # sealed to that box
ligus sync
# once the box has joined, or the admission has lapsed:
ligus sync && ligus admit close && ligus sync
```

`ligus admit close` closes this Admin's workspace's settled admissions (the
desktop does it on its own after every sync). For a box that asked through the
approval address, use `ligus approvals approve` instead
([A headless approver](#a-headless-approver)).

A workspace founded from the CLI registers a recovery key as its founder's
first credential with `ligus recovery-key create --out ./recovery-words`. The
CI job `kind-e2e` uses this path to found a throwaway workspace and admit
serve and a worker into it. The recovery words are as powerful as your
passkey: use this only for throwaway or automated workspaces, keep the file
off shared disks, and prefer the desktop or the phone. The words file must be a regular file only its owner can read
(`recovery-key create` writes it `0600`): `--recovery-words-file` refuses
one others can read, a directory or a FIFO.

## Revoking or replacing a box

A box is removed like any member below Admin: in a chat's **People** panel
(the right rail), remove the box's member, and confirm with your passkey. Its
device can also be revoked on its own (`DeviceRevoked`, an Admin's act, the
same rule as any device). Either way every device then refuses what it signs
from that point, and the next workspace key rotation leaves it out, so it
can't read new traffic. A revoked box goes read-only. To bring it back, start
it from a fresh data dir and admit it again.

**Removing a box from the relay roster.** A box bootstrapped from **People →
Remote agents** has a relay identity of its own (`ligus-box:<id>`), and the
desktop that made the bootstrap issued the box's admission with it and
remembers which admission goes with which identity. Removing that identity
under **People → Relay members** on that desktop:

1. removes it from the relay;
2. removes the box's service member from the workspace (the member the
   admission's redemption added, never matched by name), with your passkey;
   for a box that never joined, closes its admission instead, so it can't
   join with it later;
3. rotates the workspace key, sealed to the members who remain, so the box
   **loses access to new traffic** from the rotation on. It keeps what it
   had already synced.

If removing the member fails (you cancel the passkey, say), the key isn't
rotated and the error says so: finish with **Remove & revoke** on the box's
member under Members. Another Admin's desktop, or a box whose relay identity
was minted another way (`langstroth-relay bootstrap-agent`, a sign-in token,
a desktop before 0.1.1), has no such link: the removal says the entry matches
no workspace member, and you remove the box's member under Members.

## Troubleshooting

| The box says | What to do |
|---|---|
| `has no copy of the workspace '…' yet` | Check `LIGUS_WORKSPACE`, the relay URL and the relay token. If the workspace is new, sync the Admin's desktop first. |
| `isn't a member … it holds no admission` | Admit it (above), then restart it. |
| `a service member joins only through a box admission` | The code is a person's invite. Use **Admit a box**, not **Copy invite**. |
| `that box admission has expired` / `that box admission was closed` | Make a new one. |
| `that code isn't a box admission` | The code is a person's invite (or from an older build). Use **Admit a box**. |
| `a box admission is redeemed with its code's proof` / `that redemption's proof doesn't verify` | The box joined without the code's redeem key: redeem the code again with `ligus admit redeem`. |
| `that box admission is for another box` | The sealed code was made for another box's join request. |
| `a box joins under its own account` / `a box admission is redeemed by its own certified device: …` | The redemption wasn't the box's own (signed by another device than it names, or a member record naming another account): not something a Ligus box writes. Remove whatever joined and issue a new admission. |
| `this admission code was sealed to another box` | Same: redeem it on the box that printed the request. |
| `that admission is for workspace 'X', but this box serves 'Y'` | The code and `LIGUS_WORKSPACE` name different workspaces. |
| `this relay has no approval mailbox: paste the request instead` (exit 3) | The relay doesn't offer the approval mailbox. Paste the join request `--to` already printed ([Paste](#paste)). |
| `no approver answered within --wait` (exit 4) | Ask an Admin to open it in their Inbox, or paste the join request printed above. |
| `the approval address was reset` / `the approval address isn't registered on this relay` (exit 4) | Ask an Admin for the current address (`ligus admit address`). |
| `the approval wasn't found on the workspace log (…): nothing was redeemed` | The Admin's desktop hasn't published the approval yet, or it didn't come from an Admin's device with a passkey. Check their desktop is online and synced, then start again. |
| `the grant names another founder than the approval address pins` / `the grant is for another workspace than the approval address names` | The admission points at a copy of the workspace, not the real one. Nothing was kept. Tell an Admin. |
| `3 requests ended without an approval in the last 15 minutes: stopping` | Check the box and the network, then start again on a trusted network. |
