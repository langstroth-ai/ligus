import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { AttachmentsSection } from "./AttachmentsSection";
import * as ipc from "@/lib/ipc";

const off = { enabled: false, minRole: "contributor" as const, agents: false, services: false, maxBytes: 25 * 1024 * 1024 };
const recommended = { ...off, enabled: true, blockedTypes: ["@executable", "@installer", "@diskimage", "@script"] };

vi.mock("@/lib/ipc", () => ({
  attachmentRules: vi.fn(),
  setAttachmentPolicy: vi.fn(async () => undefined),
  blockAttachmentHash: vi.fn(async () => undefined),
  unblockAttachmentHash: vi.fn(async () => undefined),
}));
vi.mock("@/components/Toast", () => ({ toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() }, errMsg: (e: unknown) => String(e) }));

function renderIt() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <AttachmentsSection />
    </QueryClientProvider>,
  );
}

describe("AttachmentsSection", () => {
  beforeEach(() => vi.clearAllMocks());

  // Final review: a failed load showed "Loading…" forever.
  it("says when the rules can't be loaded", async () => {
    vi.mocked(ipc.attachmentRules).mockRejectedValue(new Error("the service is busy"));
    renderIt();
    expect(await screen.findByRole("alert")).toHaveTextContent("the service is busy");
    expect(screen.queryByText("Loading…")).toBeNull();
  });

  it("is off by default and only an Admin can change it", async () => {
    vi.mocked(ipc.attachmentRules).mockResolvedValue({ policy: off, recommended, blocked: [], canEdit: false, canShare: false, relay: true, localOnly: false, hardMaxBytes: 100 * 1024 * 1024 });
    renderIt();
    const box = await screen.findByLabelText("Let members share files");
    expect(box).not.toBeChecked();
    expect(box).toBeDisabled();
    expect(screen.getByLabelText(/Let agents offer files/)).not.toBeChecked();
    expect(screen.queryByRole("button", { name: "Save policy" })).toBeNull();
  });

  it("an Admin turns on the recommended policy and blocks a hash", async () => {
    vi.mocked(ipc.attachmentRules).mockResolvedValue({ policy: off, recommended, blocked: [], canEdit: true, canShare: false, relay: true, localOnly: false, hardMaxBytes: 100 * 1024 * 1024 });
    renderIt();
    await userEvent.click(await screen.findByRole("button", { name: "Use the recommended policy" }));
    await userEvent.click(screen.getByRole("button", { name: "Save policy" }));
    await waitFor(() => expect(ipc.setAttachmentPolicy).toHaveBeenCalledWith(recommended));
    const block = screen.getByRole("button", { name: "Block" });
    expect(block).toBeDisabled();
    await userEvent.type(screen.getByLabelText("SHA-256 to block"), "ab".repeat(32));
    await userEvent.click(block);
    await waitFor(() => expect(ipc.blockAttachmentHash).toHaveBeenCalledWith("ab".repeat(32)));
  });
});
