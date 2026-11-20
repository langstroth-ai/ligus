import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { PopoverItem } from "./ui";

describe("a disabled popover row (an @mention you can't use)", () => {
  it("is greyed out with its reason and doesn't select", () => {
    const onSelect = vi.fn();
    render(<PopoverItem label="@private" kind="Not available" disabled title="Only its owner can use this agent" onSelect={onSelect} />);
    const row = screen.getByRole("menuitem");
    expect(row).toHaveAttribute("aria-disabled", "true");
    expect(row).toHaveAttribute("title", "Only its owner can use this agent");
    fireEvent.mouseDown(row);
    expect(onSelect).not.toHaveBeenCalled();
  });
  it("an enabled row selects", () => {
    const onSelect = vi.fn();
    render(<PopoverItem label="@scout" onSelect={onSelect} />);
    fireEvent.mouseDown(screen.getByRole("menuitem"));
    expect(onSelect).toHaveBeenCalledOnce();
  });
});
