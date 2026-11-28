import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import * as ipc from "@/lib/ipc";
import * as dialog from "@/components/Dialog";
import { rowActionTarget } from "@/lib/setupChecklist";
import { SetupChecklistPane } from "./SetupChecklist";
// The shared fixture every surface renders (the backend's own test makes these
// rows from the same facts, and lt and the phone render them too).
import settled from "../../../../crates/ligus-runtime/tests/fixtures/setup-checklist/settled-team.checklist.json";
import fresh from "../../../../crates/ligus-runtime/tests/fixtures/setup-checklist/new-person.checklist.json";

vi.mock("@/lib/ipc", () => ({
  setupChecklist: vi.fn(),
  setupHide: vi.fn(async () => {}),
  setupRemoveHost: vi.fn(async () => {}),
  removeMember: vi.fn(async () => {}),
  removeAgent: vi.fn(async () => {}),
  revokeMyDevice: vi.fn(async () => {}),
}));
vi.mock("@/components/Dialog", () => ({ confirmDialog: vi.fn(async () => true) }));
vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));

function renderPane(list: ipc.SetupChecklist) {
  vi.mocked(ipc.setupChecklist).mockResolvedValue(list);
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <SetupChecklistPane initial={list} />
    </QueryClientProvider>,
  );
}

const rowsOf = () => screen.getAllByRole("listitem").filter((el) => el.dataset.testid?.startsWith("setup-row-"));

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(dialog.confirmDialog).mockResolvedValue(true);
});

describe("Set up Ligus", () => {
  it("shows the fixture's rows in order, each with its detail and at most one action", () => {
    const list = settled as ipc.SetupChecklist;
    renderPane(list);
    const rows = rowsOf();
    expect(rows.map((r) => r.dataset.testid)).toEqual(list.rows.map((r) => `setup-row-${r.id}`));
    for (const [i, r] of list.rows.entries()) {
      const el = rows[i];
      expect(el.dataset.state).toBe(r.state);
      expect(el.textContent).toContain(r.title);
      expect(el.textContent).toContain(r.detail);
      expect(within(el).queryAllByRole("button").length).toBe(r.action ? 1 : 0);
      if (r.action) expect(within(el).getByRole("button").textContent).toBe(r.action.label);
    }
  });

  it("reads grey Not set up for what was never chosen, never red", () => {
    const list = fresh as ipc.SetupChecklist;
    renderPane(list);
    for (const id of ["you", "passkey", "devices", "phone", "box", "agents"]) {
      const el = screen.getByTestId(`setup-row-${id}`);
      expect(el.dataset.state).toBe("notSetUp");
      expect(el.textContent).toMatch(/Not set up/);
      expect(el.innerHTML).not.toMatch(/danger/);
    }
  });

  it("opens Clean up, and removes from view only after the confirm", async () => {
    renderPane(settled as ipc.SetupChecklist);
    await userEvent.click(within(screen.getByTestId("setup-row-cleanup")).getByRole("button", { name: "Clean up" }));
    const list = screen.getByTestId("setup-cleanup");
    const revoked = within(list).getByTestId("cleanup-deviceRevoked:6f1c2a10-0000-4000-8000-000000000003");
    vi.mocked(dialog.confirmDialog).mockResolvedValueOnce(false);
    await userEvent.click(within(revoked).getByRole("button", { name: "Remove from view" }));
    expect(ipc.setupHide).not.toHaveBeenCalled();
    await userEvent.click(within(revoked).getByRole("button", { name: "Remove from view" }));
    expect(String(vi.mocked(dialog.confirmDialog).mock.calls.at(-1)?.[0])).toMatch(/on this device/);
    expect(ipc.setupHide).toHaveBeenCalledWith("deviceRevoked:6f1c2a10-0000-4000-8000-000000000003");
    expect(ipc.revokeMyDevice).not.toHaveBeenCalled();
  });

  it("each removal goes through its own command after its confirm", async () => {
    renderPane(settled as ipc.SetupChecklist);
    await userEvent.click(within(screen.getByTestId("setup-row-cleanup")).getByRole("button", { name: "Clean up" }));
    const list = screen.getByTestId("setup-cleanup");
    const click = async (key: string) => userEvent.click(within(within(list).getByTestId(`cleanup-${key}`)).getByRole("button", { name: "Remove" }));
    await click("memberLeft:member-taylor");
    expect(ipc.removeMember).toHaveBeenCalledWith(expect.any(String), "member-taylor");
    await click("devicePending:6f1c2a10-0000-4000-8000-000000000002");
    expect(ipc.revokeMyDevice).toHaveBeenCalledWith("6f1c2a10-0000-4000-8000-000000000002");
    await click("formerHost:6f1c2a10-0000-4000-8000-000000000006");
    expect(ipc.setupRemoveHost).toHaveBeenCalledWith("6f1c2a10-0000-4000-8000-000000000006");
    await click("legacyAgent:6f1c2a10-0000-4000-8000-000000000007");
    expect(ipc.removeAgent).toHaveBeenCalledWith(expect.any(String), "6f1c2a10-0000-4000-8000-000000000007");
    expect(vi.mocked(dialog.confirmDialog)).toHaveBeenCalledTimes(4);
  });

  it("a declined confirm removes nothing, also for Remove all", async () => {
    vi.mocked(dialog.confirmDialog).mockResolvedValue(false);
    renderPane(settled as ipc.SetupChecklist);
    await userEvent.click(within(screen.getByTestId("setup-row-cleanup")).getByRole("button", { name: "Clean up" }));
    await userEvent.click(screen.getByRole("button", { name: /Remove all/ }));
    for (const f of [ipc.removeMember, ipc.revokeMyDevice, ipc.setupRemoveHost, ipc.removeAgent, ipc.setupHide]) {
      expect(f).not.toHaveBeenCalled();
    }
  });

  it("shows the box's upgrade line in place", async () => {
    renderPane(settled as ipc.SetupChecklist);
    await userEvent.click(within(screen.getByTestId("setup-row-box")).getByRole("button", { name: "How to upgrade" }));
    expect(screen.getByTestId("setup-box-upgrade").textContent).toContain("ligus-local upgrade");
  });

  it("maps every action id a row can carry", () => {
    for (const id of ["profile", "passkey", "recovery", "deviceName", "addComputer", "devices", "addPhone", "addBox", "upgradeBox", "addAgent", "agents", "upgradeRules", "cleanup"]) {
      expect(rowActionTarget(id).kind, id).not.toBe("none");
    }
    expect(rowActionTarget("push").kind).toBe("none");
  });
});
