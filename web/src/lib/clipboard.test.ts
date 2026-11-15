import { afterEach, describe, expect, it, vi } from "vitest";
import { copyWhenReady } from "./clipboard";

class FakeItem {
  constructor(public items: Record<string, Promise<Blob>>) {}
}

afterEach(() => {
  vi.unstubAllGlobals();
});

function stubClipboard(write: (items: FakeItem[]) => Promise<void>) {
  vi.stubGlobal("ClipboardItem", FakeItem);
  vi.stubGlobal("navigator", { clipboard: { write, writeText: vi.fn() } });
}

describe("copyWhenReady", () => {
  it("starts the clipboard write before the text arrives", async () => {
    let release!: (t: string) => void;
    const text = new Promise<string>((r) => (release = r));
    const write = vi.fn(async (items: FakeItem[]) => {
      await items[0].items["text/plain"];
    });
    stubClipboard(write);

    const done = copyWhenReady(text);
    // The write was issued synchronously, inside the click, with nothing awaited yet.
    expect(write).toHaveBeenCalledTimes(1);
    release("hivews1:abc");
    await expect(done).resolves.toBe("hivews1:abc");
    const blob = await write.mock.calls[0][0][0].items["text/plain"];
    expect(await blob.text()).toBe("hivews1:abc");
  });

  it("reports the producer's error, not the clipboard's", async () => {
    stubClipboard(async (items) => {
      await items[0].items["text/plain"];
    });
    const err = new Error("Passkey check was cancelled");
    await expect(copyWhenReady(Promise.reject(err))).rejects.toBe(err);
  });

  it("reports a clipboard refusal when the text was fine", async () => {
    const refused = new DOMException("denied", "NotAllowedError");
    stubClipboard(async () => {
      throw refused;
    });
    await expect(copyWhenReady(Promise.resolve("x"))).rejects.toBe(refused);
  });

  it("falls back to writeText without ClipboardItem", async () => {
    const writeText = vi.fn(async () => {});
    vi.stubGlobal("ClipboardItem", undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    await expect(copyWhenReady(Promise.resolve("y"))).resolves.toBe("y");
    expect(writeText).toHaveBeenCalledWith("y");
  });
});
