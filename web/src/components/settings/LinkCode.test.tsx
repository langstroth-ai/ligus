import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, act } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { LinkCode } from "./LinkCode";
import * as ipc from "@/lib/ipc";
import { approvalSheet } from "@/state/approvals";

let push: (v: ipc.LinkCodeDto) => void = () => {};

vi.mock("@/lib/ipc", () => ({
  linkCodeShow: vi.fn(),
  linkCodeClose: vi.fn(async () => ({ state: "idle" })),
  onLinkCodeChanged: vi.fn(async (cb: (v: unknown) => void) => {
    push = cb as typeof push;
    return () => {};
  }),
  listApprovals: vi.fn(async () => ({ requests: [], facts: [] })),
}));

const live = (more: Partial<ipc.LinkCodeDto> = {}): ipc.LinkCodeDto => ({
  state: "live",
  workspaceId: "w1",
  code: "K7Q-4MX",
  codeSpelled: "K 7 Q, 4 M X",
  relay: null,
  expiresAt: Date.now() + 10 * 60_000,
  notice: null,
  error: null,
  address: null,
  ...more,
});

function renderIt(props: Partial<Parameters<typeof LinkCode>[0]> = {}) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <LinkCode workspaceId="w1" onRequest={props.onRequest ?? vi.fn()} onState={props.onState} />
    </QueryClientProvider>,
  );
}

describe("LinkCode", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    push = () => {};
  });

  it("shows the code, how to type it, and nothing about a relay on the community one", async () => {
    vi.mocked(ipc.linkCodeShow).mockResolvedValue(live());
    renderIt();
    expect(await screen.findByTestId("link-code")).toHaveTextContent("K7Q-4MX");
    expect(screen.getByRole("img", { name: "Code: K 7 Q, 4 M X" })).toBeTruthy();
    expect(screen.getByTestId("link-code-live")).toHaveTextContent("ligus devices request --code K7Q-4MX");
    expect(screen.getByTestId("link-code-live")).not.toHaveTextContent("--relay");
    expect(ipc.linkCodeShow).toHaveBeenCalledWith("w1");
  });

  it("names a self-hosted relay, and says when the relay restarted", async () => {
    vi.mocked(ipc.linkCodeShow).mockResolvedValue(live({ relay: "https://relay.example.org" }));
    renderIt();
    await screen.findByTestId("link-code");
    expect(screen.getByTestId("link-code-live")).toHaveTextContent("with the relay relay.example.org");
    expect(screen.getByTestId("link-code-live")).toHaveTextContent("--relay https://relay.example.org");
    act(() => push(live({ relay: "https://relay.example.org", notice: "The relay restarted; the code works again." })));
    expect(await screen.findByTestId("link-code-notice")).toHaveTextContent("restarted");
  });

  it("a relay without short codes hands over to the approval address", async () => {
    vi.mocked(ipc.linkCodeShow).mockResolvedValue(live({ state: "noRoutes", code: null }));
    const onState = vi.fn();
    renderIt({ onState });
    expect(await screen.findByTestId("link-code-noRoutes")).toHaveTextContent("approval address");
    await waitFor(() => expect(onState).toHaveBeenLastCalledWith("noRoutes"));
  });

  it("once used, opens the computer's request when it reaches the Inbox", async () => {
    vi.mocked(ipc.linkCodeShow).mockResolvedValue(live());
    const onRequest = vi.fn();
    const open = vi.spyOn(approvalSheet, "open");
    vi.mocked(ipc.listApprovals).mockResolvedValue({
      requests: [{ workspaceId: "w1", rid: "r1", kind: "computer", createdAt: Date.now() + 1000 } as unknown as ipc.ApprovalDto],
      facts: [],
    });
    renderIt({ onRequest });
    await screen.findByTestId("link-code");
    act(() => push(live({ state: "used" })));
    await waitFor(() => expect(open).toHaveBeenCalledWith("w1", "r1"));
    expect(onRequest).toHaveBeenCalled();
  });

  it("closing takes the code back", async () => {
    vi.mocked(ipc.linkCodeShow).mockResolvedValue(live());
    const { unmount } = renderIt();
    await screen.findByTestId("link-code");
    unmount();
    expect(ipc.linkCodeClose).toHaveBeenCalled();
  });
});
