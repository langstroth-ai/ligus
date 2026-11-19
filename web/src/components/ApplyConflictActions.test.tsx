import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ApplyConflictActions, applyConflictKind } from "./RightRail";

describe("apply conflicts", () => {
  it("tells a conflict and local changes from other failures", () => {
    expect(applyConflictKind("Not applied: the change doesn't apply on this chat's branch (error: README.md: patch does not apply). Nothing changed")).toBe("conflict");
    expect(applyConflictKind("Not applied: the chat's checkout has local changes in the way (x). Nothing changed")).toBe("dirty");
    expect(applyConflictKind("Not applied: this chat is locked down.")).toBeNull();
    expect(applyConflictKind("not approved yet")).toBeNull();
  });

  it("offers rebase and open in editor for a conflict", async () => {
    const onRebase = vi.fn();
    const onOpen = vi.fn();
    render(
      <ApplyConflictActions
        conflict={{ kind: "conflict", message: "Not applied: the change doesn't apply on this chat's branch" }}
        busy={false}
        rebasing={false}
        onRebase={onRebase}
        onOpen={onOpen}
      />,
    );
    await userEvent.click(screen.getByText("Ask the agent to rebase"));
    await userEvent.click(screen.getByText("Open in editor"));
    expect(onRebase).toHaveBeenCalledOnce();
    expect(onOpen).toHaveBeenCalledOnce();
  });

  it("offers only open in editor for local changes", () => {
    render(<ApplyConflictActions conflict={{ kind: "dirty", message: "Not applied" }} busy={false} rebasing={false} onRebase={() => {}} onOpen={() => {}} />);
    expect(screen.queryByText("Ask the agent to rebase")).toBeNull();
    expect(screen.getByText("Open in editor")).toBeTruthy();
  });
});
