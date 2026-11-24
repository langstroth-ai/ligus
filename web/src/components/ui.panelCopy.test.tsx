import { afterEach, describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { InfoTip, MetaTag, OnceHint, Section } from "./ui";

// The "Copy in panels" primitives (web/README.md): explanations leave the
// screen but stay reachable by keyboard and screen reader.
describe("panel copy primitives", () => {
  afterEach(() => localStorage.clear());

  it("InfoTip keeps its text in the accessibility tree, not on screen", () => {
    render(<InfoTip label="Tools" text="Only enabled servers give agents tools." />);
    const btn = screen.getByRole("button", { name: "About Tools" });
    expect(btn).toHaveAccessibleDescription("Only enabled servers give agents tools.");
    expect(btn).toHaveAttribute("data-tip", "Only enabled servers give agents tools.");
    expect(screen.getByText("Only enabled servers give agents tools.")).toHaveClass("sr-only");
  });

  it("Section puts its explanation behind a ? and its tag beside the title", () => {
    render(
      <Section title="Agents" info="The roster belongs to the workspace." tag={<MetaTag>from workspace</MetaTag>}>
        <div>rows</div>
      </Section>,
    );
    expect(screen.getByRole("heading", { name: "Agents" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "About Agents" })).toHaveAccessibleDescription(
      "The roster belongs to the workspace.",
    );
    expect(screen.getByText("from workspace")).toBeInTheDocument();
  });

  it("OnceHint hides for good once dismissed", async () => {
    const { unmount } = render(<OnceHint id="t1">Drag a card to hand it off.</OnceHint>);
    expect(screen.getByText("Drag a card to hand it off.")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Dismiss hint" }));
    expect(screen.queryByText("Drag a card to hand it off.")).toBeNull();
    unmount();
    render(<OnceHint id="t1">Drag a card to hand it off.</OnceHint>);
    expect(screen.queryByText("Drag a card to hand it off.")).toBeNull();
  });
});
