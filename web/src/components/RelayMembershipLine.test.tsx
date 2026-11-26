import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { RelayMembershipLine, relayMembershipCopy, LIGUS_ENFORCES } from "./RelayMembershipLine";
import type { RelayMembershipDto } from "@/lib/ipc";

const status = (over: Partial<RelayMembershipDto>): RelayMembershipDto => ({
  state: "unclaimed",
  members: [],
  identified: false,
  community: false,
  ...over,
});

function renderLine(s: RelayMembershipDto) {
  const onClaim = vi.fn();
  const onToken = vi.fn();
  render(<RelayMembershipLine status={s} claiming={false} onClaim={onClaim} onToken={onToken} />);
  return { onClaim, onToken };
}

describe("RelayMembershipLine", () => {
  it("can be hidden, and stays hidden for that workspace and state", async () => {
    localStorage.clear();
    const s = status({ identified: true });
    const { unmount } = render(<RelayMembershipLine status={s} claiming={false} onClaim={vi.fn()} onToken={vi.fn()} scope="ws-1" />);
    await userEvent.click(screen.getByRole("button", { name: "Hide this line" }));
    expect(screen.queryByText("Relay doesn't check membership")).toBeNull();
    unmount();
    // Remembered on this device.
    render(<RelayMembershipLine status={s} claiming={false} onClaim={vi.fn()} onToken={vi.fn()} scope="ws-1" />);
    expect(screen.queryByText("Relay doesn't check membership")).toBeNull();
  });

  it("a hidden line comes back when the relay starts refusing this device", () => {
    localStorage.clear();
    localStorage.setItem("ligus.relayMembershipLine.hidden.ws-1.unclaimed", "1");
    render(<RelayMembershipLine status={status({ state: "hidden" })} claiming={false} onClaim={vi.fn()} onToken={vi.fn()} scope="ws-1" />);
    expect(screen.getByText("Relay roster: you're not on it")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Hide this line" })).toBeNull();
  });

  it("offers Claim only when this device's token names an account", async () => {
    const { onClaim } = renderLine(status({ identified: true }));
    await userEvent.click(screen.getByRole("button", { name: "Claim ownership" }));
    expect(onClaim).toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: "Add a relay token" })).toBeNull();
  });

  it("offers a relay token, not Claim, on a self-hosted relay without one", async () => {
    const { onToken } = renderLine(status({}));
    expect(screen.queryByRole("button", { name: "Claim ownership" })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Add a relay token" }));
    expect(onToken).toHaveBeenCalled();
  });

  it("offers nothing on the community relay without an identified token", () => {
    renderLine(status({ community: true }));
    expect(screen.queryAllByRole("button").map((b) => b.textContent ?? "").join(" ")).not.toMatch(/Claim|token/);
    expect(relayMembershipCopy(status({ community: true }))!.detail).toMatch(/no sign-in: only its operator/);
  });

  it("still offers Claim on the community relay with an identified token", () => {
    renderLine(status({ community: true, identified: true }));
    expect(screen.getByRole("button", { name: "Claim ownership" })).toBeTruthy();
  });

  it("says membership is off, with no action, when the relay has none", () => {
    const c = relayMembershipCopy(status({ state: "off", identified: true }))!;
    expect(c.action).toBeNull();
    expect(c.detail).toMatch(/nothing to claim/);
  });

  it("shows nothing for a claimed workspace (the Members tag says it)", () => {
    expect(relayMembershipCopy(status({ state: "claimed" }))).toBeNull();
    const { container } = render(
      <RelayMembershipLine status={status({ state: "claimed" })} claiming={false} onClaim={() => {}} onToken={() => {}} />,
    );
    expect(container.textContent).toBe("");
  });

  it("is one calm, accurate line with the details behind the ?", () => {
    renderLine(status({}));
    expect(screen.getByText("Relay doesn't check membership")).toBeTruthy();
    expect(screen.queryByText(/anyone can write/)).toBeNull();
    for (const s of [status({}), status({ identified: true }), status({ community: true }), status({ state: "off" }), status({ state: "hidden" })]) {
      const c = relayMembershipCopy(s)!;
      expect(c.detail).toContain(LIGUS_ENFORCES);
    }
    expect(LIGUS_ENFORCES).toMatch(/can't read it, pose as a member or change anything/);
    expect(LIGUS_ENFORCES).toMatch(/junk, which every device drops/);
  });
});
