// Change model… and Runs on… from an agent's ⋯ menu: the Add an agent
// form with `agent` set (workspace model v3, 17.4). Only the field being
// changed shows; the engine says what it asks for.
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { AddAgentSheet } from "./AddAgentSheet";
import * as ipc from "@/lib/ipc";
import type { SetupField, SetupForm, SetupOutcome } from "@/lib/ipc";

function changeForm(inputs: Record<string, string>): SetupForm {
  const f = (x: Partial<SetupField> & { id: string; label: string; type: SetupField["type"] }): SetupField => ({ required: false, ...x });
  const model = inputs.model ?? "qwen3-coder";
  const runsOn = inputs["runs-on"] ?? "box-1-id";
  const moved = runsOn !== "box-1-id";
  return {
    flow: { id: "agent", title: "Add an agent", summary: "", why: "", keywords: "", minRole: "Contributor", personSteps: false },
    preflight: [],
    blocked: false,
    action: moved ? "Move @coder" : model !== "qwen3-coder" ? "Change model" : "Save",
    // Rules 4: a model change within one source needs no passkey; a move does.
    confirmations: moved ? 1 : 0,
    steps: [
      { id: "agent", title: "Agent", kind: "input", fields: [f({ id: "agent", label: "Agent", type: "text", required: true, default: "a1" })] },
      {
        id: "model",
        title: "Model",
        kind: "input",
        fields: [
          f({
            id: "model",
            label: "Model",
            type: "suggest",
            required: true,
            default: "qwen3-coder",
            options: [
              { value: "auto", label: "auto (the gateway picks)" },
              { value: "qwen3-coder", label: "qwen3-coder" },
              { value: "glm-4.6", label: "glm-4.6" },
            ],
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
            default: "box-1-id",
            note: moved ? "Turns run on box-2. When it's offline, @coder waits." : "Turns run on box-1. When it's offline, @coder waits.",
            options: [
              { value: "box-1-id", label: "box-1", hint: "ready" },
              { value: "box-2-id", label: "box-2", hint: "ready" },
              { value: "here", label: "Your devices", hint: "gw-home runs only on a host that offers it", disabled: true },
            ],
          }),
        ],
      },
      { id: "verify", title: "Saved", kind: "verify" },
    ],
  };
}

const done = (detail: string): SetupOutcome => ({
  schema: "ligus.checks.v1",
  command: "setup agent",
  ok: true,
  exitCode: 0,
  checks: [
    { id: "agent.agent", title: "agent", status: "ok", detail: "@coder · gw-home (OpenAI API)" },
    { id: "agent.change", title: "change", status: "ok", detail },
  ],
  next: [],
  flow: "agent",
  done: true,
  dryRun: false,
  results: { agentId: "a1" },
});

vi.mock("@/lib/ipc", async (orig) => ({
  ...(await orig<typeof import("@/lib/ipc")>()),
  setupForm: vi.fn(async (_flow: string, inputs: Record<string, string>) => changeForm(inputs)),
  setupRun: vi.fn(async (_flow: string, inputs: Record<string, string>) =>
    done(inputs["runs-on"] === "box-2-id" ? "@coder now uses qwen3-coder on box-2" : `@coder now uses ${inputs.model} on box-1`),
  ),
  setupCancel: vi.fn(async () => {}),
  onSetupConfirming: vi.fn(async () => () => {}),
}));

function renderSheet(change: "model" | "runs-on") {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const onDone = vi.fn();
  render(
    <QueryClientProvider client={qc}>
      <AddAgentSheet agent={{ id: "a1", name: "coder" }} change={change} onClose={() => {}} onDone={onDone} />
    </QueryClientProvider>,
  );
  return { onDone };
}

describe("Change model… and Runs on…", () => {
  beforeEach(() => vi.clearAllMocks());

  it("Change model shows only the model, from the agent's own source, and needs no passkey", async () => {
    const { onDone } = renderSheet("model");
    expect(await screen.findByText("Change model for @coder")).toBeInTheDocument();
    await waitFor(() => expect(ipc.setupForm).toHaveBeenCalledWith("agent", expect.objectContaining({ agent: "a1" })));
    // Only the model: no name, no source, no Runs on.
    expect(screen.queryByText("What answers")).toBeNull();
    expect(screen.queryByText("Runs on")).toBeNull();
    const input = await screen.findByLabelText(/Model/);
    await userEvent.clear(input);
    await userEvent.type(input, "glm-4.6");
    const button = await screen.findByRole("button", { name: "Change model" });
    expect(screen.queryByTestId("change-agent-passkey")).toBeNull();
    await userEvent.click(button);
    await waitFor(() => expect(ipc.setupRun).toHaveBeenCalled());
    const [flow, sent] = vi.mocked(ipc.setupRun).mock.calls[0];
    expect(flow).toBe("agent");
    expect(sent).toMatchObject({ agent: "a1", model: "glm-4.6" });
    expect(await screen.findByText("@coder now uses glm-4.6 on box-1")).toBeInTheDocument();
    expect(onDone).toHaveBeenCalled();
  });

  it("Runs on lists the hosts that can run it, with the one line, and says a move takes a passkey", async () => {
    renderSheet("runs-on");
    expect(await screen.findByText("Where @coder runs")).toBeInTheDocument();
    expect(await screen.findByText("Turns run on box-1. When it's offline, @coder waits.")).toBeInTheDocument();
    // A host that can't run it isn't offered.
    expect(screen.queryByText("Your devices")).toBeNull();
    await userEvent.click(await screen.findByText("box-2"));
    expect(await screen.findByRole("button", { name: "Move @coder" })).toBeInTheDocument();
    expect(await screen.findByTestId("change-agent-passkey")).toHaveTextContent("Asks for your passkey once.");
    await userEvent.click(screen.getByRole("button", { name: "Move @coder" }));
    await waitFor(() => expect(ipc.setupRun).toHaveBeenCalled());
    expect(vi.mocked(ipc.setupRun).mock.calls[0][1]).toMatchObject({ agent: "a1", "runs-on": "box-2-id" });
    expect(await screen.findByText("@coder now uses qwen3-coder on box-2")).toBeInTheDocument();
  });
});
