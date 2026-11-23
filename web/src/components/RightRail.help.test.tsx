import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import * as ipc from "@/lib/ipc";

vi.mock("@/lib/ipc", () => ({
  openExternal: vi.fn(async () => {}),
}));

import { RailFrame, agentRemovalConsequences } from "./RightRail";
import { MoreMenu } from "./MoreMenu";

describe("RailFrame help", () => {
  beforeEach(() => vi.clearAllMocks());

  it("keeps the intro off screen and behind one ? link to the docs", async () => {
    render(
      <RailFrame title="Vaults" about="Reference material mounted into this workspace." docs="ligus/concepts/vaults/">
        <div>content</div>
      </RailFrame>,
    );
    expect(screen.queryByText("Reference material mounted into this workspace.")).toBeNull();
    const help = screen.getByRole("link", { name: /About Vaults: Reference material mounted into this workspace\./ });
    expect(help).toHaveTextContent("?");
    expect(help).toHaveAttribute("title", "Reference material mounted into this workspace. Opens the docs.");
    await userEvent.click(help);
    expect(ipc.openExternal).toHaveBeenCalledWith("https://docs.langstroth.ai/ligus/concepts/vaults/");
  });
});

describe("agent removal", () => {
  it("names what removing an agent costs", () => {
    const text = agentRemovalConsequences("builder");
    expect(text).toContain("@builder leaves every chat in this workspace, for everyone");
    expect(text).toContain("Mentions of @builder go unanswered");
    expect(text).toContain("workflow stages that use it stop");
    expect(text).toContain("adding it again starts from scratch");
  });
});

describe("MoreMenu", () => {
  it("keeps actions behind ⋯ and runs the chosen one", async () => {
    const remove = vi.fn();
    const clear = vi.fn();
    render(
      <MoreMenu
        label="More actions for @builder"
        items={[
          { label: "Clear avatar", onSelect: clear },
          { label: "Remove from workspace…", danger: true, onSelect: remove },
        ]}
      />,
    );
    expect(screen.queryByRole("menuitem")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "More actions for @builder" }));
    expect(screen.getAllByRole("menuitem").map((m) => m.textContent)).toEqual(["Clear avatar", "Remove from workspace…"]);
    await userEvent.click(screen.getByRole("menuitem", { name: "Remove from workspace…" }));
    expect(remove).toHaveBeenCalled();
    expect(clear).not.toHaveBeenCalled();
    expect(screen.queryByRole("menuitem")).toBeNull();
  });

  it("works from the keyboard", async () => {
    const run = vi.fn();
    render(<MoreMenu label="More" items={[{ label: "Delete workflow…", danger: true, onSelect: run }]} />);
    screen.getByRole("button", { name: "More" }).focus();
    await userEvent.keyboard("{Enter}");
    // The first item takes focus; Enter picks it.
    await userEvent.keyboard("{Enter}");
    expect(run).toHaveBeenCalled();
  });

  it("skips a disabled item", async () => {
    const run = vi.fn();
    render(<MoreMenu label="More" items={[{ label: "Nope", disabled: true, onSelect: run }]} />);
    await userEvent.click(screen.getByRole("button", { name: "More" }));
    await userEvent.click(screen.getByRole("menuitem", { name: "Nope" }));
    expect(run).not.toHaveBeenCalled();
  });
});
