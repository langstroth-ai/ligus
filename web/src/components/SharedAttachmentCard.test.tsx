import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { SharedAttachmentCard } from "./SharedAttachmentCard";
import * as ipc from "@/lib/ipc";
import * as dialog from "@/components/Dialog";
import type { AttachmentCheck, SharedAttachmentView } from "@/lib/ipc";

const check = (over: Partial<AttachmentCheck> = {}): AttachmentCheck => ({
  id: "a1",
  sha256: "ab".repeat(32),
  name: "photo.png",
  size: 10,
  sender: "Mara",
  assessment: {
    detected: { ext: "png", mime: "image/png", class: "image" },
    nameExt: "png",
    nameClass: "image",
    mismatch: false,
    doubleExtension: false,
    risky: false,
    warnings: [],
    preview: "image",
  },
  archive: null,
  scan: { verdict: "notConfigured" },
  blockedType: null,
  refused: null,
  needsConfirmation: false,
  ...over,
});

vi.mock("@/lib/ipc", () => ({
  checkAttachment: vi.fn(),
  saveReceivedAttachment: vi.fn(async () => "/tmp/photo.png"),
  previewAttachment: vi.fn(async () => ({ kind: "image", mime: "image/png", dataBase64: "iVBORw0KGgo=" })),
  deleteAttachment: vi.fn(async () => undefined),
}));

vi.mock("@/components/Dialog", () => ({ confirmDialog: vi.fn(async () => true) }));

vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));

const view = (over: Partial<SharedAttachmentView> = {}): SharedAttachmentView => ({
  id: "a1",
  name: "photo.png",
  size: 10,
  contentType: "image/png",
  sha256: "ab".repeat(32),
  author: "mara",
  sender: "Mara",
  authorKind: "human",
  deleted: false,
  blocked: false,
  mine: false,
  canDelete: false,
  nameClass: "image",
  riskyByName: false,
  fromAgentOffer: false,
  messageIds: ["m1"],
  ...over,
});

describe("SharedAttachmentCard", () => {
  beforeEach(() => vi.clearAllMocks());

  it("fetches, opens and previews nothing until asked", async () => {
    vi.mocked(ipc.checkAttachment).mockResolvedValue(check());
    render(<SharedAttachmentCard sessionId="s1" view={view()} />);
    expect(ipc.checkAttachment).not.toHaveBeenCalled();
    expect(ipc.previewAttachment).not.toHaveBeenCalled();
    expect(screen.queryByTestId("attachment-preview")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Check" }));
    expect(ipc.checkAttachment).toHaveBeenCalledWith("s1", "a1");
    expect(await screen.findByTestId("attachment-check")).toHaveTextContent("Verified against what Mara signed");
    expect(ipc.previewAttachment).not.toHaveBeenCalled();
  });

  it("previews only in an empty sandbox with a no-network policy", async () => {
    vi.mocked(ipc.checkAttachment).mockResolvedValue(check());
    render(<SharedAttachmentCard sessionId="s1" view={view()} />);
    await userEvent.click(screen.getByRole("button", { name: "Check" }));
    await userEvent.click(await screen.findByRole("button", { name: "Preview" }));
    const frame = await screen.findByTestId("attachment-preview");
    expect(frame.getAttribute("sandbox")).toBe("");
    const doc = frame.getAttribute("srcdoc") ?? "";
    expect(doc).toContain("default-src 'none'");
    expect(doc).not.toContain("<script");
  });

  it("offers no preview for a type outside the allow-list", async () => {
    vi.mocked(ipc.checkAttachment).mockResolvedValue(check({ assessment: { ...check().assessment, preview: null } }));
    render(<SharedAttachmentCard sessionId="s1" view={view({ name: "a.pdf" })} />);
    await userEvent.click(screen.getByRole("button", { name: "Check" }));
    await screen.findByRole("button", { name: "Save…" });
    expect(screen.queryByRole("button", { name: "Preview" })).toBeNull();
  });

  it("asks twice, naming the sender, before saving a risky file", async () => {
    vi.mocked(ipc.checkAttachment).mockResolvedValue(
      check({
        name: "invoice.pdf.exe",
        assessment: { ...check().assessment, risky: true, preview: null, warnings: ["This is a program (exe)."] },
        needsConfirmation: true,
      }),
    );
    render(<SharedAttachmentCard sessionId="s1" view={view({ name: "invoice.pdf.exe", riskyByName: true })} />);
    expect(screen.getByRole("alert")).toHaveTextContent(/can run code/);
    await userEvent.click(screen.getByRole("button", { name: "Check" }));
    await userEvent.click(await screen.findByRole("button", { name: "Save…" }));
    await waitFor(() => expect(ipc.saveReceivedAttachment).toHaveBeenCalledWith("s1", "a1", true));
    const calls = vi.mocked(dialog.confirmDialog).mock.calls;
    expect(calls.length).toBe(2);
    expect(calls[0][1]?.title).toContain("Mara");
    expect(calls[1][1]?.confirmLabel).toContain("Mara");
  });

  it("doesn't save when the second confirmation is declined", async () => {
    vi.mocked(ipc.checkAttachment).mockResolvedValue(check({ needsConfirmation: true, assessment: { ...check().assessment, risky: true } }));
    vi.mocked(dialog.confirmDialog).mockResolvedValueOnce(true).mockResolvedValueOnce(false);
    render(<SharedAttachmentCard sessionId="s1" view={view()} />);
    await userEvent.click(screen.getByRole("button", { name: "Check" }));
    await userEvent.click(await screen.findByRole("button", { name: "Save…" }));
    await waitFor(() => expect(dialog.confirmDialog).toHaveBeenCalledTimes(2));
    expect(ipc.saveReceivedAttachment).not.toHaveBeenCalled();
  });

  it("refuses what the check refused, and shows blocked files without a check", async () => {
    vi.mocked(ipc.checkAttachment).mockResolvedValue(check({ refused: "this archive looks like a decompression bomb (more than 10000 entries)" }));
    const { unmount } = render(<SharedAttachmentCard sessionId="s1" view={view({ name: "x.zip" })} />);
    await userEvent.click(screen.getByRole("button", { name: "Check" }));
    expect(await screen.findByText(/decompression bomb/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Save…" })).toBeNull();
    unmount();
    render(<SharedAttachmentCard sessionId="s1" view={view({ blocked: true })} />);
    expect(screen.getByText(/blocked in this workspace/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Check" })).toBeNull();
  });
  it("shows an expired file as a note, with nothing to download", () => {
    render(<SharedAttachmentCard sessionId="s1" view={view({ expired: true })} />);
    const note = screen.getByTestId("shared-attachment-expired");
    expect(note).toHaveTextContent("photo.png");
    expect(note).toHaveTextContent("This file expired on the relay");
    expect(screen.queryByRole("button", { name: "Check" })).toBeNull();
    expect(screen.queryByTestId("shared-attachment")).toBeNull();
  });

  it("turns a download the relay can't serve into the expired note", async () => {
    vi.mocked(ipc.checkAttachment).mockRejectedValue("This file expired on the relay");
    render(<SharedAttachmentCard sessionId="s1" view={view()} />);
    await userEvent.click(screen.getByRole("button", { name: "Check" }));
    expect(await screen.findByTestId("shared-attachment-expired")).toHaveTextContent("This file expired on the relay");
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
