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
  { id: "me", actorId: "me", displayName: "Ana", role: "admin", title: "", index: 1, isSelf: true, avatarUrl: null, kind: "human", isFounder: false, removeRefusal: null, roleOptions: [], regrantRefusal: null },
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
        { id: HOST, kind: "worker", label: "sff", online: true, lastSeen: "", modelGateways: [], ownerName: "", mine: false },
        { id: "not-a-uuid", kind: "device", label: "legacy", online: false, lastSeen: "", modelGateways: [], ownerName: "", mine: false },
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

const card = async (name: string) => (await screen.findByText(name)).closest("[class*='px-3 py-3']") as HTMLElement;

beforeEach(() => vi.mocked(invoke).mockClear());

describe("the People pane follows the rank rules", () => {
  it("disables what the fold refuses and says why", async () => {
    rail("people");
    const maya = within(await card("Maya"));
    const remove = maya.getByRole("button", { name: "Remove" });
    expect(remove).toBeDisabled();
    expect(remove).toHaveAttribute("data-tip", "a cut goes strictly down the ranks");
    const revoke = maya.getByRole("button", { name: /Remove & revoke/ });
    expect(revoke).toBeDisabled();
    expect(revoke).toHaveAttribute("data-tip", "a cut goes strictly down the ranks");
    const owner = maya.getByRole("option", { name: "owner" }) as HTMLOptionElement;
    const viewer = maya.getByRole("option", { name: "viewer" }) as HTMLOptionElement;
    expect(owner.disabled).toBe(true);
    expect(owner.title).toBe("no one grants a role above their own");
    expect(viewer.disabled).toBe(true);
    // Every other role is refused: the picker itself is off.
    expect(maya.getByRole("combobox")).toBeDisabled();
  });

  it("tags a service member and keeps passkey re-grants off it", async () => {
    rail("people");
    const box = within(await card("build-box"));
    expect(box.getByText("service")).toBeInTheDocument();
    const regrant = box.getByRole("button", { name: /Allow new passkey/ });
    expect(regrant).toBeDisabled();
    expect(regrant).toHaveAttribute("data-tip", "a service member holds no passkey");
    expect((box.getByRole("option", { name: "admin" }) as HTMLOptionElement).disabled).toBe(true);
    expect((box.getByRole("option", { name: "viewer" }) as HTMLOptionElement).disabled).toBe(false);
    expect(box.getByRole("combobox")).not.toBeDisabled();
    expect(box.getByRole("button", { name: /Remove & revoke/ })).not.toBeDisabled();
  });
});

describe("Apply on a host", () => {
  it("asks the picked host to apply an approved proposal", async () => {
    proposals = [{ ...base, canRequestApply: true }];
    rail("review");
    const button = await screen.findByRole("button", { name: "Apply on sff" });
    expect(screen.queryByRole("option", { name: /legacy/ })).toBeNull();
    fireEvent.click(button);
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
