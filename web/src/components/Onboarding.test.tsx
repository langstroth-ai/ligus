import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Onboarding } from "./Onboarding";
import * as ipc from "@/lib/ipc";
import { COMMUNITY_RELAY_URL } from "@/lib/communityRelay";

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
}));

beforeEach(() => vi.clearAllMocks());

const clickByName = (name: string | RegExp) =>
  userEvent.click(screen.getByRole("button", { name }));

describe("Onboarding wizard flow", () => {
  it("walks identity → project → agent → team and finishes with accept-edits", async () => {
    const onComplete = vi.fn();
    render(<Onboarding onComplete={onComplete} />);

    // Step 1: name is prefilled from git config.
    const nameInput = await screen.findByPlaceholderText("Your name");
    await waitFor(() => expect((nameInput as HTMLInputElement).value).toBe("Sam Dev"));
    await clickByName("Next");
    expect(ipc.setDisplayName).toHaveBeenCalledWith("Sam Dev");
    expect(ipc.setGitEmail).toHaveBeenCalledWith("sam@example.com");

    // Step 2: skip choosing a folder.
    await screen.findByText("Open your project");
    await clickByName("Next");

    // Step 3: Claude Code is detected + preselected; finish the step.
    await screen.findByText("Choose your agent");
    await clickByName("Next");
    // The claude-code path persists the --model choice ("" = CLI default)…
    await waitFor(() => expect(ipc.setClaudeCodeModel).toHaveBeenCalledWith(""));
    // …and syncs the Primary Runtime's default model so the chat header matches
    // the pick (CLI-default → Sonnet).
    await waitFor(() =>
      expect(ipc.setDefaultModel).toHaveBeenCalledWith("claude-sonnet-4-6"),
    );
    // …and default permission = runCommands ("edit files and run commands" on).
    await waitFor(() =>
      expect(ipc.updateConnectionSettings).toHaveBeenCalledWith(
        expect.objectContaining({ permissionMode: "runCommands", apiKey: null }),
      ),
    );

    // Step 4: "Just me" is the default → Next clears any relay (local-only) and
    // advances to the appearance step.
    await screen.findByText("Team & sync");
    await clickByName("Next");
    // Step 5: pick-a-look → Finish completes onboarding.
    await screen.findByText("Make it yours");
    await clickByName("Finish");
    await waitFor(() => expect(onComplete).toHaveBeenCalled());
    expect(ipc.updateConnectionSettings).toHaveBeenCalledWith(
      expect.objectContaining({ relayUrl: "" }),
    );
    // No team-join happened on the solo path.
    expect(ipc.joinWorkspace).not.toHaveBeenCalled();
  });

  it("picks Claude Code + Opus and makes Opus the Primary Runtime default", async () => {
    render(<Onboarding onComplete={vi.fn()} />);

    await screen.findByPlaceholderText("Your name");
    await clickByName("Next"); // identity
    await clickByName("Next"); // project

    await screen.findByText("Choose your agent");
    // Claude Code is preselected; choose Opus in its model dropdown.
    await userEvent.selectOptions(
      screen.getByLabelText("Claude Code model"),
      "opus",
    );
    await clickByName("Next");

    // The CLI --model flag AND the displayed/default Primary Runtime both move to
    // Opus — otherwise the chat header silently falls back to Sonnet.
    await waitFor(() => expect(ipc.setClaudeCodeModel).toHaveBeenCalledWith("opus"));
    await waitFor(() =>
      expect(ipc.setDefaultModel).toHaveBeenCalledWith("claude-opus-4-8"),
    );
  });

  it("surfaces a cached Claude Code model (Fable) and wires the default from it", async () => {
    render(<Onboarding onComplete={vi.fn()} />);

    await screen.findByPlaceholderText("Your name");
    await clickByName("Next"); // identity
    await clickByName("Next"); // project

    await screen.findByText("Choose your agent");
    // The dropdown is populated from listClaudeCodeModels — Fable (cached,
    // account-specific) is present even though it isn't hardcoded.
    const select = screen.getByLabelText("Claude Code model");
    await waitFor(() =>
      expect(
        Array.from((select as HTMLSelectElement).options).map((o) => o.value),
      ).toContain("claude-fable-5[1m]"),
    );
    await userEvent.selectOptions(select, "claude-fable-5[1m]");
    await clickByName("Next");

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

    await screen.findByPlaceholderText("Your name");
    await clickByName("Next"); // identity
    await clickByName("Next"); // project (skip-by-next)

    await screen.findByText("Choose your agent");
    await clickByName(/OpenAI-compatible API/);
    await userEvent.type(screen.getByPlaceholderText(/API key/), "sk-test-123");
    await clickByName("Next");

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

  it("holds on a failed agent probe, then proceeds on a second Continue", async () => {
    // A bad key: the probe fails, so the first Next must NOT advance/configure.
    (ipc.probeProvider as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      ok: false,
      latency_ms: 0,
      reply: "",
      error: "401 Unauthorized",
    });
    render(<Onboarding onComplete={vi.fn()} />);

    await screen.findByPlaceholderText("Your name");
    await clickByName("Next"); // identity
    await clickByName("Next"); // project

    await screen.findByText("Choose your agent");
    await clickByName(/Anthropic API key/);
    await userEvent.type(screen.getByPlaceholderText(/Anthropic API key/), "sk-ant-bad");
    await clickByName("Next"); // probes → fails → holds

    // The failure is surfaced and no runtime was configured yet.
    await screen.findByText(/won't answer until it works/);
    expect(ipc.addRuntime).not.toHaveBeenCalled();

    // A second Continue proceeds anyway (transient/edge escape hatch).
    await clickByName("Next");
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
    await screen.findByPlaceholderText("Your name");
    await screen.findByText("0011 2233 4455 6677 8899 aabb ccdd eeff");
    expect(screen.queryByText(/GitHub/)).toBeNull();
    fireEvent.change(screen.getByPlaceholderText("Your name"), { target: { value: "Ana" } });
    await clickByName("Next");
    await waitFor(() => expect(ipc.setDisplayName).toHaveBeenCalledWith("Ana"));
  });

  it("blocks Finish when a host relay is unauthorized, allows it once connected", async () => {
    const onComplete = vi.fn();
    // First probe: unauthorized; after a token, ok.
    (ipc.probeRelayAt as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce({ status: "unauthorized", detail: "token rejected" })
      .mockResolvedValueOnce({ status: "ok", detail: "" });

    render(<Onboarding onComplete={onComplete} />);
    await screen.findByPlaceholderText("Your name");
    await clickByName("Next"); // identity
    await clickByName("Next"); // project
    await screen.findByText("Choose your agent");
    await clickByName("Next"); // runtime → step 4
    await screen.findByText("Team & sync");

    // Choose "Sync through a relay" → "Your own relay", enter a URL, test → unauthorized.
    await clickByName(/Sync through a relay/);
    await userEvent.click(screen.getByRole("radio", { name: /Your own relay/ }));
    await userEvent.type(
      screen.getByPlaceholderText(/Relay URL/),
      "https://relay.example",
    );
    await clickByName("Test connection");
    await screen.findByText("token rejected");
    // Next is disabled — a broken relay can't advance past the team step.
    expect(screen.getByRole("button", { name: "Next" })).toBeDisabled();

    // Paste a token + re-test → connected → Next enabled.
    await userEvent.type(screen.getByPlaceholderText(/access token/i), "good-token");
    await clickByName("Test connection");
    await screen.findByText("✓ Connected");
    expect(screen.getByRole("button", { name: "Next" })).toBeEnabled();
  });

  async function toTeamStep() {
    await screen.findByPlaceholderText("Your name");
    await clickByName("Next"); // identity
    await clickByName("Next"); // project
    await screen.findByText("Choose your agent");
    await clickByName("Next"); // agent (Claude Code)
    await screen.findByText("Team & sync");
  }

  it("offers the community relay preselected, probes it and saves it", async () => {
    render(<Onboarding onComplete={vi.fn()} />);
    await toTeamStep();
    expect(screen.getByRole("button", { name: /Just me \(no sync\)/ })).toBeTruthy();
    await clickByName(/Sync through a relay/);
    const community = screen.getByRole("radio", { name: /Community relay \(relay\.langstroth\.ai, best-effort\)/ });
    expect(community.getAttribute("aria-checked")).toBe("true");
    // No relay URL field for the community relay.
    expect(screen.queryByLabelText("Relay URL")).toBeNull();
    await clickByName("Test connection");
    await waitFor(() => expect(ipc.probeRelayAt).toHaveBeenCalledWith(COMMUNITY_RELAY_URL, null));
    await screen.findByText(/Connected/);
    await clickByName("Next");
    await screen.findByText("Make it yours");
    expect(ipc.updateConnectionSettings).toHaveBeenCalledWith(
      expect.objectContaining({ relayUrl: COMMUNITY_RELAY_URL, relayAccessToken: "" }),
    );
  });

  it("uses a typed URL and token for your own relay", async () => {
    render(<Onboarding onComplete={vi.fn()} />);
    await toTeamStep();
    await clickByName(/Sync through a relay/);
    await userEvent.click(screen.getByRole("radio", { name: /Your own relay/ }));
    // Test connection waits for a URL.
    expect((screen.getByRole("button", { name: "Test connection" }) as HTMLButtonElement).disabled).toBe(true);
    await userEvent.type(screen.getByLabelText("Relay URL"), "https://relay.acme.example");
    await userEvent.type(screen.getByPlaceholderText(/Access token/), "tok-1");
    await clickByName("Test connection");
    await waitFor(() => expect(ipc.probeRelayAt).toHaveBeenCalledWith("https://relay.acme.example", "tok-1"));
    await screen.findByText(/Connected/);
    await clickByName("Next");
    await screen.findByText("Make it yours");
    expect(ipc.updateConnectionSettings).toHaveBeenCalledWith(
      expect.objectContaining({ relayUrl: "https://relay.acme.example", relayAccessToken: "tok-1" }),
    );
  });
});

