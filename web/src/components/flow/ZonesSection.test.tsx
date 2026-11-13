import { describe, it, expect } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { ZonesView, zoneSummary } from "./ZonesSection";

describe("path zones", () => {
  it("summarises bars", () => {
    expect(zoneSummary({ id: "z", pattern: "src/", kind: "core", note: "", atMs: 0 })).toBe("core · 2 approval(s) from admin+");
    expect(zoneSummary({ id: "z", pattern: "**/*.md", kind: "lowBar", requiredApprovals: 0, note: "", atMs: 0 })).toBe("low bar · 1 approval(s)");
  });

  it("adds and removes zones", () => {
    const added: unknown[] = [];
    const removed: string[] = [];
    render(
      <ZonesView
        zones={[{ id: "z1", pattern: "crates/core/", kind: "core", note: "crypto", atMs: 0 }]}
        onAdd={(z) => added.push(z)}
        onRemove={(id) => removed.push(id)}
      />,
    );
    fireEvent.click(screen.getByText("Remove"));
    expect(removed).toEqual(["z1"]);
    fireEvent.change(screen.getByLabelText("Pattern"), { target: { value: "**/*.md" } });
    fireEvent.change(screen.getByLabelText("Zone kind"), { target: { value: "lowBar" } });
    fireEvent.change(screen.getByLabelText("Approvals"), { target: { value: "1" } });
    fireEvent.click(screen.getByText("Add zone"));
    expect(added).toEqual([{ pattern: "**/*.md", kind: "lowBar", requiredApprovals: 1, note: "" }]);
  });
});
