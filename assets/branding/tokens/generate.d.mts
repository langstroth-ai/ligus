// Types for generate.mjs, for the web tests.
export const REPO: string;
export const TOKENS_PATH: string;
/** Every generated file, as { repoRelativePath: contents }. */
export function render(tokens: unknown): Record<string, string>;
/** Repo-relative paths whose contents differ from what the JSON generates. */
export function stale(tokens: unknown): string[];
/** The committed contents of a repo-relative file. */
export function readRepoFile(rel: string): string;
