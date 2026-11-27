import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { SetupWizard } from "./SetupWizard";
import * as ipc from "@/lib/ipc";
import { ADMIT_BOX_EVENT } from "@/lib/nav";

const flowDef = (id: string, title: string) => ({ id, title, summary: "", why: `why ${id}`, keywords: "", minRole: null, personSteps: false });

const agentForm = (inputs: Record<string, string>) => ({
  flow: flowDef("agent", "Add an agent"),
  preflight: [{ id: "agent.sandbox", title: "agent sandbox", status: "ok", detail: "seatbelt" }],
  blocked: false,
  steps: [
    {
      id: "harness",
      title: "Harness",
      kind: "input",
      fields: [
        {
          id: "harness",
          label: "Harness",
          type: "choice",
          required: true,
          options: [
            { value: "claude-code", label: "Claude Code", hint: "signed in" },
            { value: "codex", label: "Codex", hint: "not signed in" },
          ],
        },
      ],
    },
    { id: "name", title: "Name", kind: "input", fields: [{ id: "name", label: "Name", type: "text", required: true, help: "You @mention it by this name." }] },
    {
      id: "model",
      title: "Model",
      kind: "input",
      fields: [{ id: "model", label: "Model", type: "suggest", required: false, options: inputs.harness === "claude-code" ? [{ value: "opus", label: "Opus" }] : [] }],
    },
    { id: "verify", title: "Ready", kind: "verify" },
  ],
});

const toolForm = {
  flow: flowDef("tool", "Connect a tool"),
  preflight: [],
  blocked: false,
  steps: [
    {
      id: "server",
      title: "Server",
      kind: "input",
      fields: [
        { id: "id", label: "Name", type: "text", required: true },
        { id: "kind", label: "Kind", type: "choice", required: true, default: "url", options: [{ value: "command", label: "A command" }, { value: "url", label: "A URL" }] },
        { id: "url", label: "URL", type: "text", required: true, when: ["kind", ["url"]] },
      ],
    },
    { id: "token", title: "Token", kind: "input", fields: [{ id: "token", label: "Bearer token", type: "secret", required: false, when: ["kind", ["url"]] }] },
    { id: "verify", title: "Connected", kind: "verify" },
  ],
};

const personForm = (id: string, title: string) => ({
  flow: flowDef(id, title),
  preflight: [],
  blocked: false,
  steps: [{ id: "x", title: "Hand-off", kind: "person" }, { id: "verify", title: "Done", kind: "verify" }],
});

const outcome = (over: Partial<ipc.SetupOutcome>): ipc.SetupOutcome => ({
  schema: "ligus.checks.v1",
  command: "setup agent",
  ok: true,
  exitCode: 0,
  checks: [{ id: "agent.agent", title: "agent", status: "ok", detail: "@builder created" }],
  next: [],
  flow: "agent",
  done: true,
  dryRun: false,
  results: {},
  ...over,
});

vi.mock("@/lib/ipc", () => ({
  setupForm: vi.fn(),
  setupRun: vi.fn(),
  setupCancel: vi.fn(async () => {}),
  workspaceInvite: vi.fn(async () => "hivews1:demo-invite"),
  pickWorkspaceFolder: vi.fn(async () => "/tmp/repo"),
}));

vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));

function open(flow: ipc.SetupFlowId, onClose = vi.fn()) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={qc}>
      <SetupWizard flow={flow} workspaceId="w1" onClose={onClose} />
    </QueryClientProvider>,
  );
  return onClose;
}

describe("SetupWizard", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(ipc.setupForm).mockImplementation(async (flow, inputs) => {
      if (flow === "agent") return agentForm(inputs) as unknown as ipc.SetupForm;
      if (flow === "tool") return toolForm as unknown as ipc.SetupForm;
      if (flow === "invite") return personForm("invite", "Invite a person") as unknown as ipc.SetupForm;
      return personForm("box", "Add a box") as unknown as ipc.SetupForm;
    });
  });

  it("walks one step per screen and runs the flow with the inputs", async () => {
    const user = userEvent.setup();
    vi.mocked(ipc.setupRun).mockResolvedValue(outcome({}));
    open("agent");
    expect(await screen.findByText("Add an agent")).toBeInTheDocument();
    // The preflight passed: the first screen is the harness, Next waits for it.
    const next = screen.getByRole("button", { name: "Next" });
    expect(next).toBeDisabled();
    await user.click(screen.getByRole("radio", { name: /Claude Code/ }));
    await user.click(next);
    await user.type(await screen.findByLabelText("Name"), "builder");
    await user.click(screen.getByRole("button", { name: "Next" }));
    // The model list follows the chosen harness.
    await waitFor(() => expect(ipc.setupForm).toHaveBeenCalledWith("agent", expect.objectContaining({ harness: "claude-code" })));
    await user.click(screen.getByRole("button", { name: "Set up" }));
    await waitFor(() => expect(ipc.setupRun).toHaveBeenCalledWith("agent", { harness: "claude-code", name: "builder" }, {}));
    expect(await screen.findByText("Ready.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Done" })).toBeInTheDocument();
  });

  it("sends a secret once, apart from the inputs, and forgets it", async () => {
    const user = userEvent.setup();
    vi.mocked(ipc.setupRun).mockResolvedValue(outcome({ exitCode: 1, ok: false, done: false, checks: [{ id: "tool.handshake", title: "handshake", status: "fail", detail: "401", fix: "check the token" }] }));
    open("tool");
    await user.type(await screen.findByLabelText("Name"), "linear");
    await user.type(screen.getByLabelText("URL"), "https://mcp.example.com");
    await user.click(screen.getByRole("button", { name: "Next" }));
    const token = await screen.findByLabelText(/Bearer token/);
    expect(token).toHaveAttribute("type", "password");
    await user.type(token, "s3cret-canary");
    await user.click(screen.getByRole("button", { name: "Set up" }));
    await waitFor(() =>
      expect(ipc.setupRun).toHaveBeenCalledWith("tool", { id: "linear", kind: "url", url: "https://mcp.example.com" }, { token: "s3cret-canary" }),
    );
    // The failure's fix is right under it.
    expect(await screen.findByText("check the token")).toBeInTheDocument();
    expect(document.body.innerHTML).not.toContain("s3cret-canary");
    // Back to the token: the field starts empty again.
    await user.click(screen.getByRole("button", { name: "Back" }));
    expect((screen.getByLabelText(/Bearer token/) as HTMLInputElement).value).toBe("");
  });

  it("hands a box's approval to Admit a box", async () => {
    const user = userEvent.setup();
    vi.mocked(ipc.setupRun).mockResolvedValue(
      outcome({ exitCode: 4, done: false, flow: "box", checks: [{ id: "box.admit", title: "admit", status: "warn", detail: "on the box, run ligus setup box", fix: "open Admit a box" }] }),
    );
    const onAdmit = vi.fn();
    window.addEventListener(ADMIT_BOX_EVENT, onAdmit);
    const onClose = open("box");
    await user.click(await screen.findByRole("button", { name: "Continue" }));
    await user.click(await screen.findByRole("button", { name: "Open Admit a box" }));
    window.removeEventListener(ADMIT_BOX_EVENT, onAdmit);
    expect(onAdmit).toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
  });

  it("creates an invite only when the person asks, in this window", async () => {
    const user = userEvent.setup();
    vi.mocked(ipc.setupRun).mockResolvedValue(
      outcome({ exitCode: 4, done: false, flow: "invite", checks: [{ id: "invite.create", title: "invite", status: "warn", detail: "the code is shown once", fix: "create the invite here" }] }),
    );
    open("invite");
    await user.click(await screen.findByRole("button", { name: "Continue" }));
    expect(ipc.workspaceInvite).not.toHaveBeenCalled();
    await user.click(await screen.findByRole("button", { name: "Create invite" }));
    expect(ipc.workspaceInvite).toHaveBeenCalledWith("w1");
    expect(((await screen.findByLabelText("Invite code")) as HTMLTextAreaElement).value).toBe("hivews1:demo-invite");
  });

  it("a blocked preflight shows the fix and offers no way on", async () => {
    vi.mocked(ipc.setupForm).mockResolvedValue({
      ...(personForm("invite", "Invite a person") as unknown as ipc.SetupForm),
      blocked: true,
      preflight: [{ id: "invite.role", title: "role", status: "fail", detail: "needs Admin or above (you are Contributor)", fix: "ask an Admin of the workspace" }],
    });
    open("invite");
    expect(await screen.findByText("ask an Admin of the workspace")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Continue" })).not.toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Close" }).length).toBeGreaterThan(0);
    expect(ipc.setupRun).not.toHaveBeenCalled();
  });
});
