import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ChatRepoChip } from "@/components/ChatRepoChip";
import { RepositoriesSection } from "@/components/settings/RepositoriesSection";
import { formatScratchDate, scratchBaseName, suggestKeepPath } from "@/components/ScratchRepo";
import * as ipc from "@/lib/ipc";

vi.mock("@/lib/ipc", () => ({
  listWorkspaceRepos: vi.fn(),
  chatWorkspaceRepo: vi.fn(),
  setChatWorkspaceRepo: vi.fn(async () => undefined),
  repinChatBase: vi.fn(async () => ""),
  addWorkspaceRepo: vi.fn(),
  bindRepoCheckout: vi.fn(),
  unbindRepoCheckout: vi.fn(),
  removeWorkspaceRepo: vi.fn(),
  setRepoAliases: vi.fn(),
  setHostAlias: vi.fn(),
  acceptRepoAlias: vi.fn(),
  renameWorkspaceRepo: vi.fn(),
  rescanRepos: vi.fn(async () => 0),
  setRepoCoverageReporting: vi.fn(),
  pickWorkspaceFolder: vi.fn(async () => null),
  cloneWorkspaceRepo: vi.fn(),
  listScratchRepos: vi.fn(),
  createScratchRepo: vi.fn(),
  keepScratchRepo: vi.fn(),
  publishScratchRepo: vi.fn(async () => ({ url: "https://github.com/ana/spike", private: true })),
  removeScratchRepo: vi.fn(async () => undefined),
  setScratchSettings: vi.fn(),
  forgesList: vi.fn(async () => [
    { id: "github:github.com", kind: "github", displayName: "GitHub", baseUrl: "https://github.com", authSource: "gh", hasRead: true, hasWrite: true, user: "ana", deviceFlow: false, openLabel: "", holder: "this device", available: true },
  ]),
}));

const REMOVE_AT = Math.floor(new Date(2026, 11, 9, 12).getTime() / 1000);

const entry = (over: Partial<ipc.ScratchRepoDto> = {}): ipc.ScratchRepoDto => ({
  id: "sc-1",
  name: "spike",
  folder: "spike-1a2b3c4d",
  path: "/Users/ana/Library/Application Support/ai.ligus.app/scratch/spike-1a2b3c4d",
  state: "live",
  repoId: "r-sc",
  createdAt: REMOVE_AT - 14 * 86400,
  lastActivity: REMOVE_AT - 14 * 86400,
  removeAt: REMOVE_AT,
  noticeFrom: REMOVE_AT - 2 * 86400,
  notice: true,
  chats: 1,
  published: null,
  ...over,
});

const repos = (over: Partial<ipc.WorkspaceReposDto> = {}): ipc.WorkspaceReposDto => ({
  repos: [
    {
      id: "r1",
      name: "WeFit",
      canonical: "github.com/zazulam/wefit",
      forgeKind: "github",
      defaultBranch: "main",
      removed: false,
      here: { state: "present", code: "present", viaFork: false, path: "/Users/ana/wefit", found: [] },
      hosts: [],
    },
    {
      id: "r-sc",
      name: "spike",
      canonical: "local:9f2c41d07a",
      forgeKind: "local",
      defaultBranch: "main",
      removed: false,
      here: { state: "present", code: "present", viaFork: false, path: entry().path, found: [] },
      hosts: [],
      scratch: true,
      scratchInfo: entry(),
    },
  ],
  canAdmin: true,
  suggestion: null,
  hostLabel: "This Mac",
  coverageReporting: true,
  scanRoots: [],
  ...over,
});

const chatRepo = (over: Partial<ipc.ChatWorkspaceRepoDto> = {}): ipc.ChatWorkspaceRepoDto => ({
  id: "r-sc",
  name: "spike",
  canonical: "local:9f2c41d07a",
  removed: false,
  baseBranch: "main",
  basePinned: false,
  here: { state: "present", reason: "" },
  scratch: true,
  scratchInfo: entry(),
  ...over,
});

const settings = (cleanupDays: number | null = null): ipc.ScratchReposDto => ({
  scratch: [entry(), entry({ id: "sc-2", name: "kept-one", folder: "kept-one-00ff00ff", state: "kept", removeAt: null, notice: false })],
  settings: { root: "/tmp/scratch", rootIsDefault: true, cleanupDays, noticeDays: 2, maxPerHost: 20, maxPerMember: 10, live: 1 },
});

function wrap(ui: React.ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>);
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(ipc.listWorkspaceRepos).mockResolvedValue(repos());
  vi.mocked(ipc.chatWorkspaceRepo).mockResolvedValue(chatRepo());
  vi.mocked(ipc.listScratchRepos).mockResolvedValue(settings());
  vi.mocked(ipc.setScratchSettings).mockResolvedValue(settings(0));
  vi.mocked(ipc.keepScratchRepo).mockResolvedValue({ ...entry({ state: "kept", path: "/Users/ana/code/spike" }), workspaceFlagDropped: true });
});

describe("scratch repos in the chat's repo picker", () => {
  it("offers New scratch repo first and makes one for this chat", async () => {
    vi.mocked(ipc.chatWorkspaceRepo).mockResolvedValue(null);
    vi.mocked(ipc.createScratchRepo).mockResolvedValue({ ...entry(), repoId: "r-sc" });
    wrap(<ChatRepoChip sessionId="s1" />);
    await userEvent.click(await screen.findByLabelText("Chat repository"));
    const items = within(screen.getByRole("menu")).getAllByRole("menuitem");
    expect(items[0].textContent).toBe("New scratch repo");
    expect(items[1].textContent).toContain("WeFit");
    // The scratch repo's row carries the tag.
    expect(items[2].textContent).toContain("spike");
    expect(within(items[2]).getByText("scratch")).toBeTruthy();
    await userEvent.click(items[0]);
    expect(ipc.createScratchRepo).toHaveBeenCalledWith("s1");
  });

  it("shows a refusal inline", async () => {
    vi.mocked(ipc.chatWorkspaceRepo).mockResolvedValue(null);
    vi.mocked(ipc.createScratchRepo).mockRejectedValue(new Error("only an Admin can add a repository"));
    wrap(<ChatRepoChip sessionId="s1" />);
    await userEvent.click(await screen.findByLabelText("Chat repository"));
    await userEvent.click(screen.getByText("New scratch repo"));
    expect((await screen.findByRole("alert")).textContent).toContain("only an Admin can add a repository");
  });

  it("tags the chat's scratch repo and shows the removal notice with its date", async () => {
    wrap(<ChatRepoChip sessionId="s1" />);
    const chip = await screen.findByLabelText("Chat repository");
    expect(within(chip).getByText("scratch")).toBeTruthy();
    const notice = await screen.findByRole("status");
    expect(notice.textContent).toBe(`This scratch repo will be removed on ${formatScratchDate(REMOVE_AT)}·Keep it`);
    expect(formatScratchDate(REMOVE_AT)).toBe(new Date(REMOVE_AT * 1000).toLocaleDateString(undefined, { dateStyle: "medium" }));
  });

  it("shows no notice outside the notice window", async () => {
    vi.mocked(ipc.chatWorkspaceRepo).mockResolvedValue(chatRepo({ scratchInfo: entry({ notice: false }) }));
    wrap(<ChatRepoChip sessionId="s1" />);
    await screen.findByLabelText("Chat repository");
    expect(screen.queryByRole("status")).toBeNull();
  });
});

describe("Keep it as a repo", () => {
  it("suggests ~/projects/<name> and keeps it at the folder given", async () => {
    expect(scratchBaseName("spike-1a2b3c4d")).toBe("spike");
    expect(suggestKeepPath(entry())).toBe("/Users/ana/projects/spike");
    wrap(<ChatRepoChip sessionId="s1" />);
    await userEvent.click(await screen.findByText("Keep it"));
    const dlg = within(screen.getByRole("dialog"));
    const folder = dlg.getByLabelText("Folder") as HTMLInputElement;
    expect(folder.value).toBe("/Users/ana/projects/spike");
    await userEvent.clear(folder);
    await userEvent.type(folder, "/Users/ana/code/spike");
    await userEvent.click(dlg.getByRole("button", { name: "Keep it" }));
    expect(ipc.keepScratchRepo).toHaveBeenCalledWith("sc-1", "/Users/ana/code/spike", null);
    expect(ipc.publishScratchRepo).not.toHaveBeenCalled();
  });

  it("publishes only after the confirmation, passing owner/name exactly", async () => {
    wrap(<ChatRepoChip sessionId="s1" />);
    await userEvent.click(await screen.findByLabelText("Chat repository"));
    await userEvent.click(screen.getByText("Keep it as a repo…"));
    let dlg = within(screen.getByRole("dialog"));
    await userEvent.click(dlg.getByLabelText("Also publish to GitHub (private)"));
    // The owner defaults to the GitHub login on this device.
    expect(((await dlg.findByDisplayValue("ana")) as HTMLInputElement).getAttribute("aria-label")).toBe("GitHub owner");
    await userEvent.click(dlg.getByRole("button", { name: "Keep it" }));
    expect(ipc.keepScratchRepo).toHaveBeenCalledTimes(1);
    dlg = within(await screen.findByRole("dialog"));
    expect(dlg.getByText(/Create the private repository/).textContent).toBe("Create the private repository ana/spike on GitHub and push its branches?");
    expect(ipc.publishScratchRepo).not.toHaveBeenCalled();
    await userEvent.click(dlg.getByRole("button", { name: "Create and push" }));
    expect(ipc.publishScratchRepo).toHaveBeenCalledWith("sc-1", "ana", "spike", "ana/spike", "github:github.com");
  });

  it("doesn't publish when the confirmation is declined", async () => {
    wrap(<ChatRepoChip sessionId="s1" />);
    await userEvent.click(await screen.findByText("Keep it"));
    const dlg = within(screen.getByRole("dialog"));
    await userEvent.click(dlg.getByLabelText("Also publish to GitHub (private)"));
    await userEvent.click(dlg.getByRole("button", { name: "Keep it" }));
    await userEvent.click(await screen.findByRole("button", { name: "Not now" }));
    expect(ipc.publishScratchRepo).not.toHaveBeenCalled();
  });
});

describe("Settings > Repositories > Scratch repositories", () => {
  it("tags scratch rows and lists this device's scratch repos with their removal date or kept", async () => {
    wrap(<RepositoriesSection />);
    const part = (await screen.findByRole("heading", { name: "Scratch repositories" })).closest("section")!;
    const list = within(part as HTMLElement).getByRole("list", { name: "Scratch repositories" });
    expect(within(list).getByText(`removed on ${formatScratchDate(REMOVE_AT)}`)).toBeTruthy();
    expect(within(list).getByText("kept")).toBeTruthy();
    // The tag marks the live one; a kept one is a repository like any other.
    expect(within(list).getAllByText("scratch")).toHaveLength(1);
    // The workspace list's scratch row carries the tag too.
    expect(screen.getAllByText("scratch").length).toBeGreaterThanOrEqual(2);
  });

  it("turns the cleanup off", async () => {
    wrap(<RepositoriesSection />);
    const sel = await screen.findByLabelText("Scratch cleanup");
    await userEvent.selectOptions(sel, "0");
    expect(ipc.setScratchSettings).toHaveBeenCalledWith({ cleanupDays: 0 });
    await userEvent.selectOptions(sel, "default");
    expect(ipc.setScratchSettings).toHaveBeenCalledWith({ resetDays: true });
  });
});
