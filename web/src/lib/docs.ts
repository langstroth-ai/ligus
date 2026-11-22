// Links into the public docs site, so panes can point at their help page
// instead of carrying paragraphs of explanation on screen.

export const DOCS_BASE = "https://docs.langstroth.ai/";

/// Absolute docs URL for a site path such as "ligus/features/workflows/".
export function docsUrl(path: string): string {
  return DOCS_BASE + path.replace(/^\/+/, "");
}

/// The help page behind each rail pane's "?" link.
export const PANE_DOCS = {
  tools: "ligus/features/right-rail/",
  review: "ligus/features/right-rail/",
  people: "ligus/features/right-rail/",
  vaults: "ligus/concepts/vaults/",
  skills: "ligus/addons/skills/",
  workflows: "ligus/features/workflows/",
  flow: "ligus/features/flow/",
  memory: "ligus/features/memory/",
  context: "ligus/features/right-rail/",
  activity: "ligus/features/right-rail/",
} as const;
