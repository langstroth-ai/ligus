import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, within, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import * as ipc from "@/lib/ipc";
import { requestDevices } from "@/lib/nav";
import { DevicesSection } from "./AccountSection";

vi.mock("@/lib/ipc", () => ({
  listMyDevices: vi.fn(),
  listDeviceRequests: vi.fn(async () => []),
  listDeviceRevocations: vi.fn(),
  listDevicePairings: vi.fn(async () => []),
  revokeMyDevice: vi.fn(),
  retryDeviceRevocation: vi.fn(async () => {}),
  inspectComputerRequest: vi.fn(),
  addComputer: vi.fn(),
  removeDuplicateMember: vi.fn(),
}));
vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));
vi.mock("@/components/settings/AddPhonePanel", () => ({ AddPhonePanel: () => <div>add phone panel</div> }));

const dev = (deviceId: string, label: string, revoked: boolean, thisDevice = false): ipc.MyDeviceDto => ({
  deviceId,
  label,
  fingerprint: `FP-${deviceId}`,
  thisDevice,
  revoked,
  lastSeen: null,
  phone: revoked,
  named: true,
});
const step = (state: ipc.RevocationStep["state"]) => ({ state, at: 1, attempts: 1 });
const report = (deviceId: string, complete: boolean): ipc.DeviceRevocationDto => ({
  hostId: `h-${deviceId}`,
  hostLabel: "home-server",
  reportedAt: 1_700_000_100,
  summary: complete ? "revoked: tunnel cut, push removed" : "revoked: tunnel not cut yet",
  complete,
  deviceId,
  name: "",
  revokedAt: 1_700_000_000,
  tunnel: step(complete ? "done" : "failed"),
  push: step("done"),
  session: step("not_applicable"),
});

function renderSection() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <DevicesSection />
    </QueryClientProvider>,
  );
}

describe("DevicesSection", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    vi.mocked(ipc.listMyDevices).mockResolvedValue([
      dev("d1", "MacBook", false, true),
      dev("d2", "lt on Michaels-Air-3", false),
      dev("p1", "Old iPhone", true),
      dev("p2", "Lost iPhone", true),
    ]);
    vi.mocked(ipc.listDeviceRevocations).mockResolvedValue([report("p1", true), report("p2", false)]);
  });

  it("keeps active devices as one-line rows and folds finished revocations away", async () => {
    renderSection();
    expect(await screen.findByText("lt on Michaels-Air-3")).toBeInTheDocument();
    const toggle = screen.getByRole("button", { name: /Revoked \(1\)/ });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("Old iPhone")).toBeNull();
    // No cleanup report on screen until asked for.
    expect(screen.queryByText(/Tunnel:/)).toBeNull();
  });

  it("keeps an unfinished cleanup in view with one warning line and its retry", async () => {
    renderSection();
    const row = await screen.findByTestId("device-p2");
    expect(row.textContent).toContain("revoked · tunnel not cut on home-server");
    await userEvent.click(within(row).getByRole("button", { name: "Retry now" }));
    expect(ipc.retryDeviceRevocation).toHaveBeenCalledWith("p2");
  });

  it("opens the group, remembers it, shows details and hides a row locally", async () => {
    renderSection();
    await userEvent.click(await screen.findByRole("button", { name: /Revoked \(1\)/ }));
    expect(localStorage.getItem("ligus.devices.revokedOpen")).toBe("1");
    const row = screen.getByTestId("device-p1");
    expect(row.textContent).toMatch(/Old iPhone.*revoked/);
    await userEvent.click(within(row).getByRole("button", { name: "Actions for Old iPhone" }));
    await userEvent.click(screen.getByRole("menuitem", { name: "Details" }));
    expect(screen.getByTestId("device-details-p1").textContent).toContain("Tunnel: done");
    await userEvent.click(within(row).getByRole("button", { name: "Actions for Old iPhone" }));
    const hide = screen.getByRole("menuitem", { name: "Remove from view" });
    expect(hide.closest("[data-tip]")?.getAttribute("data-tip") ?? hide.getAttribute("data-tip") ?? "").toContain(
      "it stays revoked",
    );
    await userEvent.click(hide);
    expect(screen.queryByTestId("device-p1")).toBeNull();
    expect(JSON.parse(localStorage.getItem("ligus.devices.hiddenRevoked")!)).toEqual(["p1"]);
    // Never a write: hiding is local.
    expect(ipc.revokeMyDevice).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Revoked devices actions" }));
    await userEvent.click(screen.getByRole("menuitem", { name: "Show hidden (1)" }));
    expect(screen.getByTestId("device-p1").textContent).toContain("hidden");
  });

  it("opens Add a computer when asked from elsewhere, with the member to remove", async () => {
    renderSection();
    await screen.findByText("lt on Michaels-Air-3");
    act(() => requestDevices({ open: "computer", member: { memberId: "m-zaz", displayName: "zaz" } }));
    expect((await screen.findByTestId("add-computer-suggested")).textContent).toContain("zaz");
  });
});
