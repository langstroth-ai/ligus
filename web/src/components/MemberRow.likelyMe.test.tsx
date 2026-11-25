import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

vi.mock("@/lib/ipc", () => ({}));

import { MemberRow, type RosterMember } from "./RightRail";
import { LIKELY_ME_HINT } from "@/lib/likelyMe";

const zaz: RosterMember = {
  id: "m-zaz",
  actorId: "a-zaz",
  displayName: "zaz",
  role: "owner",
  title: "",
  index: 1,
  isSelf: false,
  avatarUrl: null,
  kind: "human",
  isFounder: false,
  removeRefusal: null,
  roleOptions: [],
  regrantRefusal: null,
};

function row(onLinkComputer?: () => void) {
  const noop = () => {};
  return render(
    <ul>
      <MemberRow
        member={zaz}
        online={false}
        presenceKnown
        dup={false}
        onRole={noop}
        onRemove={noop}
        onRegrant={noop}
        onRevoke={noop}
        onLinkComputer={onLinkComputer}
      />
    </ul>,
  );
}

describe("MemberRow: you on another computer", () => {
  it("shows a muted hint, a tooltip and a first row-menu entry that only opens Add a computer", async () => {
    const link = vi.fn();
    row(link);
    expect(screen.getByTestId("likely-me").textContent).toBe(" · you on another computer?");
    expect(screen.getByTestId("member-m-zaz").querySelector("[data-tip]")?.getAttribute("data-tip")).toBe(LIKELY_ME_HINT);
    await userEvent.click(screen.getByRole("button", { name: "Actions for zaz" }));
    const items = screen.getAllByRole("menuitem");
    expect(items[0].textContent).toBe(`${LIKELY_ME_HINT}…`);
    await userEvent.click(items[0]);
    expect(link).toHaveBeenCalledTimes(1);
  });

  it("says nothing for anyone else", async () => {
    row(undefined);
    expect(screen.queryByTestId("likely-me")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Actions for zaz" }));
    expect(screen.queryByRole("menuitem", { name: new RegExp(LIKELY_ME_HINT.slice(0, 20)) })).toBeNull();
  });
});
