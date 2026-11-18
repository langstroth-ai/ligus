import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { AdmitBoxModal } from "./AdmitBoxModal";
import * as ipc from "@/lib/ipc";

vi.mock("@/lib/ipc", () => ({
  inspectBoxRequest: vi.fn(async () => ({ fingerprint: "AB12-CD34-EF56", label: "build-1", accountId: "a1" })),
  admitBox: vi.fn(async (_ws: string, request: string | null) =>
    request ? { code: "ligusadmit1:sealed", sealed: true, expiresInMinutes: 60 } : { code: "hivews1:bearer", sealed: false, expiresInMinutes: 1440 },
  ),
}));

vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));

describe("AdmitBoxModal", () => {
  beforeEach(() => vi.clearAllMocks());

  it("admits only after the request's fingerprint was shown", async () => {
    render(<AdmitBoxModal workspaceId="w1" workspaceName="Acme" onClose={() => {}} />);
    const admit = screen.getByRole("button", { name: "Admit this box" });
    expect(admit).toBeDisabled();
    await userEvent.type(screen.getByLabelText(/Join request/), "ligusjoin1:xyz");
    await userEvent.click(screen.getByRole("button", { name: "Check" }));
    expect(await screen.findByText("AB12-CD34-EF56")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Admit this box" }));
    await waitFor(() => expect(ipc.admitBox).toHaveBeenCalledWith("w1", "ligusjoin1:xyz", null));
    expect((screen.getByLabelText("Admission code") as HTMLTextAreaElement).value).toBe("ligusadmit1:sealed");
    expect(screen.getByText(/only it can open this code/)).toBeInTheDocument();
  });

  it("makes a bearer code for Kubernetes with its lifetime", async () => {
    render(<AdmitBoxModal workspaceId="w1" workspaceName="Acme" onClose={() => {}} />);
    await userEvent.click(screen.getByRole("tab", { name: "For Kubernetes" }));
    await userEvent.click(screen.getByRole("button", { name: "Make an admission code" }));
    await waitFor(() => expect(ipc.admitBox).toHaveBeenCalledWith("w1", null, 24 * 60));
    expect((screen.getByLabelText("Admission code") as HTMLTextAreaElement).value).toBe("hivews1:bearer");
    expect(screen.getByText(/Kubernetes Secret/)).toBeInTheDocument();
  });
});
