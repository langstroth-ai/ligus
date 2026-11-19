// The workspace the window shows. Settings and grant commands send it as
// `workspaceId`, and the desktop refuses the change when another workspace
// became active in between (a switch from another window, a link, the
// palette), so a change never lands on a workspace the person wasn't
// looking at. App sets it every time it renders a workspace.
let shown = "";

export function setShownWorkspace(id: string): void {
  shown = id;
}

export function shownWorkspace(): string {
  return shown;
}
