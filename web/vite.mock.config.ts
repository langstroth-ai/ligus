// Dev-only browser preview with a fixture backend: `bun run dev:mock`.
// Aliases the Tauri IPC modules to dev/mock so the UI renders in a plain
// browser with canned data. The production build (vite.config.ts) never
// references dev/mock, so none of this ships.
import { defineConfig, mergeConfig } from "vite";
import { fileURLToPath, URL } from "node:url";
import base from "./vite.config";

export default mergeConfig(
  base,
  defineConfig({
    resolve: {
      alias: [
        { find: "@tauri-apps/api/core", replacement: fileURLToPath(new URL("./dev/mock/core.ts", import.meta.url)) },
        { find: "@tauri-apps/api/event", replacement: fileURLToPath(new URL("./dev/mock/event.ts", import.meta.url)) },
      ],
    },
    server: { port: 5199, strictPort: true },
  }),
);
