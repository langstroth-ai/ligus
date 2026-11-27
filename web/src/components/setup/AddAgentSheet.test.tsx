import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { AddAgentSheet } from "./AddAgentSheet";
import * as ipc from "@/lib/ipc";
import type { SetupConfirming, SetupField, SetupForm, SetupOutcome } from "@/lib/ipc";

let confirmingCb: ((e: SetupConfirming) => void) | null = null;

/// The engine's agent form, shaped like setup_form's JSON for these inputs.
function agentForm(inputs: Record<string, string>): SetupForm {
  const source = inputs.source ?? "claude-code";
  const gateway = source === "gateway:gw-home";
  const runsOn = inputs["runs-on"] ?? (gateway ? "box-1-id" : "here");
  const name = inputs.name ?? "";
  const who = name ? `@${name}` : "the agent";
  const f = (x: Partial<SetupField> & { id: string; label: string; type: SetupField["type"] }): SetupField => ({ required: false, ...x });
  return {
    flow: { id: "agent", title: "Add an agent", summary: "", why: "An agent answers when you @mention it.", keywords: "", minRole: "Contributor", personSteps: false },
    preflight: [{ id: "agent.sandbox", title: "agent sandbox", status: "ok", detail: "seatbelt" }],
    blocked: false,
    action: name ? `Add @${name}` : "Add the agent",
    confirmations: gateway ? 2 : 1,
    steps: [
      { id: "name", title: "Name", kind: "input", fields: [f({ id: "name", label: "Name", type: "text", required: true, help: "You @mention it by this name." })] },
      {
        id: "source",
        title: "What answers",
        kind: "input",
        fields: [
          f({
            id: "source",
            label: "What answers",
            type: "choice",
            required: true,
            default: "claude-code",
            options: [
              { value: "claude-code", label: "Claude Code", hint: "signed in · this Mac" },
              { value: "gateway:gw-home", label: "gw-home (OpenAI API)", hint: "offered by box-1" },
              { value: "key:anthropic", label: "Anthropic API key", hint: "the key stays on this Mac" },
            ],
          }),
          f({ id: "endpoint", label: "Address", type: "text", default: "http://localhost:11434", when: ["source", ["local", "key:openai"]] }),
          f({ id: "api-key", label: "API key", type: "secret", required: true, when: ["source", ["key:anthropic", "key:openai"]] }),
        ],
      },
      {
        id: "model",
        title: "Model",
        kind: "input",
        fields: [
          f({
            id: "model",
            label: "Model",
            type: "suggest",
            required: gateway,
            default: gateway ? "auto" : "",
            options: gateway ? [{ value: "auto", label: "auto" }, { value: "qwen3-coder", label: "qwen3-coder" }] : [{ value: "sonnet", label: "sonnet" }],
          }),
        ],
      },
      {
        id: "runs-on",
        title: "Runs on",
        kind: "input",
        fields: [
          f({
            id: "runs-on",
            label: "Runs on",
            type: "choice",
            required: true,
            default: runsOn,
            note: runsOn === "here" ? "Turns run on this device while Ligus is open." : `Turns run on box-1. When it's offline, ${who} waits.`,
            options: gateway
              ? [
                  { value: "box-1-id", label: "box-1", hint: "ready" },
                  { value: "here", label: "This device", hint: "gw-home runs only on a host that offers it", disabled: true },
                  { value: "box-2-id", label: "box-2", hint: "doesn't offer gw-home", disabled: true },
                ]
              : [{ value: "here", label: "This device", hint: "this Mac · signed in" }],
          }),
        ],
      },
      { id: "test", title: "Test", kind: "input", fields: [f({ id: "test-turn", label: "Run a test turn", type: "toggle", default: "true" })] },
      { id: "verify", title: "Ready", kind: "verify" },
    ],
  };
}

const ok = (results: Record<string, string> = {}): SetupOutcome => ({
  schema: "ligus.checks.v1",
  command: "setup agent",
  ok: true,
  exitCode: 0,
  checks: [{ id: "agent.agent", title: "agent", status: "ok", detail: "@coder created, runs on box-1" }],
  next: [],
  flow: "agent",
  done: true,
  dryRun: false,
  results,
});

vi.mock("@/lib/ipc", async (orig) => ({
  ...(await orig<typeof import("@/lib/ipc")>()),
  setupForm: vi.fn(async (_flow: string, inputs: Record<string, string>) => agentForm(inputs)),
  setupRun: vi.fn(async () => ok({ agentId: "a1", runtimeId: "gw-home-auto" })),
  setupCancel: vi.fn(async () => {}),
  onSetupConfirming: vi.fn(async (cb: (e: SetupConfirming) => void) => {
    confirmingCb = cb;
    return () => {};
  }),
}));

function renderSheet(props: Partial<Parameters<typeof AddAgentSheet>[0]> = {}) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const onClose = vi.fn();
  const onDone = vi.fn();
  render(
    <QueryClientProvider client={qc}>
      <AddAgentSheet onClose={onClose} onDone={onDone} {...props} />
    </QueryClientProvider>,
  );
  return { onClose, onDone };
}

const lastFormInputs = () => {
  const calls = (ipc.setupForm as ReturnType<typeof vi.fn>).mock.calls;
  return calls[calls.length - 1][1] as Record<string, string>;
};

beforeEach(() => {
  vi.clearAllMocks();
  confirmingCb = null;
});

describe("Add an agent", () => {
  it("starts from the best source, where it runs, and the line under Runs on", async () => {
    renderSheet();
    expect(await screen.findByRole("radio", { name: /Claude Code/ })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByRole("radio", { name: /This device/ })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByTestId("note-runs-on")).toHaveTextContent("Turns run on this device while Ligus is open.");
    expect(screen.getByRole("button", { name: "Add the agent" })).toBeDisabled();
    // No address or key for a CLI.
    expect(screen.queryByLabelText(/API key/)).toBeNull();
  });

  it("a gateway: the form is asked again, it runs on the one host offering it, hosts that don't are hidden", async () => {
    renderSheet();
    await userEvent.type(await screen.findByRole("textbox", { name: "Name" }), "coder");
    await userEvent.click(screen.getByRole("radio", { name: /gw-home/ }));
    await waitFor(() => expect(lastFormInputs().source).toBe("gateway:gw-home"));
    expect(await screen.findByRole("radio", { name: /^box-1/ })).toHaveAttribute("aria-checked", "true");
    expect(screen.queryByRole("radio", { name: /^box-2/ })).toBeNull();
    expect(screen.queryByRole("radio", { name: /This device/ })).toBeNull();
    await waitFor(() => expect(screen.getByTestId("note-runs-on")).toHaveTextContent("Turns run on box-1. When it's offline, @coder waits."));
    expect(screen.getByTestId("add-agent-confirmations")).toHaveTextContent("twice");
    const add = await screen.findByRole("button", { name: "Add @coder" });
    let finish: (o: SetupOutcome) => void = () => {};
    (ipc.setupRun as ReturnType<typeof vi.fn>).mockImplementationOnce(() => new Promise<SetupOutcome>((r) => (finish = r)));
    await userEvent.click(add);
    act(() => confirmingCb?.({ step: 1, of: 2, what: "Define gw-home (OpenAI API) for @coder" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Confirm 1 of 2: Define gw-home (OpenAI API) for @coder");
    act(() => confirmingCb?.({ step: 2, of: 2, what: "Add @coder" }));
    expect(screen.getByRole("status")).toHaveTextContent("Confirm 2 of 2: Add @coder");
    await act(async () => finish(ok({ agentId: "a1", runtimeId: "gw-home-auto" })));
    const [, inputs, secrets] = (ipc.setupRun as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(inputs).toMatchObject({ name: "coder", source: "gateway:gw-home", "runs-on": "box-1-id", model: "auto" });
    expect(secrets).toEqual({});
    expect(await screen.findByText("@coder created, runs on box-1")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Done" })).toBeInTheDocument();
  });

  it("an API key goes in secrets, never in the inputs", async () => {
    renderSheet();
    await userEvent.type(await screen.findByRole("textbox", { name: "Name" }), "claude");
    await userEvent.click(screen.getByRole("radio", { name: /Anthropic API key/ }));
    const key = await screen.findByLabelText(/API key/);
    await userEvent.type(key, "sk-ant-canary");
    await userEvent.click(await screen.findByRole("button", { name: "Add @claude" }));
    await waitFor(() => expect(ipc.setupRun).toHaveBeenCalled());
    const [, inputs, secrets] = (ipc.setupRun as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(JSON.stringify(inputs)).not.toContain("sk-ant-canary");
    expect(secrets).toEqual({ "api-key": "sk-ant-canary" });
    for (const call of (ipc.setupForm as ReturnType<typeof vi.fn>).mock.calls) expect(JSON.stringify(call)).not.toContain("sk-ant-canary");
  });

  it("a failed run says what failed and how to fix it", async () => {
    (ipc.setupRun as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      ...ok(),
      ok: false,
      exitCode: 3,
      done: false,
      checks: [{ id: "agent.harness.signed-in", title: "sign-in", status: "fail", detail: "Claude Code isn't signed in on this device", fix: "run `claude` in a terminal on this device, then run this again" }],
    });
    const { onDone } = renderSheet({ initialName: "coder" });
    await userEvent.click(await screen.findByRole("button", { name: "Add @coder" }));
    const failed = await screen.findByTestId("add-agent-failed");
    expect(failed).toHaveTextContent("Claude Code isn't signed in on this device");
    expect(failed).toHaveTextContent("run `claude` in a terminal");
    expect(onDone).not.toHaveBeenCalled();
  });

  it("renders in place for the first run and reports what it added", async () => {
    const { onDone } = renderSheet({ inline: true, initialName: "coder" });
    expect(await screen.findByTestId("add-agent-inline")).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).toBeNull();
    await userEvent.click(await screen.findByRole("button", { name: "Add @coder" }));
    await waitFor(() => expect(onDone).toHaveBeenCalled());
    const [outcome, inputs] = onDone.mock.calls[0];
    expect(outcome.results.runtimeId).toBe("gw-home-auto");
    expect(inputs["runs-on"]).toBe("here");
    expect(inputs["test-turn"]).toBe("true");
  });
});
