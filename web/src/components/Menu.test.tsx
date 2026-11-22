import { fireEvent, render, screen } from "@testing-library/react";
import { useRef, useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { useContextMenu } from "./ContextMenu";
import { Menu, type MenuItemSpec } from "./Menu";

function Harness({ items }: { items: MenuItemSpec[] }) {
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  return (
    <>
      <button ref={ref} aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen(true)}>
        Actions
      </button>
      <Menu open={open} anchorRef={ref} onClose={() => setOpen(false)} items={items} label="Workspace actions" />
    </>
  );
}

const keyOn = (key: string) => fireEvent.keyDown(document.activeElement!, { key });

describe("Menu", () => {
  it("focuses the first item, moves with the arrows and Home/End, and picks with Enter", () => {
    const rename = vi.fn();
    const copy = vi.fn();
    render(
      <Harness
        items={[
          { type: "header", label: "Apiary" },
          { label: "Rename", onClick: rename },
          { label: "Archive", onClick: () => {}, disabled: true },
          { label: "Copy invite", onClick: copy },
        ]}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Actions" }));
    expect(screen.getByRole("menu", { name: "Workspace actions" })).toBeInTheDocument();
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "Rename" }));
    keyOn("ArrowDown"); // skips the disabled item
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "Copy invite" }));
    keyOn("ArrowDown"); // wraps
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "Rename" }));
    keyOn("End");
    keyOn("Enter");
    expect(copy).toHaveBeenCalledOnce();
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });

  it("closes on Esc and gives focus back to the trigger", () => {
    render(<Harness items={[{ label: "Rename", onClick: () => {} }]} />);
    const trigger = screen.getByRole("button", { name: "Actions" });
    trigger.focus();
    fireEvent.click(trigger);
    keyOn("Escape");
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(document.activeElement).toBe(trigger);
  });

  it("jumps to an item by its first letter", () => {
    render(
      <Harness
        items={[
          { label: "Rename", onClick: () => {} },
          { label: "Copy invite", onClick: () => {} },
          { label: "Leave", onClick: () => {} },
        ]}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Actions" }));
    keyOn("l");
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "Leave" }));
  });

  it("asks for a second press on a confirming danger item", () => {
    const leave = vi.fn();
    render(<Harness items={[{ label: "Leave workspace", danger: true, confirm: "Press again to leave", onClick: leave }]} />);
    fireEvent.click(screen.getByRole("button", { name: "Actions" }));
    const item = screen.getByRole("menuitem", { name: "Leave workspace" });
    fireEvent.click(item);
    expect(leave).not.toHaveBeenCalled();
    expect(item).toHaveTextContent("Press again to leave");
    fireEvent.click(item);
    expect(leave).toHaveBeenCalledOnce();
  });

  it("closes on a click outside", () => {
    render(<Harness items={[{ label: "Rename", onClick: () => {} }]} />);
    fireEvent.click(screen.getByRole("button", { name: "Actions" }));
    const backdrop = document.querySelector('[aria-hidden="true"].fixed') as HTMLElement;
    fireEvent.mouseDown(backdrop);
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });
});

describe("useContextMenu", () => {
  function Target({ onRename }: { onRename: () => void }) {
    const { open, menu } = useContextMenu();
    return (
      <>
        <div onContextMenu={(e) => open(e, [{ label: "Rename", onClick: onRename }], "Chat actions")}>chat</div>
        {menu}
      </>
    );
  }

  it("opens the shared menu at the pointer", () => {
    const onRename = vi.fn();
    render(<Target onRename={onRename} />);
    fireEvent.contextMenu(screen.getByText("chat"), { clientX: 40, clientY: 60 });
    expect(screen.getByRole("menu", { name: "Chat actions" })).toBeInTheDocument();
    keyOn("Enter");
    expect(onRename).toHaveBeenCalled();
  });
});
