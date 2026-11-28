import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { RightRail } from "./RightRail";
import type { ProposalDto } from "@/lib/ipc";

// Phase B: the People pane offers only what the fold allows (the backend
// computes each refusal with the fold's own rules), and an approved proposal
// can be applied on one named host.

const HOST = "11111111-2222-4333-8444-555555555555";

const members = [
  { id: "me", actorId: "me", displayName: "Ana", role: "admin", title: "", index: 1, isSelf: true, avatarUrl: null, kind: "human", isFounder: false, removeRefusal: null, roleOptions: [], regrantRefusal: null, leftAt: null },
  {
    id: "maya",
    actorId: "maya",
    displayName: "Maya",
    role: "admin",
    title: "",
    index: 2,
    isSelf: false,
    avatarUrl: null,
    kind: "human",
    isFounder: false,
    removeRefusal: "a cut goes strictly down the ranks",
    roleOptions: [
      { role: "owner", refusal: "no one grants a role above their own" },
      { role: "admin", refusal: null },
      { role: "contributor", refusal: "a cut goes strictly down the ranks" },
      { role: "viewer", refusal: "a cut goes strictly down the ranks" },
    ],
    regrantRefusal: null,
    leftAt: null,
  },
  {
    id: "box",
    actorId: "box",
    displayName: "build-box",
    role: "contributor",
    title: "",
    index: 3,
    isSelf: false,
    avatarUrl: null,
    kind: "service",
    isFounder: false,
    removeRefusal: null,
    roleOptions: [
      { role: "owner", refusal: "a service member holds at most Contributor" },
      { role: "admin", refusal: "a service member holds at most Contributor" },
      { role: "contributor", refusal: null },
      { role: "viewer", refusal: null },
    ],
    regrantRefusal: "a service member holds no passkey",
    leftAt: null,
  },
];

let proposals: ProposalDto[] = [];

const base: ProposalDto = {
  id: "p1",
  title: "Add new.txt",
  body: "",
  authorActorId: "agent",
  kind: "fileDiff",
  status: "approved",
  requiredApprovals: 1,
  qualifyingApprovals: 1,
  quorumMet: true,
  approvals: [],
  createdAt: "2026-11-01T12:00:00Z",
  dismissed: false,
  refinements: [],
};

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === "list_members") return members;
    if (cmd === "list_proposals") return proposals;
    if (cmd === "list_workspace_hosts")
      return [
        { id: HOST, kind: "worker", label: "sff", online: true, lastSeen: "", modelGateways: [], ownerName: "", mine: false, offers: [], readiness: "", build: "", upgradeTo: "" },
        { id: "not-a-uuid", kind: "device", label: "legacy", online: false, lastSeen: "", modelGateways: [], ownerName: "", mine: false, offers: [], readiness: "", build: "", upgradeTo: "" },
      ];
    if (cmd === "request_apply_on_host") return "r-new";
    if (cmd?.startsWith("list_")) return [];
    return null;
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

function rail(pane: "people" | "review") {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <RightRail width={400} sessionId="s1" pane={pane} activeRuntimeId="" onChangePane={() => {}} onClose={() => {}} />
    </QueryClientProvider>,
  );
}

// Member actions live in the row menu (right-click, the "⋯", Shift+F10).
const openMenu = async (name: string) => {
  fireEvent.click(await screen.findByRole("button", { name: `Actions for ${name}` }));
  return within(await screen.findByRole("menu"));
};

beforeEach(() => vi.mocked(invoke).mockClear());

describe("the People pane follows the rank rules", () => {
  it("disables what the fold refuses and says why", async () => {
    rail("people");
    const menu = await openMenu("Maya");
    const remove = menu.getByRole("menuitem", { name: "Remove" });
    expect(remove).toHaveAttribute("aria-disabled", "true");
    expect(remove).toHaveAttribute("data-tip", "a cut goes strictly down the ranks");
    const revoke = menu.getByRole("menuitem", { name: /Remove & revoke/ });
    expect(revoke).toHaveAttribute("aria-disabled", "true");
    expect(revoke).toHaveAttribute("data-tip", "a cut goes strictly down the ranks");
    const owner = menu.getByRole("menuitem", { name: "Change role to owner" });
    expect(owner).toHaveAttribute("aria-disabled", "true");
    expect(owner).toHaveAttribute("data-tip", "no one grants a role above their own");
    // Every other role is refused too.
    for (const item of menu.getAllByRole("menuitem", { name: /^Change role to / })) {
      expect(item).toHaveAttribute("aria-disabled", "true");
    }
  });

  it("tags a service member and keeps passkey re-grants off it", async () => {
    rail("people");
    expect(within((await screen.findByText("build-box")).closest("li") as HTMLElement).getByText("service")).toBeInTheDocument();
    const menu = await openMenu("build-box");
    const regrant = menu.getByRole("menuitem", { name: /Allow new passkey/ });
    expect(regrant).toHaveAttribute("aria-disabled", "true");
    expect(regrant).toHaveAttribute("data-tip", "a service member holds no passkey");
    expect(menu.getByRole("menuitem", { name: "Change role to admin" })).toHaveAttribute("aria-disabled", "true");
    expect(menu.getByRole("menuitem", { name: "Change role to viewer" })).not.toHaveAttribute("aria-disabled");
    expect(menu.getByRole("menuitem", { name: /Remove & revoke/ })).not.toHaveAttribute("aria-disabled");
  });
});

describe("Apply on a host", () => {
  it("asks the picked host to apply an approved proposal", async () => {
    proposals = [{ ...base, canRequestApply: true }];
    rail("review");
    // "Apply on <host>" lives in the proposal's row menu, one item per host.
    fireEvent.click(await screen.findByRole("button", { name: /^Actions for / }));
    const item = await screen.findByRole("menuitem", { name: "Apply on sff" });
    expect(screen.queryByRole("menuitem", { name: /legacy/ })).toBeNull();
    fireEvent.click(item);
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("request_apply_on_host", { sessionId: "s1", proposalId: "p1", hostDeviceId: HOST }),
    );
  });

  it("shows an open request with Withdraw and turns Implement off", async () => {
    proposals = [{ ...base, applyRequests: [{ id: "r1", hostDeviceId: HOST, hostLabel: "sff", state: "open", mine: true }] }];
    rail("review");
    expect(await screen.findByText(/Waiting for sff to apply/)).toBeInTheDocument();
    const implement = screen.getByRole("button", { name: /Implement/ });
    expect(implement).toBeDisabled();
    expect(implement.getAttribute("data-tip")).toMatch(/one applier/i);
    fireEvent.click(screen.getByRole("button", { name: "Withdraw" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("withdraw_apply_request", { sessionId: "s1", requestId: "r1" }));
  });

  it("shows why a host couldn't apply it, and what it applied", async () => {
    proposals = [
      {
        ...base,
        canRequestApply: true,
        applyRequests: [
          { id: "r1", hostDeviceId: HOST, hostLabel: "sff", state: "failed", reason: "the checkout moved", mine: false },
          { id: "r2", hostDeviceId: HOST, state: "withdrawn", mine: false },
        ],
      },
    ];
    rail("review");
    expect(await screen.findByText(/sff couldn't apply it: the checkout moved/)).toBeInTheDocument();
    expect(screen.getByText("Withdrawn")).toBeInTheDocument();
  });

  it("shows the applied commit", async () => {
    proposals = [
      {
        ...base,
        status: "applied",
        applyRequests: [{ id: "r1", hostDeviceId: HOST, hostLabel: "sff", state: "applied", commit: "0123456789abcdef0123", mine: true }],
      },
    ];
    rail("review");
    // Settled work starts collapsed.
    fireEvent.click(await screen.findByText(/1 settled proposal/));
    expect(await screen.findByText(/Applied on sff \(0123456789ab\)/)).toBeInTheDocument();
  });
});
