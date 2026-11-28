import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { WorkspaceHostDto } from "@/bindings/WorkspaceHostDto";

vi.mock("@/lib/ipc", () => ({ openExternal: vi.fn(async () => {}) }));

import { HostRow, HOST_UPGRADE_HOW } from "./RightRail";

const host = (over: Partial<WorkspaceHostDto> = {}): WorkspaceHostDto => ({
  id: "h1",
  kind: "worker",
  label: "box-1",
  online: true,
  lastSeen: "",
  modelGateways: [],
  ownerName: "",
  mine: true,
  offers: [],
  readiness: "",
  build: "0.4.1",
  upgradeTo: "0.5.0",
  ...over,
});

const row = (h: WorkspaceHostDto) =>
  render(
    <ul>
      <HostRow host={h} />
    </ul>,
  );

describe("Update available on a box row", () => {
  it("shows one grey line with the box's build, the release and how", async () => {
    row(host());
    const line = screen.getByTestId("host-update");
    expect(line).toHaveTextContent("0.4.1 → 0.5.0 available · how");
    expect(screen.queryByText(HOST_UPGRADE_HOW)).toBeNull();
    const how = screen.getByRole("button", { name: "how" });
    expect(how).toHaveAttribute("data-tip", HOST_UPGRADE_HOW);
    await userEvent.click(how);
    expect(screen.getByText(HOST_UPGRADE_HOW)).toBeInTheDocument();
    expect(HOST_UPGRADE_HOW).toMatch(/ligus-local upgrade/);
    expect(HOST_UPGRADE_HOW).toMatch(/brew upgrade ligus/);
    expect(HOST_UPGRADE_HOW).toMatch(/never upgrades a box on its own/);
  });

  it("says nothing when the box is current, unreported, or a desktop", () => {
    for (const h of [host({ upgradeTo: "" }), host({ build: "" }), host({ kind: "device" })]) {
      const { unmount } = row(h);
      expect(screen.queryByTestId("host-update")).toBeNull();
      unmount();
    }
  });
});
