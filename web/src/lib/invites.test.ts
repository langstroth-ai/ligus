import { afterEach, describe, expect, it, vi } from "vitest";
import { copyWhenReady } from "./clipboard";
import { NO_RELAY_MESSAGE, inviteError, isNoRelayError } from "./invites";
import { OPEN_SETTINGS_EVENT } from "./nav";

class FakeItem {
  constructor(public items: Record<string, Promise<Blob>>) {}
}

afterEach(() => {
  vi.unstubAllGlobals();
});

// What the desktop's workspace_invite returns for a workspace with no relay.
const BACKEND_ERR =
  "workspace-no-relay: This workspace has no relay, so an invite to it can't be used. Set one under Settings → Team sync (Relay URL), then copy a new invite.";

describe("relayless invites", () => {
  it("recognizes the backend's typed error, and only it", () => {
    expect(isNoRelayError(BACKEND_ERR)).toBe(true);
    expect(isNoRelayError(new Error(BACKEND_ERR))).toBe(true);
    expect(isNoRelayError("Only a workspace Admin can invite people (x).")).toBe(false);
    expect(isNoRelayError(undefined)).toBe(false);
  });

  it("maps it to a plain message with a way to Settings → Team sync", () => {
    const opened: string[] = [];
    vi.stubGlobal("window", new EventTarget());
    window.addEventListener(OPEN_SETTINGS_EVENT, (e) => opened.push((e as CustomEvent<string>).detail));
    const m = inviteError(BACKEND_ERR);
    expect(m.message).toBe(NO_RELAY_MESSAGE);
    expect(m.message).not.toContain("workspace-no-relay");
    expect(m.action?.label).toMatch(/Team sync/);
    m.action!.run();
    expect(opened).toEqual(["team-sync"]);
  });

  it("passes other errors through unchanged, with no action", () => {
    const m = inviteError(new Error("unknown workspace"));
    expect(m).toEqual({ message: "unknown workspace", action: undefined });
  });

  it("copies nothing when the invite is refused", async () => {
    const written: string[] = [];
    vi.stubGlobal("ClipboardItem", FakeItem);
    vi.stubGlobal("navigator", {
      clipboard: {
        write: async (items: FakeItem[]) => {
          written.push(await (await items[0].items["text/plain"]).text());
        },
        writeText: vi.fn(),
      },
    });
    const err = await copyWhenReady(Promise.reject(BACKEND_ERR)).catch((e: unknown) => e);
    expect(isNoRelayError(err)).toBe(true);
    expect(written).toEqual([]);
  });
});
