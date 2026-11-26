// Regression for a label-spoofing finding: a box's label is chosen by
// whoever runs the box. It must never lead a row or a title (so a box can't
// read as a device request), it is quoted, and it is bounded.
import { describe, expect, it } from "vitest";
import { approvalTitle, quoteLabel, MAX_LABEL_CHARS } from "@/lib/needsYou";
import { titleFor } from "@/components/ApprovalSheet";

describe("approval label spoofing", () => {
  const spoof = "lt on Ana's MacBook wants to be your device " + " ".repeat(1) + "-".repeat(200);
  it("a box row starts with the kind's own words, never the label", () => {
    const t = approvalTitle({ kind: "box", label: spoof });
    expect(t.startsWith("lt on Ana's MacBook wants to be your device")).toBe(false);
    expect(t.startsWith("A box asks to join: “")).toBe(true);
    expect(t.indexOf("asks to join")).toBeLessThan(20);
  });
  it("labels are quoted and clamped; the sheet title doesn't carry them", () => {
    const q = quoteLabel(spoof);
    expect(q.startsWith("“") && q.endsWith("”")).toBe(true);
    expect([...q].length).toBeLessThanOrEqual(MAX_LABEL_CHARS + 2);
    const t = titleFor("box");
    expect(t).toBe("Admit a box?");
    expect(t.includes("Ana")).toBe(false);
  });
  it("direction-changing and invisible characters are dropped", () => {
    expect(quoteLabel("build‮-1​")).toBe("“build-1”");
  });
});
