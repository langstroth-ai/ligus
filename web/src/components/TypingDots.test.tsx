import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { PeopleTyping, typingLabel, typingNamesFor } from "./TypingDots";
import type { PresenceDto } from "@/lib/ipc";

const p = (actorId: string, name: string, typing: boolean, sessionId = "s1"): PresenceDto => ({ actorId, name, typing, sessionId });

describe("people typing", () => {
  it("names only the people typing in this chat, once each", () => {
    const list = [p("a", "Ada", true), p("a", "Ada", true), p("b", "Bea", true, "other"), p("c", "Cy", false), p("d", "  ", true)];
    expect(typingNamesFor(list, "s1")).toEqual(["Ada", "Someone"]);
    expect(typingNamesFor(undefined, "s1")).toEqual([]);
  });

  it("labels one, two and many", () => {
    expect(typingLabel(["Ada"])).toBe("Ada is typing");
    expect(typingLabel(["Ada", "Bea"])).toBe("Ada and Bea are typing");
    expect(typingLabel(["Ada", "Bea", "Cy"])).toBe("Several people are typing");
  });

  it("renders a polite status for someone typing on their phone", () => {
    render(<PeopleTyping presence={[p("owner", "Olive", true)]} sessionId="s1" />);
    const row = screen.getByTestId("people-typing");
    expect(row.getAttribute("role")).toBe("status");
    expect(row.textContent).toContain("Olive is typing");
  });

  it("renders nothing when nobody is typing here", () => {
    const { container } = render(<PeopleTyping presence={[p("x", "X", true, "elsewhere")]} sessionId="s1" />);
    expect(container.innerHTML).toBe("");
  });
});
