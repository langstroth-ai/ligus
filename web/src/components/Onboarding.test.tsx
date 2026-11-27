import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Onboarding } from "./Onboarding";
import * as ipc from "@/lib/ipc";

// Mock the IPC boundary — this is what makes a full UI flow testable headlessly.
vi.mock("@/lib/ipc", () => ({
  setDisplayName: vi.fn(async () => {}),
  setGitEmail: vi.fn(async () => {}),
  pickWorkspaceFolder: vi.fn(async () => "/Users/sam/proj"),
  addWorkspaceToList: vi.fn(async () => ["/Users/sam/proj"]),
  setWorkspaceRoot: vi.fn(async () => {}),
  addRuntime: vi.fn(async () => {}),
  probeProvider: vi.fn(async () => ({ ok: true, latency_ms: 5, reply: "PINGOK", error: null })),
  // The claude-code path persists the chosen --model; missing this mock made
  // applyRuntime() throw (undefined is not a function), silently stranding
  // the wizard on step 3 — the long-standing "pre-existing failure".
  setClaudeCodeModel: vi.fn(async () => {}),
  setDefaultModel: vi.fn(async () => {}),
  listClaudeCodeModels: vi.fn(async () => [
    { value: "", label: "Default", description: null },
    { value: "sonnet", label: "Sonnet", description: null },
    { value: "opus", label: "Opus", description: null },
    { value: "haiku", label: "Haiku", description: null },
    { value: "claude-fable-5[1m]", label: "Fable", description: "Fable 5" },
  ]),
  setAvatar: vi.fn(async () => {}),
  openExternal: vi.fn(async () => {}),
  updateConnectionSettings: vi.fn(async () => ({
    relayUrl: "",
    room: "default",
    hasWorkspaceKey: false,
    hasApiKey: false,
    permissionMode: "acceptEdits",
  })),
  getConnectionSettings: vi.fn(async () => ({
    relayUrl: "",
    room: "default",
    hasWorkspaceKey: false,
    hasApiKey: false,
    permissionMode: "default",
  })),
  detectEnvironment: vi.fn(async () => ({
    claudeCode: true,
    ollama: false,
    anthropicEnv: false,
    openaiEnv: false,
    gitName: "Sam Dev",
    gitEmail: "sam@example.com",
  })),
  myProfile: vi.fn(async () => ({
    accountId: "6f1c2d3e-0000-4000-8000-000000000001",
    displayName: "You",
    avatarUrl: null,
    safetyCode: "0011 2233 4455 6677 8899 aabb ccdd eeff",
    gitEmail: "",
  })),
  joinWorkspace: vi.fn(async () => ({ id: "w", name: "Acme", kind: "room", active: true })),
  redeemShortCode: vi.fn(async () => ({ kind: "workspace", label: "Acme" })),
  probeRelay: vi.fn(async () => ({ status: "ok", detail: "" })),
  probeRelayAt: vi.fn(async () => ({ status: "ok", detail: "" })),
  listOllamaModels: vi.fn(async () => ({ base: "http://localhost:11434", host: "localhost", models: [], pulling: [] })),
  setFirstRunPath: vi.fn(async () => {}),
  firstRunWorkspace: vi.fn(async (name: string) => ({ id: "w1", name, kind: "room", active: true, iconUrl: null })),
  passkeyRegister: vi.fn(async () => {}),
  recoveryKeyCreate: vi.fn(async () => "alpha bravo charlie"),
  securityStatus: vi.fn(async () => ({ passkeys: [], rules: 3, keyVersion: 0, secretBackend: "file", deviceConfirmed: true })),
  linkRequestStatus: vi.fn(async () => ({ phase: "idle" })),
  onLinkFlowChanged: vi.fn(async (cb: (v: unknown) => void) => {
    linkPush = cb;
    return () => {};
  }),
}));

// The link screen's backend events; tests push them by hand.
let linkPush: (v: unknown) => void = () => {};

beforeEach(() => vi.clearAllMocks());

const clickByName = (name: string | RegExp) =>
  userEvent.click(screen.getByRole("button", { name }));

/// The question, then You (the name prefilled from git).
async function toYou() {
  await clickByName("Yes, start here");
  await screen.findByPlaceholderText("Your name");
}

/// The question, You and Secure, to the agent screen.
async function toAgent() {
  await toYou();
  await waitFor(() => expect((screen.getByPlaceholderText("Your name") as HTMLInputElement).value).toBe("Sam Dev"));
  await clickByName("Next");
  await screen.findByTestId("first-run-secure");
  await clickByName("Create a passkey");
  await screen.findByTestId("first-run-recovery-words");
  await clickByName("I wrote them down");
  await screen.findByText("Choose your agent");
}

describe("Onboarding wizard flow", () => {
  it("asks the question first, then walks You → Secure → agent and finishes with accept-edits", async () => {
    const onComplete = vi.fn();
    render(<Onboarding onComplete={onComplete} />);

    // The question comes before a name.
    await screen.findByText("Is this your first device with Ligus?");
    expect(screen.queryByPlaceholderText("Your name")).toBeNull();
    await clickByName("Yes, start here");
    expect(ipc.setFirstRunPath).toHaveBeenCalledWith("first");

    // You: the name is prefilled from git config.
    const nameInput = await screen.findByPlaceholderText("Your name");
    await waitFor(() => expect((nameInput as HTMLInputElement).value).toBe("Sam Dev"));
    await clickByName("Next");
    expect(ipc.setDisplayName).toHaveBeenCalledWith("Sam Dev");
    expect(ipc.setGitEmail).toHaveBeenCalledWith("sam@example.com");
    // The person's first workspace is made for them.
    await waitFor(() => expect(ipc.firstRunWorkspace).toHaveBeenCalledWith("Sam's workspace"));

    // Secure: one button makes the passkey, then the recovery key.
    await screen.findByTestId("first-run-secure");
    await clickByName("Create a passkey");
    expect(await screen.findByTestId("first-run-recovery-words")).toHaveTextContent("alpha bravo charlie");
    expect(ipc.passkeyRegister).toHaveBeenCalledOnce();
    expect(ipc.recoveryKeyCreate).toHaveBeenCalledOnce();
    await clickByName("I wrote them down");

    // The agent: Claude Code is detected + preselected.
    await screen.findByText("Choose your agent");
    await clickByName("Finish");
    // The claude-code path persists the --model choice ("" = CLI default)…
    await waitFor(() => expect(ipc.setClaudeCodeModel).toHaveBeenCalledWith(""));
    // …and syncs the Primary Runtime's default model so the chat header matches
    // the pick (CLI-default → Sonnet).
    await waitFor(() => expect(ipc.setDefaultModel).toHaveBeenCalledWith("claude-sonnet-4-6"));
    // …and default permission = runCommands ("edit files and run commands" on).
    await waitFor(() =>
      expect(ipc.updateConnectionSettings).toHaveBeenCalledWith(
        expect.objectContaining({ permissionMode: "runCommands", apiKey: null }),
      ),
    );
    await waitFor(() => expect(onComplete).toHaveBeenCalled());
    // No folder, relay, team or theme is asked, and nothing is joined.
    expect(screen.queryByText("Open your project")).toBeNull();
    expect(screen.queryByText("Team & sync")).toBeNull();
    expect(screen.queryByText("Make it yours")).toBeNull();
    expect(ipc.joinWorkspace).not.toHaveBeenCalled();
  });

  it("I already use Ligus: links this device, and asks no name and makes no workspace", async () => {
    const onComplete = vi.fn();
    render(<Onboarding onComplete={onComplete} />);
    await clickByName("I already use Ligus on another device");
    expect(ipc.setFirstRunPath).toHaveBeenCalledWith("link");
    await screen.findByTestId("link-this-device");
    expect(screen.queryByPlaceholderText("Your name")).toBeNull();
    // Back returns to the question; nothing was made or joined.
    await clickByName("Back");
    await screen.findByText("Is this your first device with Ligus?");
    for (const f of [ipc.setDisplayName, ipc.firstRunWorkspace, ipc.passkeyRegister, ipc.joinWorkspace, ipc.redeemShortCode, ipc.addRuntime])
      expect(f).not.toHaveBeenCalled();
    expect(onComplete).not.toHaveBeenCalled();
  });

  it("a linked device ends the first run, saying where to add a passkey when the account has none", async () => {
    const onComplete = vi.fn();
    render(<Onboarding onComplete={onComplete} />);
    await clickByName("I already use Ligus on another device");
    await screen.findByTestId("link-this-device");
    act(() => linkPush({ phase: "linked", personName: "Ana", workspaceId: "w1" }));
    expect(await screen.findByTestId("link-linked")).toHaveTextContent("one of Ana's devices");
    await clickByName("Continue");
    // The account has no passkey yet (securityStatus): one calm note, then done.
    await screen.findByTestId("first-run-linked-secure");
    expect(onComplete).not.toHaveBeenCalled();
    await clickByName("Done");
    await waitFor(() => expect(onComplete).toHaveBeenCalled());
    // Nothing of a first device's run happened.
    for (const f of [ipc.setDisplayName, ipc.firstRunWorkspace, ipc.passkeyRegister, ipc.addRuntime]) expect(f).not.toHaveBeenCalled();
  });

  it("a linked device whose account has a passkey goes straight on", async () => {
    const onComplete = vi.fn();
    (ipc.securityStatus as ReturnType<typeof vi.fn>).mockResolvedValueOnce({ passkeys: [{ credentialId: "pk", recovery: false }], rules: 3 });
    render(<Onboarding onComplete={onComplete} />);
    await clickByName("I already use Ligus on another device");
    await screen.findByTestId("link-this-device");
    act(() => linkPush({ phase: "linked", personName: "Ana", workspaceId: "w1" }));
    await screen.findByTestId("link-linked");
    await clickByName("Continue");
    await waitFor(() => expect(onComplete).toHaveBeenCalled());
    expect(screen.queryByTestId("first-run-linked-secure")).toBeNull();
  });

  it("Secure can't be skipped half-done: Finish later asks first", async () => {
    render(<Onboarding onComplete={vi.fn()} />);
    await toYou();
    await clickByName("Next");
    await screen.findByTestId("first-run-secure");
    // No Back once the workspace is made.
    expect(screen.queryByRole("button", { name: "← Back" })).toBeNull();
    await clickByName("Finish later");
    await screen.findByTestId("first-run-finish-later");
    expect(screen.getByText("Finish later?")).toBeInTheDocument();
    await clickByName("Keep going");
    await screen.findByTestId("first-run-secure");
    await clickByName("Finish later");
    await clickByName("Finish later");
    await screen.findByText("Choose your agent");
    expect(ipc.passkeyRegister).not.toHaveBeenCalled();
  });

  it("a failed passkey stays on Secure, and the retry makes only what's missing", async () => {
    (ipc.recoveryKeyCreate as ReturnType<typeof vi.fn>).mockRejectedValueOnce(new Error("dismissed"));
    render(<Onboarding onComplete={vi.fn()} />);
    await toYou();
    await clickByName("Next");
    await clickByName("Create a passkey");
    await screen.findByText(/dismissed/);
    expect(screen.getByTestId("first-run-secure")).toBeInTheDocument();
    await clickByName("Create a recovery key");
    await screen.findByTestId("first-run-recovery-words");
    expect(ipc.passkeyRegister).toHaveBeenCalledOnce();
    expect(ipc.recoveryKeyCreate).toHaveBeenCalledTimes(2);
  });

  it("a first run started again after Secure goes straight to the agent", async () => {
    (ipc.securityStatus as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      passkeys: [
        { credentialId: "a", recovery: false },
        { credentialId: "r", recovery: true },
      ],
      rules: 3,
      keyVersion: 0,
      secretBackend: "file",
      deviceConfirmed: true,
    });
    render(<Onboarding onComplete={vi.fn()} />);
    await toYou();
    await clickByName("Next");
    await screen.findByText("Choose your agent");
    expect(ipc.passkeyRegister).not.toHaveBeenCalled();
  });

  it("picks Claude Code + Opus and makes Opus the Primary Runtime default", async () => {
    render(<Onboarding onComplete={vi.fn()} />);

    await toAgent();
    // Claude Code is preselected; choose Opus in its model dropdown.
    await userEvent.selectOptions(
      screen.getByLabelText("Claude Code model"),
      "opus",
    );
    await clickByName("Finish");

    // The CLI --model flag AND the displayed/default Primary Runtime both move to
    // Opus — otherwise the chat header silently falls back to Sonnet.
    await waitFor(() => expect(ipc.setClaudeCodeModel).toHaveBeenCalledWith("opus"));
    await waitFor(() =>
      expect(ipc.setDefaultModel).toHaveBeenCalledWith("claude-opus-4-8"),
    );
  });

  it("surfaces a cached Claude Code model (Fable) and wires the default from it", async () => {
    render(<Onboarding onComplete={vi.fn()} />);

    await toAgent();
    // The dropdown is populated from listClaudeCodeModels — Fable (cached,
    // account-specific) is present even though it isn't hardcoded.
    const select = screen.getByLabelText("Claude Code model");
    await waitFor(() =>
      expect(
        Array.from((select as HTMLSelectElement).options).map((o) => o.value),
      ).toContain("claude-fable-5[1m]"),
    );
    await userEvent.selectOptions(select, "claude-fable-5[1m]");
    await clickByName("Finish");

    await waitFor(() =>
      expect(ipc.setClaudeCodeModel).toHaveBeenCalledWith("claude-fable-5[1m]"),
    );
    // The Primary Runtime default uses the full id with the [1m] suffix stripped.
    await waitFor(() =>
      expect(ipc.setDefaultModel).toHaveBeenCalledWith("claude-fable-5"),
    );
  });

  it("adds an OpenAI-compatible runtime with key + base URL", async () => {
    const onComplete = vi.fn();
    render(<Onboarding onComplete={onComplete} />);

    await toAgent();
    await clickByName(/OpenAI-compatible API/);
    await userEvent.type(screen.getByPlaceholderText(/API key/), "sk-test-123");
    await clickByName("Finish");

    await waitFor(() =>
      expect(ipc.addRuntime).toHaveBeenCalledWith(
        expect.any(String),
        "OpenAI-compatible",
        "openAI",
        "remote",
        "https://api.openai.com/v1/chat/completions",
        "gpt-4o",
        true,
        false,
      ),
    );
    expect(ipc.updateConnectionSettings).toHaveBeenCalledWith(
      expect.objectContaining({ apiKey: "sk-test-123", permissionMode: "runCommands" }),
    );
  });

  it("gives the Ollama runtime an installed model, never gpt-4o", async () => {
    (ipc.detectEnvironment as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      claudeCode: false,
      ollama: true,
      anthropicEnv: false,
      openaiEnv: false,
      gitName: "Sam Dev",
      gitEmail: "sam@example.com",
    });
    (ipc.listOllamaModels as ReturnType<typeof vi.fn>).mockResolvedValue({
      base: "http://localhost:11434",
      host: "localhost",
      models: [{ name: "qwen2.5-coder:7b" }],
      pulling: [],
    });
    render(<Onboarding onComplete={vi.fn()} />);
    await toAgent();
    const field = (await screen.findByLabelText("Ollama model")) as HTMLInputElement;
    await waitFor(() => expect(field.value).toBe("qwen2.5-coder:7b"));
    await clickByName("Finish");
    await waitFor(() =>
      expect(ipc.addRuntime).toHaveBeenCalledWith(
        "ollama-local",
        "Ollama (local)",
        "ollama",
        "local",
        "http://localhost:11434",
        "qwen2.5-coder:7b",
        true,
        false,
      ),
    );
  });

  it("says Finish, the button's own name, when a probe fails", async () => {
    (ipc.probeProvider as ReturnType<typeof vi.fn>).mockResolvedValueOnce({ ok: false, latency_ms: 0, reply: "", error: "401" });
    render(<Onboarding onComplete={vi.fn()} />);
    await toAgent();
    await clickByName(/Anthropic API key/);
    await userEvent.type(screen.getByPlaceholderText(/Anthropic API key/), "sk-ant-bad");
    await clickByName("Finish");
    await screen.findByText(/press Finish again/);
  });

  it("holds on a failed agent probe, then proceeds on a second Finish", async () => {
    // A bad key: the probe fails, so the first Next must NOT advance/configure.
    (ipc.probeProvider as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      ok: false,
      latency_ms: 0,
      reply: "",
      error: "401 Unauthorized",
    });
    render(<Onboarding onComplete={vi.fn()} />);

    await toAgent();
    await clickByName(/Anthropic API key/);
    await userEvent.type(screen.getByPlaceholderText(/Anthropic API key/), "sk-ant-bad");
    await clickByName("Finish"); // probes → fails → holds

    // The failure is surfaced and no runtime was configured yet.
    await screen.findByText(/won't answer until it works/);
    expect(ipc.addRuntime).not.toHaveBeenCalled();

    // A second Continue proceeds anyway (transient/edge escape hatch).
    await clickByName("Finish");
    await waitFor(() => expect(ipc.updateConnectionSettings).toHaveBeenCalled());
  });

  it("onboards without any provider: a name, an optional picture and the safety code", async () => {
    (ipc.myProfile as ReturnType<typeof vi.fn>).mockResolvedValue({
      accountId: "6f1c2d3e-0000-4000-8000-000000000001",
      displayName: "You",
      avatarUrl: null,
      safetyCode: "0011 2233 4455 6677 8899 aabb ccdd eeff",
      gitEmail: "",
    });
    render(<Onboarding onComplete={vi.fn()} />);
    await toYou();
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "About your account on this device" }).getAttribute("data-tip")).toContain(
        "0011 2233 4455 6677 8899 aabb ccdd eeff",
      ),
    );
    expect(screen.queryByText(/GitHub/)).toBeNull();
    fireEvent.change(screen.getByPlaceholderText("Your name"), { target: { value: "Ana" } });
    await clickByName("Next");
    await waitFor(() => expect(ipc.setDisplayName).toHaveBeenCalledWith("Ana"));
  });
});
