// Copy for the canvas-mode tabs and the tour steps that describe them, kept in
// one place so the tour, the tab tooltips and the palette say the same thing.
//
// The Diff tab shows the uncommitted git changes in the folder the window shows
// for the chat (its checkout: the chat worktree, or the workspace folder;
// DiffView → get_workspace_diffs), not agents' proposals: those are reviewed in
// the Review pane.

export const CANVAS_TAB_HINT = {
  chat: "The conversation",
  diff: "Uncommitted changes in this chat's checkout",
  code: "Code editor, file tree and terminal",
} as const;

export const TOUR_CANVAS_TABS = {
  title: "Chat · Diff · Code",
  body: "Switch the canvas between the conversation, the uncommitted changes in this chat's checkout, and a full code editor.",
};

export const TOUR_DIFF = {
  title: "Uncommitted changes",
  body: "The Diff tab shows what's changed in this chat's checkout but not yet committed, file by file. Agents' proposed changes are reviewed in the Review pane; nothing touches your files until you approve one.",
};
