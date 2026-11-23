import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RepositoriesSection, RepoSuggestionNotice, coverageLabel } from "./RepositoriesSection";
import { ChatRepoChip, PinnedHostWarning, pinnedWarning, repoWarning } from "@/components/ChatRepoChip";
import * as ipc from "@/lib/ipc";

const data = (over: Partial<ipc.WorkspaceReposDto> = {}): ipc.WorkspaceReposDto => ({
  repos: [
    {
      id: "r1",
      name: "WeFit",
      canonical: "github.com/zazulam/wefit",
      forgeKind: "github",
      defaultBranch: "main",
      removed: false,
      here: { state: "missing", code: "unconfirmed", viaFork: false, path: null, found: ["/Users/me/apps/WeFit"] },
      hosts: [
        { hostId: "h1", label: "mac-test", kind: "worker", online: true, state: "present", viaFork: false },
        { hostId: "h2", label: "Alice's laptop", kind: "device", online: false, state: "unknown", viaFork: false },
      ],
    },
  ],
  canAdmin: true,
  suggestion: null,
  hostLabel: "This Mac",
  coverageReporting: true,
  scanRoots: [],
  ...over,
});

vi.mock("@/lib/ipc", () => ({
  listWorkspaceRepos: vi.fn(),
  addWorkspaceRepo: vi.fn(async () => ({ id: "r2", canonical: "github.com/a/b", name: "b" })),
  bindRepoCheckout: vi.fn(async () => undefined),
  unbindRepoCheckout: vi.fn(async () => undefined),
  removeWorkspaceRepo: vi.fn(async () => undefined),
  setRepoAliases: vi.fn(async () => []),
  setHostAlias: vi.fn(async () => undefined),
  acceptRepoAlias: vi.fn(async () => ""),
  renameWorkspaceRepo: vi.fn(async () => undefined),
  rescanRepos: vi.fn(async () => 0),
  setRepoCoverageReporting: vi.fn(async () => undefined),
  pickWorkspaceFolder: vi.fn(async () => null),
  cloneWorkspaceRepo: vi.fn(async () => ({ path: "/Users/me/code/wefit-1a2b3c4d", rootMatches: true })),
  chatWorkspaceRepo: vi.fn(),
  setChatWorkspaceRepo: vi.fn(async () => undefined),
  repinChatBase: vi.fn(async () => "89abcdef0123456789abcdef0123456789abcdef"),
}));

function wrap(ui: React.ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>);
}

beforeEach(() => vi.clearAllMocks());

describe("RepositoriesSection", () => {
  it("shows synced aliases, accepts one on this device, and lets an admin add a host alias", async () => {
    const d = data({ hostAliases: [{ alias: "ssh.git.example.com", host: "git.example.com", accepted: false }] });
    d.repos[0].aliases = [{ alias: "github.com/old-org/wefit", accepted: false }];
    d.repos[0].here = { state: "present", code: "", viaFork: false, viaAlias: true, path: "/Users/me/apps/WeFit", found: [] };
    vi.mocked(ipc.listWorkspaceRepos).mockResolvedValue(d);
    wrap(<RepositoriesSection />);
    expect(await screen.findByText(/via alias/)).toBeTruthy();
    expect(screen.getByText("github.com/old-org/wefit")).toBeTruthy();
    await userEvent.click(screen.getByLabelText("Accept github.com/old-org/wefit here"));
    expect(ipc.acceptRepoAlias).toHaveBeenCalledWith("github.com/old-org/wefit", true, "r1");
    await userEvent.click(screen.getByLabelText("Accept ssh.git.example.com here"));
    expect(ipc.acceptRepoAlias).toHaveBeenCalledWith("ssh.git.example.com", true, "git.example.com");
    await userEvent.type(screen.getByLabelText("Alias host"), "ssh2.git.example.com");
    await userEvent.type(screen.getByLabelText("Repository host"), "git.example.com");
    await userEvent.click(screen.getByText("Add alias"));
    expect(ipc.setHostAlias).toHaveBeenCalledWith("ssh2.git.example.com", "git.example.com");
  });

  it("hides host aliases from a member when there are none", async () => {
    vi.mocked(ipc.listWorkspaceRepos).mockResolvedValue(data({ canAdmin: false }));
    wrap(<RepositoriesSection />);
    expect(await screen.findByText("WeFit")).toBeTruthy();
    expect(screen.queryByText("Host aliases")).toBeNull();
  });

  it("shows other hosts' coverage by label, and confirms a found checkout", async () => {
    vi.mocked(ipc.listWorkspaceRepos).mockResolvedValue(data());
    wrap(<RepositoriesSection />);
    expect(await screen.findByText("WeFit")).toBeTruthy();
    expect(screen.getByText(/mac-test: checked out/)).toBeTruthy();
    expect(screen.getByText(/Alice's laptop: unknown/)).toBeTruthy();
    await userEvent.click(screen.getByText("Use it"));
    expect(ipc.bindRepoCheckout).toHaveBeenCalledWith("r1", "/Users/me/apps/WeFit");
  });

  it("lets an admin add by URL; a member is told to ask", async () => {
    vi.mocked(ipc.listWorkspaceRepos).mockResolvedValue(data());
    wrap(<RepositoriesSection />);
    await userEvent.type(await screen.findByLabelText("Repository URL"), "git@github.com:a/b.git");
    await userEvent.click(screen.getByText("Add repository"));
    expect(ipc.addWorkspaceRepo).toHaveBeenCalledWith("git@github.com:a/b.git");
  });

  it("hides admin actions from members", async () => {
    vi.mocked(ipc.listWorkspaceRepos).mockResolvedValue(data({ canAdmin: false }));
    wrap(<RepositoriesSection />);
    expect(await screen.findByText(/Ask an admin to add a repository/)).toBeTruthy();
    expect(screen.queryByText("Remove")).toBeNull();
  });

  it("offers to add the bound folder's repo", async () => {
    vi.mocked(ipc.listWorkspaceRepos).mockResolvedValue(
      data({ repos: [], suggestion: { canonical: "github.com/zazulam/wefit", name: "WeFit", viaFork: false, path: "/Users/me/apps/WeFit" } }),
    );
    wrap(<RepoSuggestionNotice />);
    expect(await screen.findByText(/Add WeFit to the workspace/)).toBeTruthy();
    await userEvent.click(screen.getByText("Add"));
    expect(ipc.addWorkspaceRepo).toHaveBeenCalledWith("github.com/zazulam/wefit", { name: "WeFit", path: "/Users/me/apps/WeFit" });
  });

  it("clones a repo that isn't here into a picked folder, by id", async () => {
    vi.mocked(ipc.listWorkspaceRepos).mockResolvedValue(data());
    vi.mocked(ipc.pickWorkspaceFolder).mockResolvedValueOnce("/Users/me/code");
    wrap(<RepositoriesSection />);
    await userEvent.click(await screen.findByText("Clone into…"));
    expect(ipc.cloneWorkspaceRepo).toHaveBeenCalledWith("r1", "/Users/me/code");
  });

  it("doesn't offer to clone a local-only repo, or when the picker is cancelled", async () => {
    const d = data();
    d.repos[0].canonical = `local:${"a".repeat(40)}`;
    vi.mocked(ipc.listWorkspaceRepos).mockResolvedValue(d);
    wrap(<RepositoriesSection />);
    expect(await screen.findByText("Bind folder…")).toBeTruthy();
    expect(screen.queryByText("Clone into…")).toBeNull();
    expect(ipc.cloneWorkspaceRepo).not.toHaveBeenCalled();
  });

  it("words coverage states", () => {
    expect(coverageLabel("present", true)).toBe("checked out (via a fork)");
    expect(coverageLabel("quantum")).toBe("unknown (doesn't report)");
  });
});

describe("ChatRepoChip", () => {
  it("names the chat's repo and warns when this device lacks it", async () => {
    vi.mocked(ipc.listWorkspaceRepos).mockResolvedValue(data());
    vi.mocked(ipc.chatWorkspaceRepo).mockResolvedValue({
      id: "r1",
      name: "WeFit",
      canonical: "github.com/zazulam/wefit",
      removed: false,
      baseBranch: "main",
      basePinned: false,
      here: { state: "no_checkout", reason: "No checkout of WeFit on This Mac. Clone it here or bind a folder." },
    });
    wrap(<ChatRepoChip sessionId="s1" />);
    expect(await screen.findByText("WeFit · main")).toBeTruthy();
    expect(screen.getByText(/No checkout of WeFit on This Mac/)).toBeTruthy();
    await userEvent.click(screen.getByLabelText("Chat repository"));
    await userEvent.click(screen.getByText("No repository"));
    expect(ipc.setChatWorkspaceRepo).toHaveBeenCalledWith("s1", null);
  });

  it("offers moving the pinned base only to an Admin or the chat's creator", async () => {
    vi.mocked(ipc.listWorkspaceRepos).mockResolvedValue(data());
    const dto = (canRepin: boolean): ipc.ChatWorkspaceRepoDto => ({
      id: "r1",
      name: "WeFit",
      canonical: "github.com/zazulam/wefit",
      removed: false,
      baseBranch: "main",
      basePinned: true,
      baseCommit: "0123456789abcdef0123456789abcdef01234567",
      canRepin,
      here: { state: "present", reason: "" },
    });
    vi.mocked(ipc.chatWorkspaceRepo).mockResolvedValue(dto(false));
    const first = wrap(<ChatRepoChip sessionId="s1" />);
    await userEvent.click(await screen.findByLabelText("Chat repository"));
    expect(screen.queryByText(/Move the base/)).toBeNull();
    first.unmount();
    vi.mocked(ipc.chatWorkspaceRepo).mockResolvedValue(dto(true));
    wrap(<ChatRepoChip sessionId="s2" />);
    await userEvent.click(await screen.findByLabelText("Chat repository"));
    await userEvent.click(screen.getByText("Move the base to the latest main"));
    expect(ipc.repinChatBase).toHaveBeenCalledWith("s2");
  });

  it("is hidden while there are no repos", async () => {
    vi.mocked(ipc.listWorkspaceRepos).mockResolvedValue(data({ repos: [] }));
    vi.mocked(ipc.chatWorkspaceRepo).mockResolvedValue(null);
    const { container } = wrap(<ChatRepoChip sessionId="s1" />);
    await new Promise((r) => setTimeout(r, 20));
    expect(container.textContent).toBe("");
    expect(repoWarning(null, "x")).toBeNull();
  });
});

const pinned = (canRunHere: boolean): ipc.ChatWorkspaceRepoDto => ({
  id: "r1",
  name: "WeFit",
  canonical: "github.com/zazulam/wefit",
  removed: false,
  baseBranch: "main",
  basePinned: true,
  here: { state: "present", reason: "" },
  pinnedElsewhere: [{ agentId: "a1", agentName: "ScoutBee", hostId: "h1", hostLabel: "mac-test", canRunHere }],
});

describe("PinnedHostWarning", () => {
  it("names the agent's host and the actions for a draft to that agent", async () => {
    vi.mocked(ipc.chatWorkspaceRepo).mockResolvedValue(pinned(true));
    const onRunHere = vi.fn();
    wrap(<PinnedHostWarning sessionId="s1" routeHandle="scoutbee" runHere={false} onRunHere={onRunHere} />);
    expect(await screen.findByText("ScoutBee runs on mac-test, which has no WeFit.")).toBeTruthy();
    // Cloning on another host is the phone's: the desktop says how.
    await userEvent.click(screen.getByText("Clone there"));
    expect(screen.getByText(/from the Ligus phone app connected to it/)).toBeTruthy();
    await userEvent.click(screen.getByText("Run here this time"));
    expect(onRunHere).toHaveBeenCalledWith(true);
  });

  it("says when it runs here, and can undo", async () => {
    vi.mocked(ipc.chatWorkspaceRepo).mockResolvedValue(pinned(true));
    const onRunHere = vi.fn();
    wrap(<PinnedHostWarning sessionId="s1" routeHandle="ScoutBee" runHere onRunHere={onRunHere} />);
    await userEvent.click(await screen.findByText("Undo"));
    expect(onRunHere).toHaveBeenCalledWith(false);
  });

  it("can't run here without the repo, the runtime or ownership; silent for other recipients", async () => {
    vi.mocked(ipc.chatWorkspaceRepo).mockResolvedValue(pinned(false));
    wrap(<PinnedHostWarning sessionId="s1" routeHandle="ScoutBee" runHere={false} onRunHere={() => {}} />);
    const btn = (await screen.findByText("Run here this time")) as HTMLButtonElement;
    expect(btn.disabled).toBe(true);
    expect(pinnedWarning(pinned(true), "primary")).toBeNull();
    expect(pinnedWarning({ ...pinned(true), removed: true }, "ScoutBee")).toBeNull();
    expect(pinnedWarning(null, "ScoutBee")).toBeNull();
  });
});
