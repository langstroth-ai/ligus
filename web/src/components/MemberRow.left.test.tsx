import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

vi.mock("@/lib/ipc", () => ({}));

import { MemberRow, type RosterMember } from "./RightRail";

const ola: RosterMember = {
  id: "m-ola",
  actorId: "a-ola",
  displayName: "Ola",
  role: "contributor",
  title: "",
  index: 2,
  isSelf: false,
  avatarUrl: null,
  kind: "human",
  isFounder: false,
  removeRefusal: null,
  roleOptions: [],
  regrantRefusal: null,
  leftAt: "2026-11-24T10:00:00Z",
};

function row(member: RosterMember, onHideHere?: (hide: boolean) => void, hiddenHere = false) {
  const noop = () => {};
  return render(
    <ul>
      <MemberRow
        member={member}
        online={false}
        presenceKnown
        dup={false}
        onRole={noop}
        onRemove={noop}
        onRegrant={noop}
        onRevoke={noop}
        onHideHere={onHideHere}
        hiddenHere={hiddenHere}
      />
    </ul>,
  );
}

describe("MemberRow: a member who asked to leave", () => {
  it("is grey, says so, keeps Remove for Admins and offers Remove from view", async () => {
    const hide = vi.fn();
    row(ola, hide);
    expect(screen.getByTestId("member-left").textContent).toMatch(/asked to leave/);
    expect((screen.getByTestId("member-m-ola") as HTMLElement).style.opacity).toBe("0.6");
    await userEvent.click(screen.getByRole("button", { name: "Actions for Ola" }));
    expect(screen.getByRole("menuitem", { name: "Remove" })).toBeTruthy();
    await userEvent.click(screen.getByRole("menuitem", { name: "Remove from view" }));
    expect(hide).toHaveBeenCalledWith(true);
  });

  it("shown again after removing from view, it offers Show in the list", async () => {
    const hide = vi.fn();
    row(ola, hide, true);
    expect(screen.getByTestId("member-left").textContent).toMatch(/removed from view/);
    await userEvent.click(screen.getByRole("button", { name: "Actions for Ola" }));
    await userEvent.click(screen.getByRole("menuitem", { name: "Show in the list" }));
    expect(hide).toHaveBeenCalledWith(false);
  });

  it("a current member has neither", async () => {
    row({ ...ola, leftAt: null });
    expect(screen.queryByTestId("member-left")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Actions for Ola" }));
    expect(screen.queryByRole("menuitem", { name: "Remove from view" })).toBeNull();
  });
});
