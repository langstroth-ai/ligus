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
    it (**Settings → Account → My devices → Revoke**) and start it from a
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

## From the desktop, with the box's join request

On the box:

```sh
ligus admit request --label build-1
```

It prints a join request (`ligusjoin1:…`) and a fingerprint such as
`6D85-7BCB-0AEB-91F2-3C4A`. The request holds only public keys: the box's account id,
its device key and its key-agreement key. The fingerprint covers all of them, so
a request swapped on the way to you (another key to seal the code to, another
account) shows a different fingerprint.

On the desktop, right-click the workspace in the rail and choose **Admit a
box… → From the box's join request**. Paste the request, choose **Check**,
compare the fingerprint with the one the box printed, then choose **Admit this
box** and confirm with your passkey. You need to be an Admin.

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

## From the phone

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
redeems it.

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
desktop does it on its own after every sync).

A workspace founded from the CLI registers a recovery key as its founder's
first credential with `ligus recovery-key create --out ./recovery-words`. The
CI job `kind-e2e` uses this path to found a throwaway workspace and admit
serve and a worker into it. The recovery words are as powerful as your
passkey: use this only for throwaway or automated workspaces, keep the file
off shared disks, and prefer the desktop or the phone.

## Revoking or replacing a box

A box is removed like any member below Admin: in a chat's **People** panel
(the right rail), remove the box's member, and confirm with your passkey. Its
device can also be revoked on its own (`DeviceRevoked`, an Admin's act, the
same rule as any device). Either way every device then refuses what it signs
from that point, and the next workspace key rotation leaves it out, so it
can't read new traffic. A revoked box goes read-only. To bring it back, start
it from a fresh data dir and admit it again.

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
