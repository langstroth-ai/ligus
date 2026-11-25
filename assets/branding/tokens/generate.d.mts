// Types for generate.mjs, for the web tests.
export const REPO: string;
export const TOKENS_PATH: string;
/** Every generated file, as { repoRelativePath: contents }. */
export function render(tokens: unknown): Record<string, string>;
/** Repo-relative paths whose contents differ from what the JSON generates. */
export function stale(tokens: unknown): string[];
/** The committed contents of a repo-relative file. */
export function readRepoFile(rel: string): string;
type Rgb = [number, number, number];
/** lt's colours for one palette on one terminal background. */
export function ltScheme(
  tokens: unknown,
  name: string,
  variant: "light" | "dark",
): Record<
  | "canvas" | "text" | "dim" | "accent" | "border" | "selection" | "mention" | "ok" | "info" | "warn" | "danger" | "neutral"
  | "diff_add" | "diff_del" | "diff_comment" | "agent",
  Rgb
> & { presence: Rgb[] };
/** The common terminal backgrounds lt's colours are checked against. */
export const LT_TERMINAL_BACKGROUNDS: Record<"light" | "dark", Record<string, Rgb>>;
