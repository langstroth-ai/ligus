# Copying from `lt`, including invite codes (design note)

Status: design only, not implemented. Target: `lt` 0.2. Lives in the `ligus`
repository with the other design notes; the code is in
`langstroth-ai/ligus-term`. Ligus is short for *ligustica*, the Italian
honeybee.

`lt` shows an invite code and tells you to "Select it with your terminal to
copy". Terminal selection breaks across wrapped lines and pane borders, and
over SSH it is the only way. This note adds a copy action that works locally
and over SSH (OSC 52), and decides which secrets may be copied at all.

## 1. What exists today (code-verified, `ligus-term` at `a70b9c7`)

- **No clipboard code.** No OSC 52, no clipboard crate, no copy key. Copying
  is the terminal's own selection: `src/ui/overlays.rs:257` "Select it with
  your terminal to copy. Any key closes."; `:462-463` puts the passkey URL
  full width so a selection copies only the URL.
- **No mouse capture.** `src/app/runtime.rs:83-92` enables bracketed paste and
  the kitty keyboard flags only; `:118-122` drops every other event. Raw
  bytes are already written outside ratatui for the bell (`:174-177`).
- **No terminal multiplexer handling.** `src/theme.rs:180-201` looks at
  `COLORTERM`/`TERM` for colour depth only; nothing reads `TMUX`, `STY`,
  `TERM_PROGRAM` or `SSH_*`.
- **Secrets on screen, in full:**
  - The invite overlay (`People` → `I`, `src/keymap.rs:208`; `Overlay::Invite`,
    `src/app/state.rs:242`; drawn at `overlays.rs:249-260`): a `hivews1:` code.
    It is a **bearer secret**: base64url JSON of `WorkspaceConn` carrying the
    workspace key, the epoch keys and a single-use invite id
    (`ligus-runtime/src/workspace_invite.rs:17, 147-155`).
  - The box admission result (`B`, `overlays.rs:53-120`): a sealed
    `ligusadmit1:` code (safe to share) or a bearer code with the warning
    "A bearer code: it carries the workspace key…" (`:106-108`).
  - The passkey remote URL (`overlays.rs:412-421, 464-478`): "a one-time
    secret for this prompt: don't paste it anywhere else."
  - The recovery screen (`src/ui/onboarding.rs:174-213`): 24 words, "shown once".
- **Masked:** only the Connect form's key and relay token fields
  (`onboarding.rs:21-46, 140-149`).
- **Overlays close on any key** (`src/app/update.rs:1115-1118, 1140-1143`).
- **`y` is Memory, `c` is Chats** (`keymap.rs:173, 167`); a key inside an
  overlay goes to the overlay first.
- **Plain mode** prints `invite\t<code>` to stdout (`src/plain.rs:637-638,
  1277-1278`); `admit` without `--out` refuses to print a bearer code
  (`:739-745`).

## 2. What changes

### 2.1 The copy action

- `ctrl-y` copies the focused thing: a message, a code block, a diff hunk, a
  proposal id. The palette gains "Copy …" entries. Copy is never bound to a
  bare letter, so it can't fire while typing.
- In the overlays that show a shareable code, **`y` copies** (the overlay owns
  its keys) and the hint line becomes "y copy · esc close". Those overlays stop
  closing on *any* key: Esc, Enter or `q` close them. This is what makes copying
  possible without the code vanishing on the first keypress.
- Feedback is one quiet status-bar line for 4 seconds, not a toast stack.

### 2.2 How the bytes reach the clipboard

In order:

1. **Native, when `lt` runs on the machine you're looking at**: not over SSH
   (`SSH_CONNECTION` and `SSH_TTY` unset) and a native clipboard answers. On
   macOS the write marks the item **concealed and current-host-only**
   (`org.nspasteboard.ConcealedType`, and the pasteboard's current-host-only
   option, so clipboard managers skip it and it doesn't travel to other
   devices through Universal Clipboard). If the chosen crate can't set those
   markers, a small platform call does; no secret copy without them on macOS.
   Linux: Wayland/X11 through the same crate; no history markers exist there,
   which the confirmation says (2.4).
2. **OSC 52** otherwise: `ESC ] 52 ; c ; <base64> ST`, written to the
   controlling terminal, not to stdout. Inside tmux (`TMUX` set) it is wrapped
   in tmux's passthrough (`ESC P tmux; … ESC \`, with inner `ESC` doubled);
   inside GNU screen (`STY` set), in screen's DCS. lt **never** sends the OSC
   52 query form (`?`), which would ask the terminal to send the clipboard
   back.
3. **Neither**: the status line says "Couldn't copy here. Select it with your
   terminal." The code stays on screen.

OSC 52 has no acknowledgement. When it was used, the line says
"Sent to your terminal's clipboard (if your terminal allows it)."

Payloads over 74,994 bytes (100,000 base64 bytes, a common terminal cap) are
not sent by OSC 52; the line says so. Invite codes are far below this.

### 2.3 What may be copied

| Thing | Copy? | Why |
|---|---|---|
| Messages, code blocks, hunks, ids | yes | not secret |
| Sealed `ligusadmit1:` admission code, `ligusjoin1:` request | yes | designed to be shared |
| Safety codes, fingerprints | yes | public by design |
| `hivews1:` invite | **yes, explicitly**, in its overlay only, with the confirmation below | it is shown in full already; copying is the reason it's shown |
| Bearer admission code | **yes, explicitly**, as the invite | same |
| Passkey remote URL | yes, in its overlay | the point is to open it in a browser elsewhere |
| Recovery words | **never** | write them down; a clipboard copy of the only recovery secret outlives the screen |
| Key and relay token fields | **never** | masked inputs stay masked |

`ctrl-y` on a message whose text contains something that looks like a
`hivews1:` code or an `hrt1` token copies it with the same confirmation as
the invite overlay.

### 2.4 The confirmation for a secret

After copying an invite or bearer code, the overlay (not a toast) shows,
in the warn hue:

> Copied. Anyone who can paste it can join this workspace as a contributor,
> once. Paste it only into a private message to the person you're inviting.

On Linux and over OSC 52 it adds: "Your clipboard may keep a copy."

### 2.5 Clearing

- **Native backend:** 120 seconds after copying a secret, lt clears the
  clipboard **only if it still holds what lt put there** (on macOS by checking
  the pasteboard's change count; on other platforms by comparing content
  without displaying it). The confirmation says "Clears in 2 minutes."
- **OSC 52:** no automatic clearing. lt can't read the clipboard back, and
  clearing blindly could wipe something you copied later.

### 2.6 Configuration

- `copy_secrets = true | false` in `config.toml` (default `true`). With
  `false`, `y` in the secret overlays shows "Copying secrets is off in your
  config." Adding a key to `Config` (`src/config.rs:17-24`,
  `deny_unknown_fields`) makes older `lt` refuse the file; the CHANGELOG says
  so, as for every new key.
- `LT_CLIPBOARD=native|osc52|off` overrides the backend (for terminals that
  mis-handle OSC 52). Documented in `src/help.rs` `ENV`.

## 3. Tests (executable)

| # | Test | Kind |
|---|---|---|
| T1 | The OSC 52 encoder produces exact bytes for ASCII, UTF-8 and an empty payload; tmux and screen wrapping double inner `ESC`; the query form is never produced (grep test over the module plus a unit test). | unit |
| T2 | Backend choice: SSH set → OSC 52; local with a fake native backend → native; `LT_CLIPBOARD=off` → nothing written. | unit with env injection |
| T3 | Recovery screen and masked fields: `ctrl-y` and `y` copy nothing (fake clipboard records no write). | screen test (`tests/screens.rs`) |
| T4 | Invite overlay: `y` copies the exact code and shows the confirmation; any other letter no longer closes it; Esc closes. | screen test |
| T5 | Copied bytes never reach stdout or the log file (capture both while copying). | integration |
| T6 | Native clear runs only if the change count is unchanged; a copy by "another app" in the fake backend prevents the clear. | unit |
| T7 | Payload over the cap is refused with the message; nothing partial is written. | unit |
| T8 | Plain mode and `--json` never emit OSC sequences. | integration |

## 4. Adversarial assessment

Stance: the adversarial reviewer, security lens. The first pass looked at a
draft that copied every displayed string with OSC 52.

### First pass: Revise

1. **Clipboard reach** (High, high). On macOS the clipboard syncs to every
   device on the Apple account and is recorded by clipboard managers; an
   invite there is a workspace key at rest in places nobody audits.
   *Revision: native backend with concealed and current-host-only markers
   (2.2), timed clear (2.5), warning text (2.4), tests T2 and T6.*
2. **Recovery words copied** (High, medium). The draft copied "everything on
   screen". *Revision: never-copy list (2.3), test T3.*
3. **OSC 52 read-back** (Medium, medium). Some terminals answer the query
   form with the clipboard; any code path that sends it would let a crafted
   message leak the clipboard into lt's input. *Revision: never send `?`
   (2.2), test T1.*
4. **Any-key close** (Low, high). Copy keys can't exist while every key closes
   the overlay. *Revision: 2.1, test T4.*
5. **Secrets in logs** (Medium, low). OSC 52 writes the secret through the
   terminal stream. *Considered: the code is already written to the same
   stream in plain text when it's drawn, so terminal recordings already hold
   it; OSC 52 adds base64 of the same bytes. Not a new exposure. Test T5
   ensures lt's own log never gets it.*

### Second pass

#### Decision

**Approve with risks.**

#### Claimed invariants

- C1. lt never copies the recovery words or a masked field.
- C2. A secret is copied only by an explicit key in the overlay that shows it.
- C3. lt never asks the terminal for the clipboard's contents.
- C4. On macOS, a copied secret stays out of clipboard history and off other
  devices, and is cleared after 2 minutes if still there.

#### Hidden assumptions

- Clipboard managers honour the concealed marker (most popular ones do; not
  all).
- The terminal and multiplexer pass OSC 52 to the right clipboard; lt can't
  check.
- `SSH_CONNECTION`/`SSH_TTY` reliably mean "remote" (false inside `mosh`, or
  `sudo -i` that drops the variables: then the native backend writes the
  *remote* machine's clipboard, which is useless but not a leak).

#### Counterexamples considered after revision

| Claim | State | Expected | Likely actual | Severity | Confidence |
|---|---|---|---|---|---|
| C4 | Linux with a clipboard manager | not kept | kept; the warning says it may be | Medium | High |
| C4 | OSC 52 to a local terminal on macOS | not synced | the terminal writes a normal item that syncs | Medium | Medium |
| C2 | A message quoting someone's invite, `ctrl-y` on it | warned | warned (2.3 last line) | Low | Medium |

#### Required tests

T1-T8.

#### Simpler alternatives

Don't copy secrets at all and only make selection easier (a "show the code
alone" view without borders). Safer on paper, but people then use the
terminal selection, which puts the same bytes on the same clipboard with none
of the markers or the clear. Copy-with-markers is safer in practice.

#### Residual risks

- OSC 52 and Linux clipboards may retain the secret; the warning says so.
- Whether to keep `copy_secrets` default `true` is an owner choice.
</content>
</invoke>
