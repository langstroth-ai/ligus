# Sharing files

You can share a file with everyone in a chat. It is end-to-end encrypted with
the workspace key before it leaves your device, so the relay that carries it
sees only ciphertext. Because the relay can't look inside, every check runs on
members' own devices, and nothing a teammate sends opens by itself.

File sharing is **off** in a new workspace. An Admin turns it on in
**Settings → File attachments**. While it is off, an attached file stays on
your device: the message records its path, your own agents can read it, and
other members see only its name.

## Turning it on (Admins)

**Settings → File attachments** holds the workspace's attachment policy. It is
part of the signed workspace rules, so changing it needs an Admin and a
passkey confirmation, and every member's device enforces the same policy.

| Setting | Default | What it does |
|---|---|---|
| Let members share files | off | the switch for the whole workspace |
| Lowest role that may share | Contributor | Viewers can't share unless you lower it |
| Let agents offer files | off | agents never share directly; see [Files from agents](#files-from-agents) |
| Let service members share files | off | `ligus serve` boxes enrolled as their own members |
| Largest file | 25 MB | at most 100 MB; the relay has its own cap (25 MB by default) |
| Blocked types | none | extensions (`exe`) or classes (`@executable`) |
| Only these types | any | when set, only these may be shared |

**Use the recommended policy** turns sharing on for Contributors and above
with programs (`@executable`), installers (`@installer`), disk images
(`@diskimage`) and scripts (`@script`) blocked. The other classes are
`@text`, `@image`, `@audio`, `@video`, `@document`, `@macro` (Office files
that can carry macros, and legacy OLE files), `@archive`, `@markup` (HTML,
SVG and XML, which can run scripts when opened) and `@unknown`.

A type rule applies to the file's name **and** to its content, which each
device detects from the file's bytes. A program renamed `invoice.pdf` is
still a program, and it is refused when it is shared and again when it is
received.

A policy change also applies to files shared before it: once sharing is off,
or a type is blocked, or the size cap is lowered, no device fetches, previews
or saves a file the new policy refuses, and agents no longer get it.

### Blocked files

**Settings → File attachments → Blocked files** lists files that are refused
by the SHA-256 of their content. An Admin can block a file without a passkey,
because blocking only narrows what is allowed; unblocking needs one. A blocked
file is refused when someone tries to share it, and every device refuses to
fetch or save it, including copies shared before the block.

## Sharing a file

Attach the file in the composer as usual. In a workspace that allows you to
share, Ligus checks it against the policy, detects its type from its content,
cleans its name, encrypts it and uploads it to the relay, then signs a
reference to it: your device, the file's SHA-256 and size, its type and its
name. The message carries only a marker that points at that signed reference.

## Receiving a file

A shared file shows as a card in the sharer's message, with its name, size and
sender. Nothing is downloaded, opened or previewed until you ask:

- **Check** downloads the file into Ligus's private storage and verifies it
  against what the sender signed. If the relay swapped, cut short or replayed
  the file, the check fails and nothing is kept. The check also detects the
  real type from the content, flags a name that disagrees with it (a
  `photo.jpg` that is a program) or hides a second extension
  (`report.pdf.exe`), looks inside zip and gzip files without extracting
  anything, and runs your scanner if you set one up. A zip is unpacked into
  a counter, never to disk, so the check judges what an extractor would
  produce: its directory must match its contents, an entry may not unpack
  to more than the directory says, every name an extractor might use counts
  (including Unicode Path names), zips and gzips inside it are checked the
  same way (at most three deep), and the types of all the files inside are
  checked against the workspace's blocked types. Entries Ligus can't open
  (encrypted, or compressed another way) and other archive formats (7z, RAR,
  tar) are flagged, and saving them takes the second confirmation, but the
  types inside them can't be checked.
- **Save…** asks where to put the file, never replaces an existing file, never
  makes it executable, and marks it as downloaded (macOS
  `com.apple.quarantine`, Windows Mark-of-the-Web), so Gatekeeper,
  SmartScreen and Office Protected View check it when you open it. Programs,
  scripts, installers, disk images, macro documents, archives, web pages,
  documents with embedded objects (RTF, Office files with embeddings),
  disguised files and any type Ligus doesn't recognize show a warning, and
  saving them takes a second confirmation that names the sender (their
  display name and the start of their member id, so a copied name doesn't
  pass for someone else).
- **Preview** is offered only for inert formats: PNG, JPEG, GIF, WebP and BMP
  images, and plain text (`.txt`, `.md`, `.csv`, `.tsv`, `.log`, `.json`). The
  preview renders in a sandboxed frame with no scripts and no network access.
  SVG, HTML and PDF are never previewed.

Saving is refused outright when the file's type, or the type of a file inside
an archive, is blocked in the workspace, when a zip's directory doesn't match
its contents,
when an archive looks like a decompression bomb (more than 10,000 entries,
more than 1 GB unpacked, more than 100 times its packed size, more than 16
archives inside it, archives nested more than three deep, or an entry that
unpacks to more than its directory says), or when your scanner reports a
threat.

A card in someone else's message that points at your file, or at anyone's
file but theirs, isn't shown. A file path from another member's device shows
only as text; Ligus never opens a path someone else wrote, nor one in an
agent's reply or in a message your device posted automatically for you (an
automatic pickup).

## Scanning

Ligus can run a scanner on a file during **Check**, before you save it. The
scanner runs on your device on the decrypted file in Ligus's private storage.
Nothing is uploaded anywhere.

| Environment variable | Effect |
|---|---|
| `LIGUS_ATTACHMENT_SCANNER=auto` | use ClamAV (`clamdscan`, else `clamscan`) from the absolute directories on your `PATH` |
| `LIGUS_ATTACHMENT_SCANNER=/path/to/scanner` | run this program with the file's path as its last argument |
| `LIGUS_ATTACHMENT_SCANNER_ARGS` | extra arguments, whitespace-separated, before the path |

The scanner runs without a shell and with a two-minute limit, and because it
reads a file someone else sent, it runs confined: a fresh scratch directory is
its home, temporary and working directory, and on macOS and Linux it gets only
`PATH` and your locale from Ligus's environment. Where Ligus's agent sandbox
runs (macOS Seatbelt, Linux bubblewrap) the scanner also has no IP network
and can write only its scratch directory. `clamd` still works over its local
socket (and on macOS over its default loopback port, 3310; on Linux use its
local socket). A scanner that needs anything more reports a failed scan. Exit status 0
means clean and 1 means a threat was found (ClamAV's convention), which
refuses the save. Any other result is shown as a failed scan and doesn't block
the save. On macOS, XProtect also checks a quarantined file when you open it.

## Deleting a file

The sharer or an Admin can **Delete** a file from its card. The deletion is a
signed event: every member's device stops offering the file, and the deleting
device removes its encrypted copy from the relay. When an Admin removes a
member, Ligus asks whether to delete the files that member shared as well.
A relay that binds files to their uploader may refuse an Admin's delete of
someone else's file; the file is still hidden on every device and the relay
expires it at the end of its retention. The relay also deletes files on its own after its retention period (30 days by
default); see [Operating the relay's file storage](../../operate/networking/self-host.md#file-attachments).

## Files from agents {#files-from-agents}

Agents can't share files. When an Admin turns on **Let agents offer files**,
an agent with the Ligus tools can call `ligus_offer_file` for a file in its
worktree. Ligus runs the same checks as for a person's file, refuses a file
that contains one of the host device's credentials, keeps a copy on the host,
and files the offer as a proposal in the Review pane, which shows the file's
name, type, size and hash. Nothing is uploaded until a person approves the
proposal. Then the host that produced it uploads the approved file, shares it
and posts it in the chat, and every device checks that the shared file is
exactly the one that was approved. An offer always names, and is signed by,
the host that holds the file, so nobody can make another member's device share
something. A file is read only from inside the agent's worktree, and a file
that holds a credential (a private key, an environment file, an access token,
or one of the host's own secrets) is refused, and so is a symlink or a file
with another hard link (which could be a file outside the worktree). If the
workspace stops letting agents offer files before the host shares an
approved offer, the offer isn't shared. A rejected or dismissed offer's
copy is deleted from the host.

`ligus serve` and worker boxes can't keep offered files yet, so their agents'
offers are refused.

## What agents see

When a message an agent reads carries a shared file, the agent gets the file
as clearly delimited untrusted data, with an instruction never to follow
directions inside it or run it. A text file's verified content is included (up
to 64 KB); for any other file the agent gets only its name, type, size and
hash. Agents never get a path to a shared file, so they have nothing to run.
A file path written in someone else's message, or inside a shared file, is
defused before an agent sees it, so it can't make Ligus read a file on your
device.

## On the phone

The iOS app doesn't show shared files yet. When it does, the same rules will
hold: nothing opens automatically, risky types get a warning, and files open
only in the system's document viewer.

See also: [Threat model: shared files](../../operate/security/threat-model.md#shared-files).
