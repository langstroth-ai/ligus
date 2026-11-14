// Self-host Monaco instead of @monaco-editor/react's default CDN loader — a
// Tauri webview has no guaranteed network at runtime. Import this once before
// rendering. Vite bundles each worker via the `?worker` suffix.
//
// Wiring the *language* workers (not just the base editor worker) is what turns
// on Monaco's built-in language service — hover, go-to-definition, completion,
// signature help and diagnostics for TS/JS/JSON/CSS/HTML — with NO language
// server installed. Our LSP adapter is a separate, project-aware layer on top.
import * as monaco from "monaco-editor";
import editorWorker from "monaco-editor/editor/editor.worker?worker";
import jsonWorker from "monaco-editor/languages/features/json/json.worker?worker";
import cssWorker from "monaco-editor/languages/features/css/css.worker?worker";
import htmlWorker from "monaco-editor/languages/features/html/html.worker?worker";
import tsWorker from "monaco-editor/languages/features/typescript/ts.worker?worker";
import { loader } from "@monaco-editor/react";

self.MonacoEnvironment = {
  getWorker(_moduleId: string, label: string) {
    switch (label) {
      case "json":
        return new jsonWorker();
      case "css":
      case "scss":
      case "less":
        return new cssWorker();
      case "html":
      case "handlebars":
      case "razor":
        return new htmlWorker();
      case "typescript":
      case "javascript":
        return new tsWorker();
      default:
        return new editorWorker();
    }
  },
};

// The editor renders repo files without a tsconfig/node_modules in scope, so the
// TS worker's *semantic* pass would flag every unresolved import as an error.
// Keep syntax validation (real typos) but silence semantic noise; hover,
// go-to-definition and completion still work off open models + the default libs.
// Project-aware diagnostics come from the LSP path (typescript-language-server).
// Since monaco-editor 0.56 the TS language service is the top-level
// `monaco.typescript` namespace (it is no longer hung off `monaco.languages`).
const tsLangs = monaco.typescript;
tsLangs.typescriptDefaults.setEagerModelSync(true);
tsLangs.javascriptDefaults.setEagerModelSync(true);
tsLangs.typescriptDefaults.setDiagnosticsOptions({ noSemanticValidation: true, noSyntaxValidation: false });
tsLangs.javascriptDefaults.setDiagnosticsOptions({ noSemanticValidation: true, noSyntaxValidation: false });

loader.config({ monaco });
