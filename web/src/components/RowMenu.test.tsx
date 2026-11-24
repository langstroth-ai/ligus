import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ListRow, RowList } from "./ui";
import { rowMenuItems } from "./RowMenu";

function row() {
  const edit = vi.fn();
  const remove = vi.fn();
  const select = vi.fn();
  render(
    <RowList label="Things">
      <ListRow
        title="fs"
        meta="stdio"
        onClick={select}
        actions={[
          { label: "Remove…", danger: true, onSelect: remove },
          { label: "Edit", onSelect: edit },
        ]}
      />
    </RowList>,
  );
  return { edit, remove, select };
}

describe("row actions menu", () => {
  it("keeps the row's primary action as the row click", async () => {
    const { select } = row();
    await userEvent.click(screen.getByRole("button", { name: /^fs/ }));
    expect(select).toHaveBeenCalled();
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("opens from the ⋯ button, safe items first and destructive last", async () => {
    const { edit } = row();
    const more = screen.getByRole("button", { name: "Actions for fs" });
    expect(more).toHaveAttribute("aria-haspopup", "menu");
    expect(more).toHaveAttribute("aria-expanded", "false");
    await userEvent.click(more);
    const menu = screen.getByRole("menu");
    expect(more).toHaveAttribute("aria-expanded", "true");
    expect(within(menu).getAllByRole("menuitem").map((m) => m.textContent)).toEqual(["Edit", "Remove…"]);
    expect(within(menu).getByRole("separator")).toBeInTheDocument();
    await userEvent.click(within(menu).getByRole("menuitem", { name: "Edit" }));
    expect(edit).toHaveBeenCalled();
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("opens on right-click at the pointer", () => {
    row();
    fireEvent.contextMenu(screen.getByText("fs"), { clientX: 40, clientY: 50 });
    expect(screen.getByRole("menu")).toBeInTheDocument();
  });

  it("opens with Shift+F10 or the Menu key on the focused row, Escape closes and focus returns", async () => {
    row();
    const primary = screen.getByRole("button", { name: /^fs/ });
    primary.focus();
    fireEvent.keyDown(primary, { key: "F10", shiftKey: true });
    expect(screen.getByRole("menu")).toBeInTheDocument();
    await userEvent.keyboard("{Escape}");
    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).toBe(primary);
    fireEvent.keyDown(primary, { key: "ContextMenu" });
    expect(screen.getByRole("menu")).toBeInTheDocument();
  });

  it("shows no ⋯ and no menu for a row without actions", () => {
    render(
      <RowList>
        <ListRow title="plain" />
      </RowList>,
    );
    expect(screen.queryByRole("button", { name: /Actions for/ })).toBeNull();
    fireEvent.contextMenu(screen.getByText("plain"));
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("orders menu items without a separator when all are safe", () => {
    expect(rowMenuItems([{ label: "A", onSelect: () => {} }]).map((i) => ("label" in i ? i.label : "-"))).toEqual(["A"]);
  });
});
