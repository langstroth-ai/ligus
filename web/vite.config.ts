/// <reference types="vitest/config" />
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath, URL } from "node:url";
import type { Plugin } from "vite";

// Monaco vendors its own DOMPurify (esm/vs/base/browser/dompurify/dompurify.js)
// and its sanitizer imports it by relative path, so the package.json
// "overrides" entry for dompurify never reaches the copy Monaco actually uses.
// Point every import of that vendored file at the npm package instead, so the
// pinned, patched DOMPurify is what ships. src/lib/dompurify-pin.test.ts
// checks the version; the production build is checked by grepping dist.
const VENDORED_DOMPURIFY = /(^|[\\/])base[\\/]browser[\\/]dompurify[\\/]dompurify\.js$/;
function monacoUsesPinnedDompurify(): Plugin {
  return {
    name: "ligus:monaco-uses-pinned-dompurify",
    enforce: "pre",
    async resolveId(source, importer) {
      const fromMonaco = importer !== undefined && /[\\/]monaco-editor[\\/]/.test(importer);
      const direct = source.startsWith("monaco-editor/");
      if ((fromMonaco && source === "./dompurify/dompurify.js") || (direct && VENDORED_DOMPURIFY.test(source))) {
        return this.resolve("dompurify", importer, { skipSelf: true });
      }
      return null;
    },
  };
}

// Tauri expects a fixed dev port and ignores Vite's HMR over the custom
// protocol; see https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [monacoUsesPinnedDompurify(), react(), tailwindcss()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
    // Let a test read the app stylesheet with `?raw` (vitest blanks CSS otherwise).
    css: { include: [/styles\.css/] },
  },
});
