# Set up Ligus

**Set up Ligus** is one checklist of your setup in a workspace, the same on
every surface: the desktop's **Settings > Set up Ligus**, `ligus setup` in a
terminal, lt's setup screen and the phone's **Health**. One line per item,
each with a state and at most one next step. **Clean up** at the end lists
what's left over, and nothing is removed until you confirm it.

## The rows

```
✓ You          Ana · picture
✓ Passkey      2 passkeys · recovery key
✓ This device  Ana's MacBook
• My devices   this Mac, build-1, old-laptop · 1 approved, never used    Review
✓ Phone        Ana's iPhone · push on
✓ Box          box-1 · 0.4.1 (0.5.0 out)                                  How to upgrade
✓ Agents       @coder on box-1 · @reviewer on your devices
✓ Workspace    Team · rules 3 · Upgrade available                         Upgrade
• Clean up     1 member who asked to leave · 1 device never finished      Clean up
```

| Row | Shows | Its one action |
|---|---|---|
| **You** | your profile name, and whether it has a picture | **Set your name** when you never set one |
| **Passkey** | how many passkeys you hold and whether you saved a recovery key | **Add a passkey**, or **Save a recovery key** when you have passkeys but no recovery key |
| **This device** | the name every member sees for this device | **Name it** when it has none |
| **My devices** | your other computers, terminals and boxes; a dot when one was approved and never used, or has been idle for 30 days | **Add a computer** when this is your only device, **Review** otherwise |
| **Phone** | your phones, and push when the surface can tell (the phone and its box can; the desktop can't) | **Add a phone**; **Turn on push** on the phone when it's off |
| **Box** | the workspace's always-on boxes and workers, their release, a newer one when there is one, and offline | **Add a box** (optional), **How to upgrade** when a newer release is out: run `ligus-local upgrade` on the box. Nothing upgrades a box on its own |
| **Agents** | each agent and where it answers: a host's name, "your devices", or a teammate's devices; why one doesn't answer ("offline", "host removed") | **Add an agent** when there are none, **Review** when none of them would answer now |
| **Workspace** | the workspace's name and its rules version | **Upgrade** for Owners when this build supports a newer rules version (Settings > Passkeys & keys) |
| **Clean up** | how many leftovers there are, by kind | **Clean up** |

The marks:

- **✓** done.
- **○** grey, **Not set up**: something you never chose. Some rows are
  optional (a box, a phone). Nothing is ever shown in red for not having it.
- **•** amber: something you chose wants a look.

The rows come from what this device already knows: the workspace's log, this
device's own store and the beats the hosts send. Building them uses no network,
so the checklist opens instantly and works offline.

## Clean up

Clean up gathers what an older workspace collects over time:

| Leftover | Action |
|---|---|
| A member who **asked to leave** | **Remove** (Admins and Owners, with a passkey), otherwise **Remove from view** |
| A member **retired** into their person (a duplicate after linking) | **Remove from view** |
| One of your devices **approved, never used** or **idle** for 30 days | **Remove** (it's revoked, with your passkey) |
| One of your devices that was **revoked** | **Remove from view** |
| A host whose **device was revoked** (or, in a workspace still on rules 2, a former member's host; rules 3 already takes those out of every list) | **Remove** (Admins, with a passkey; agents placed on it keep saying the host is gone) |
| One of your phones a box **unpaired**, or that **forgot** a box, while it stays your device (desktop; read from the boxes' reports when you open Clean up) | **Remove** (it's revoked, with your passkey); the Inbox's **Keep** stops asking |
| An **old agent name**: a roster agent called `hive` (from the Hive builds) or `ligus` | **Remove** (its owner or an Admin, with a passkey) |

Every action asks first, in plain words, and a **Remove** then asks for your
passkey: each removal is its own ceremony. **Remove all** walks through the
removable ones one after another ("Confirm 1 of 3"), still one passkey each.

**Remove from view** writes nothing in the workspace: the member is still
listed for everyone else and the device stays revoked. It only hides the line
on this device (in `setup-hidden.json` in the data dir, and in People and My
devices on the desktop).

## On each surface

=== "Desktop"

    **Settings > Set up Ligus** (the first item under **You**). Also: the
    quiet **Set up Ligus** link on the empty Home, **⌘K** > **Set up Ligus**,
    and **See what's left to set up** at the end of the first run. Each row's
    button opens the place that does it (Profile, Passkeys & keys, My devices,
    Add a phone, the Add a box or Add an agent setup). Clean up opens in place.

=== "Terminal (`ligus`)"

    `ligus setup` prints the checklist, each row's next step as a command
    (or where to do it when only another device can), then the guided setup
    flows:

    ```console
    $ ligus setup
    Set up Ligus

    ✓ You          Ana · picture
    ○ Phone        Not set up                               Add a phone: ligus setup phone
    ...
    ```

    `ligus setup --json` is the flows' `ligus.checks.v1` report with the
    checklist under `checklist` (`ligus.setup.v1`): `rows[]` with `id`,
    `title`, `state` (`done`, `notSetUp` or `attention`), `detail` and an
    optional `action {id, label}`, and `cleanup[]`.

    `ligus setup cleanup` lists the leftovers; `ligus setup cleanup --hide
    KEY` removes one from view on this device and `--show KEY` brings it back.
    A removal from the workspace is done on your desktop, with your passkey.

=== "lt"

    lt's setup screen shows the same rows, and `lt setup --json` carries the
    same `checklist` document.

=== "Phone"

    **Health** shows the same rows for you, from the box the phone is paired
    with (`GET /v1/setup`, see the
    [serve API](../../operate/reference/serve-api.md#setup)). "This device"
    is the phone, and push comes from the box's own pairing. Clean up lists
    the leftovers; removing one is done on your desktop.
