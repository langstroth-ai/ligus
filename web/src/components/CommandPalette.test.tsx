import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { CommandPalette, type PaletteActions } from "./CommandPalette";

vi.mock("@/lib/ipc", () => ({
  listChats: vi.fn(async () => [
    { id: "c1", title: "Fix login flow", archived: false, lastActivityAt: "", messageCount: 3 },
    { id: "c2", title: "Archived thing", archived: true, lastActivityAt: "", messageCount: 1 },
  ]),
  listWorkspaces: vi.fn(async () => [
    { id: "w1", name: "My workspace", kind: "local", active: true },
    { id: "w2", name: "team-alpha", kind: "room", active: false },
  ]),
}));

function setup(actions: Partial<PaletteActions> = {}) {
  const acts: PaletteActions = {
    newChat: vi.fn(),
    openSettings: vi.fn(),
    selectChat: vi.fn(),
    selectWorkspace: vi.fn(),
    ...actions,
  };
  const onClose = vi.fn();
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={qc}>
      <CommandPalette open onClose={onClose} actions={acts} />
    </QueryClientProvider>,
  );
  return { acts, onClose };
}

describe("CommandPalette", () => {
  beforeEach(() => vi.clearAllMocks());

  it("lists actions, workspaces, and non-archived chats", async () => {
    setup();
    expect(await screen.findByText("New chat")).toBeInTheDocument();
    expect(screen.getByText("Open settings")).toBeInTheDocument();
    expect(await screen.findByText("Switch to team-alpha")).toBeInTheDocument();
    expect(await screen.findByText("Fix login flow")).toBeInTheDocument();
    // Archived chats are excluded.
    expect(screen.queryByText("Archived thing")).not.toBeInTheDocument();
  });

  it("filters by query", async () => {
    const user = userEvent.setup();
    setup();
    await screen.findByText("Switch to team-alpha");
    await user.type(screen.getByPlaceholderText(/search/i), "team");
    expect(screen.getByText("Switch to team-alpha")).toBeInTheDocument();
    expect(screen.queryByText("New chat")).not.toBeInTheDocument();
    expect(screen.queryByText("Fix login flow")).not.toBeInTheDocument();
  });

  it("runs the matching action on Enter and closes", async () => {
    const user = userEvent.setup();
    const { acts, onClose } = setup();
    await screen.findByText("Fix login flow");
    await user.type(screen.getByPlaceholderText(/search/i), "fix login");
    await user.keyboard("{Enter}");
    expect(acts.selectChat).toHaveBeenCalledWith("c1");
    expect(onClose).toHaveBeenCalled();
  });

  it("navigates with ArrowDown and selects the active item", async () => {
    const user = userEvent.setup();
    const { acts } = setup();
    await screen.findByText("New chat");
    await user.click(screen.getByPlaceholderText(/search/i)); // focus the input
    // First item is "New chat"; ArrowDown moves to "Open settings".
    await user.keyboard("{ArrowDown}{Enter}");
    expect(acts.openSettings).toHaveBeenCalled();
    expect(acts.newChat).not.toHaveBeenCalled();
  });

  it("closes on Escape", async () => {
    const user = userEvent.setup();
    const { onClose } = setup();
    await screen.findByText("New chat");
    await user.click(screen.getByPlaceholderText(/search/i)); // focus the input
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalled();
  });

  it("reaches every pane and every settings section, with honest shortcut hints", async () => {
    const { PANES } = await import("@/lib/panes");
    const { SETTINGS_TABS } = await import("./SettingsView");
    const openPane = vi.fn();
    const openSettingsTab = vi.fn();
    setup({ openPane, openSettingsTab });
    for (const p of PANES) expect(screen.getByText(`Open ${p.label}`)).toBeInTheDocument();
    for (const t of SETTINGS_TABS) expect(screen.getByText(`Settings: ${t.label}`)).toBeInTheDocument();
    // ⌘⌥1–9 cover the first nine panes; the tenth is palette-only, so no fake key.
    expect(PANES.length).toBeGreaterThan(9);
    expect(screen.getByText("Pane · ⌘⌥9")).toBeInTheDocument();
    expect(screen.queryByText("Pane · ⌘⌥10")).not.toBeInTheDocument();
    const last = PANES[PANES.length - 1];
    await userEvent.setup().click(screen.getByText(`Open ${last.label}`));
    expect(openPane).toHaveBeenCalledWith(last.id);
  });

  it("offers Admit a box only when the workspace can admit one", async () => {
    const admitBox = vi.fn();
    setup({ admitBox });
    await userEvent.setup().click(screen.getByText("Admit a box…"));
    expect(admitBox).toHaveBeenCalledTimes(1);
  });

  it("has no Admit a box entry without the action (a personal workspace)", () => {
    setup();
    expect(screen.queryByText("Admit a box…")).not.toBeInTheDocument();
  });
});

