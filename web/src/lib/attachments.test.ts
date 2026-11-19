import { describe, it, expect } from "vitest";
import { attachmentMarker, splitAttachments, fileBaseName, isImagePath, previewSrcDoc } from "./attachments";

describe("attachments", () => {
  it("round-trips markers", () => {
    const body = `Look at this ${attachmentMarker("/tmp/a.png")}`;
    const { text, paths } = splitAttachments(body);
    expect(paths).toEqual(["/tmp/a.png"]);
    expect(text).toBe("Look at this");
  });

  it("extracts multiple paths and collapses blank lines", () => {
    const body = `Review\n\n${attachmentMarker("/x/a.png")}\n${attachmentMarker("/x/b.txt")}`;
    const { text, paths } = splitAttachments(body);
    expect(paths).toEqual(["/x/a.png", "/x/b.txt"]);
    expect(text).toBe("Review");
  });

  it("returns body unchanged when no markers", () => {
    expect(splitAttachments("plain text")).toEqual({ text: "plain text", paths: [], attachmentIds: [] });
  });

  it("derives base name across separators", () => {
    expect(fileBaseName("/a/b/c.png")).toBe("c.png");
    expect(fileBaseName("C:\\docs\\d.txt")).toBe("d.txt");
    expect(fileBaseName("solo.md")).toBe("solo.md");
  });

  it("detects image extensions", () => {
    expect(isImagePath("/x/a.PNG")).toBe(true);
    expect(isImagePath("/x/a.jpeg")).toBe(true);
    expect(isImagePath("/x/a.txt")).toBe(false);
  });

  it("splits shared-file markers by id, once each", () => {
    const id = "6f1c2b8e-3d4a-4e5f-9a0b-1c2d3e4f5a6b";
    const { text, attachmentIds } = splitAttachments(`see [Attachment: ${id}] and [Attachment: ${id.toUpperCase()}] [Attachment: ../../etc]`);
    expect(attachmentIds).toEqual([id]);
    expect(text).toBe("see  and  [Attachment: ../../etc]");
  });

  it("builds inert previews: escaped text, data-only images, no network", () => {
    const evil = btoa('<script>alert(1)</script><img src="https://evil.example/x">');
    const doc = previewSrcDoc("text", "text/plain", evil);
    expect(doc).not.toContain("<script>alert");
    expect(doc).toContain("&lt;script&gt;");
    expect(doc).toContain("default-src 'none'");
    const img = previewSrcDoc("image", "image/svg+xml", "PHN2Zz4=");
    expect(img).not.toContain("image/svg+xml");
    expect(previewSrcDoc("image", "image/png", 'AAAA"onerror="x')).not.toContain('"onerror');
  });
});
