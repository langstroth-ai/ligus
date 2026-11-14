// Monaco ships its own copy of DOMPurify under esm/vs/base/browser/dompurify
// and its sanitizer (domSanitize.js, used by the markdown hover renderer and
// buttons) imports that copy by relative path. An npm "overrides" entry for
// dompurify therefore never reaches it: the override only swaps the
// node_modules/dompurify that nothing imports. vite.config.ts redirects the
// vendored file to the npm package so the patched version is what ships; CI
// also greps the production bundle for the version string.
import { describe, expect, it } from "vitest";
import npmPurify from "dompurify";
import pkg from "../../package.json";
// @ts-expect-error Monaco publishes no types for its vendored DOMPurify.
import monacoPurify from "monaco-editor/base/browser/dompurify/dompurify.js";

describe("Monaco's DOMPurify", () => {
  it("is the overridden, patched version, not Monaco's vendored copy", () => {
    expect(npmPurify.version).toBe(pkg.overrides.dompurify);
    expect((monacoPurify as { version: string }).version).toBe(pkg.overrides.dompurify);
  });
});
