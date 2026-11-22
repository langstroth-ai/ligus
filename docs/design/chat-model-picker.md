# Picking the model behind a chat

Status: built for the desktop (this change). The phone app and `lt` don't
have it yet; see [Other clients](#other-clients).

A runtime is a harness plus a model: Claude Code with `opus`, Copilot CLI
with `gpt-5.3-codex`, an Ollama server with `qwen3-coder`. Until now the only
way to change the model was to edit the runtime in Settings, which changed it
for every chat, and nothing checked the name: a model the account can't use
(for example a Copilot model the plan doesn't include) failed only when a turn
ran, as a failed reply. This note covers choosing the model per chat while
chatting, where the list of models comes from, and what is checked before a
message is sent.

## What the person sees

The route pill under the composer (`→ @ligus · via …`) already opens a list
of runtimes. Each runtime row gets a quiet `›` that opens a second step for
that runtime: **Default** (the runtime's own model) at the top, then the
models the harness offers, then **Other…** to type a name. The choice is
remembered for that chat and shows in the "Runs on" line under the chat
title (`Runs on MacBook · copilot / gpt-5.3-codex`). Nothing new is
always visible; the second step is reached from the popover only, and it
works from the keyboard (arrow keys, Enter, Escape goes back a step).

If the model turns out to be unavailable, the composer shows one line
(`gpt-6-sol isn't available to this Copilot account.`) with a **Pick another
model** action that opens the second step, instead of a failed reply.

## Rules

1. **Per chat, per runtime, on this device.** The choice is stored in this
   device's settings (`settings.json`, `chat_models: {chat id: {runtime id:
   model}}`). It is keyed by runtime, so switching the chat to another
   runtime and back brings the choice back, and a choice made for Copilot is
   never handed to Codex. Clearing it (Default) removes the entry. Deleting
   the chat removes its entries.

2. **It applies only to turns this device runs.** The override is read when
   this device builds the turn (`responder_for` and `rebind_to_catalog`, the
   same place the runtime is resolved), never sent anywhere. Turns that
   another device runs use that device's own settings, so a choice here can
   never change another member's agent or spend their quota. In a shared
   chat the untagged turns are answered by the device of the person who
   created the chat; when that isn't this device, the second step says so
   ("Untagged messages here are answered on another member's device, with its
   own model") and the choice waits until this device answers. When this
   device does answer a teammate's untagged message, it answers with this
   device's choice, under this device's login, as it already does with this
   device's runtime settings.

3. **@-mentioned agents keep their own runtime and model.** The override is
   for `@ligus` (untagged messages) only. An agent may run on another device
   or host under its owner's login, so overriding it from here would either
   do nothing or spend someone else's quota. There is no per-message
   override: it would need a new field on the synced message for every
   device to honor, and the per-chat choice covers the need. Changing an
   agent's model stays where it is (the agent's runtime).

4. **No sync, no new event.** Making the choice follow the person to their
   other devices would need a new synced event type (the chat's runtime is a
   synced event; a per-member model on it is not). The choice stays
   device-local for now. If that changes, it should be a new, additive event
   kind keyed by member, never a change to `SessionRuntimeChanged`.

5. **What a turn records.** A reply already carries `runtime_id`. The
   existing optional `runtime_label` field on the reply (present in the
   message format, never written until now) now carries the model the turn
   ran with, when it is known. The transcript labels a reply with that model,
   falling back to the runtime's current model for older replies. Older
   clients already parse the field and ignore it; no format, signing context
   or stored id changes.

## Where the list comes from

Each list is read-only and spends no quota: no prompt is sent. CLI listings
run with a timeout (20 s for `agy models`, 15 s otherwise) and stdin closed;
HTTP listings use an 8 s timeout. Results are cached in memory for five
minutes per runtime (a refresh button in the second step bypasses the
cache). A failure shows "Couldn't list models" with the reason and leaves
**Default** and **Other…** usable.

| Runtime | Source | Complete? |
|---|---|---|
| Claude Code | The CLI's aliases (`sonnet`, `opus`, `haiku`) plus the account's cached options from `~/.claude.json` (what the CLI's own `/model` shows), the existing Settings list | no |
| Codex | `codex debug models` (the CLI's model catalog as JSON; models it marks `list`) | no |
| Copilot CLI | `copilot help config`, the `model` setting's choices (the catalog of the installed CLI version) | no: the plan may not include every model, and Copilot has no read-only, no-quota way to ask |
| Antigravity (`agy`) | `agy models` (signed-in account's models) | yes |
| Ollama | `GET /api/tags` on the runtime's server | yes |
| OpenAI-compatible (OpenAI, OpenRouter, LM Studio, llama.cpp, gateways on a URL) | `GET …/v1/models` with the runtime's key | yes for a local or private server, no for a hosted API |
| Anthropic API | `GET /v1/models` with the key | no |
| A named model gateway, pi, aider, Hermes | none: **Other…** only | — |

"Complete" lists are the ones a send is checked against.

## Checked before sending

The model a turn will use is the chat's choice for its runtime, else the
runtime's model. Before an untagged message is sent from this device, the
composer stops it with the one-line notice when that model is

- one this device saw fail with "model not available" (any runtime), or
- missing from a complete list (`agy`, Ollama, a local OpenAI-compatible
  server).

Incomplete lists never block: Copilot's catalog includes models a plan may
not have, and an unknown name may be a custom or new model. The first
failure teaches the device: when a turn fails with a "model not available"
error (Copilot's "Model is not available", Ollama's "model … not found",
OpenAI's `model_not_found`, …) the desktop remembers that runtime and model
for the rest of the session, marks it in the list, and shows the notice. A
person can still pick it again (a plan may change); picking it clears the
mark.

The same lists and the same check are offered in Settings → Models &
runtimes, on the runtime form's model field and on the Claude Code model
selector.

## Other clients

- **Phone (iOS) and `lt`** would need: the `list_models` equivalent on
  `ligus serve` (a `GET /v1/runtimes/{id}/models` route returning the same
  `{models, source, complete, note}` shape), a per-chat choice stored on the
  serve box keyed by chat and runtime (the box is the device that runs those
  turns), and the same "model not available" classification on the box's
  turn errors. The phone picker would sit in the chat's runtime sheet.
- Until then a phone or `lt` message in a chat with a desktop choice is
  answered with that choice only when the desktop runs the turn.
