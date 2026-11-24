import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { Avatar } from "./Avatar";

describe("Avatar", () => {
  it("shows an uploaded data:image avatar", () => {
    const url = "data:image/png;base64,iVBORw0KGgo=";
    const { container } = render(<Avatar name="Ada" url={url} />);
    expect(container.querySelector("img")?.getAttribute("src")).toBe(url);
  });

  it("never loads a remote URL from a synced roster entry", () => {
    // The local setter only accepts data:image/ URLs, but the roster is synced:
    // another member's client can put https://host/pixel in its avatar, and
    // rendering it would leak every viewer's IP and when they read the chat
    // (the CSP allows https: images).
    for (const url of ["https://evil.example/p.png?who=me", "http://10.0.0.1/x", "//evil.example/x", "blob:https://x/y"]) {
      const { container, unmount } = render(<Avatar name="Mallory" url={url} />);
      expect(container.querySelector("img")).toBeNull();
      expect(container.textContent).toContain("M");
      unmount();
    }
  });

  it("uses a hex accent color but falls back for anything else", () => {
    const bg = (colorHex: string) => {
      const { container, unmount } = render(<Avatar name="Mallory" colorHex={colorHex} />);
      const v = (container.firstChild as HTMLElement).style.background;
      unmount();
      return v;
    };
    const fallback = bg("");
    expect(bg("#3f72a8")).not.toBe(fallback);
    // A synced roster color goes into CSS: a url() would load a remote image
    // and tell its host who is viewing.
    for (const c of ["url(https://evil.example/p.gif)", "#fff url(https://evil.example/p.gif)", "red", "#12"]) {
      expect(bg(c)).toBe(fallback);
      expect(bg(c)).not.toContain("url(");
    }
  });
});
