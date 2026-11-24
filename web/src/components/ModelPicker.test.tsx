import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import * as ipc from "@/lib/ipc";
import type { RuntimeModelsDto, RuntimeSummaryDto } from "@/lib/ipc";
import {
  ModelFieldHelp,
  ModelStep,
  looksModelUnavailable,
  modelProblemText,
  modelRows,
  refusedByList,
  runtimeLabelWithModel,
} from "./ModelPicker";

vi.mock("@/lib/ipc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/ipc")>()),
  listRuntimeModels: vi.fn(),
}));

const rt = (o: Partial<RuntimeSummaryDto>): RuntimeSummaryDto =>
  ({
    id: "rt-copilot",
    name: "copilot",
    label: "GitHub Copilot CLI",
    provider: "copilot",
    location: "local",
    model: "claude-opus-4.8",
    endpoint: "",
    supportsTools: true,
    supportsEmbeddings: false,
    isDefault: false,
    isManaged: true,
    modelBaseUrl: null,
    modelProviderId: null,
    contextWindow: null,
    ...o,
  }) as RuntimeSummaryDto;

const copilotList: RuntimeModelsDto = {
  models: [{ id: "claude-sonnet-5.5" }, { id: "claude-opus-4.8" }, { id: "gpt-5.3-codex", label: "GPT-5.3 Codex" }],
  source: "catalog",
  complete: false,
  note: "Copilot lists every model it knows; your plan may not include all of them.",
  unavailable: ["claude-opus-4.8"],
};
const ollamaList: RuntimeModelsDto = {
  models: [{ id: "qwen3-coder:30b" }, { id: "llama3.2:latest" }],
  source: "live",
  complete: true,
  unavailable: [],
};

function wrap(ui: React.ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>);
}

beforeEach(() => {
  vi.mocked(ipc.listRuntimeModels).mockReset();
});

describe("model labels and checks", () => {
  it("shows the chat's choice in place of the runtime's model", () => {
    expect(runtimeLabelWithModel(rt({}), null)).toBe("copilot / claude-opus-4.8");
    expect(runtimeLabelWithModel(rt({}), "gpt-5.3-codex")).toBe("copilot / gpt-5.3-codex");
    expect(runtimeLabelWithModel(rt({ model: "" }), "  ")).toBe("copilot");
  });

  it("refuses only what a complete list leaves out", () => {
    expect(refusedByList(ollamaList, "llama3.2")).toBe(false);
    expect(refusedByList(ollamaList, "qwen3-coder:30b")).toBe(false);
    expect(refusedByList(ollamaList, "mistral")).toBe(true);
    expect(refusedByList(copilotList, "anything-new")).toBe(false);
    expect(refusedByList(ollamaList, "")).toBe(false);
    expect(refusedByList(undefined, "x")).toBe(false);
  });

  it("recognizes a model-unavailable failure and nothing else", () => {
    expect(looksModelUnavailable("Copilot CLI: Model is not available")).toBe(true);
    expect(looksModelUnavailable('ollama: model "llama9" not found, try pulling it first')).toBe(true);
    expect(looksModelUnavailable("model_not_found")).toBe(true);
    expect(looksModelUnavailable("The model is temporarily unavailable")).toBe(false);
    expect(looksModelUnavailable("Copilot CLI isn't signed in")).toBe(false);
  });

  it("says in one line what is wrong", () => {
    expect(modelProblemText({ model: "claude-opus-4.8", reason: "unavailable" }, rt({}))).toBe(
      "claude-opus-4.8 isn't available to this Copilot account.",
    );
    expect(modelProblemText({ model: "mistral", reason: "unavailable" }, rt({ provider: "ollama" }))).toBe(
      "mistral isn't on this Ollama server.",
    );
    expect(modelProblemText({ model: "gemini-9", reason: "notListed" }, rt({ provider: "antigravity" }))).toBe(
      "gemini-9 isn't one of the models Antigravity lists.",
    );
  });

  it("keeps the current choice visible and marks unavailable models", () => {
    const rows = modelRows(copilotList, "my-custom", "");
    expect(rows.map((r) => r.id)).toEqual(["my-custom", "claude-sonnet-5.5", "claude-opus-4.8", "gpt-5.3-codex"]);
    expect(rows[0].hint).toBe("typed");
    expect(rows.find((r) => r.id === "claude-opus-4.8")?.unavailable).toBe(true);
    expect(modelRows(copilotList, null, "codex").map((r) => r.id)).toEqual(["gpt-5.3-codex"]);
  });
});

describe("ModelStep", () => {
  it("lists Default, the harness's models and Other…, and picks one", async () => {
    vi.mocked(ipc.listRuntimeModels).mockResolvedValue(copilotList);
    const onPick = vi.fn();
    wrap(<ModelStep runtime={rt({})} chosen={null} appliesHere onBack={() => {}} onPick={onPick} />);
    expect(await screen.findByText("gpt-5.3-codex")).toBeInTheDocument();
    expect(ipc.listRuntimeModels).toHaveBeenCalledWith({ runtimeId: "rt-copilot" });
    expect(screen.getByRole("menuitemradio", { name: /Default/ })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByText("unavailable here")).toBeInTheDocument();
    expect(screen.getByText(/your plan may not include/)).toBeInTheDocument();
    await userEvent.click(screen.getByText("gpt-5.3-codex"));
    expect(onPick).toHaveBeenCalledWith("gpt-5.3-codex");
    await userEvent.click(screen.getByText("Default"));
    expect(onPick).toHaveBeenLastCalledWith(null);
  });

  it("works from the keyboard: arrows move, Enter picks, left goes back", async () => {
    vi.mocked(ipc.listRuntimeModels).mockResolvedValue(copilotList);
    const onPick = vi.fn();
    const onBack = vi.fn();
    wrap(<ModelStep runtime={rt({})} chosen="claude-sonnet-5.5" appliesHere onBack={onBack} onPick={onPick} />);
    const sonnet = await screen.findByRole("menuitemradio", { name: /claude-sonnet-5.5/ });
    await waitFor(() => expect(sonnet).toHaveFocus());
    await userEvent.keyboard("{ArrowDown}");
    expect(screen.getByRole("menuitemradio", { name: /^claude-opus-4\.8\s*unavailable/ })).toHaveFocus();
    await userEvent.keyboard("{Enter}");
    expect(onPick).toHaveBeenCalledWith("claude-opus-4.8");
    await userEvent.keyboard("{ArrowLeft}");
    expect(onBack).toHaveBeenCalled();
  });

  it("takes a typed model, but not one a complete list leaves out", async () => {
    vi.mocked(ipc.listRuntimeModels).mockResolvedValue(ollamaList);
    const onPick = vi.fn();
    wrap(<ModelStep runtime={rt({ id: "rt-ollama", provider: "ollama", model: "qwen3-coder:30b" })} chosen={null} appliesHere onBack={() => {}} onPick={onPick} />);
    await screen.findByText("llama3.2:latest");
    await userEvent.click(screen.getByText("Other…"));
    const field = screen.getByRole("textbox", { name: "Model name" });
    await userEvent.type(field, "mistral{Enter}");
    expect(onPick).not.toHaveBeenCalled();
    expect(screen.getByText(/won't run/)).toBeInTheDocument();
    await userEvent.clear(field);
    await userEvent.type(field, "llama3.2{Enter}");
    expect(onPick).toHaveBeenCalledWith("llama3.2");
  });

  it("says when another member's device answers, and when listing fails", async () => {
    vi.mocked(ipc.listRuntimeModels).mockResolvedValue({ models: [], source: "live", complete: false, error: "`agy models` didn't answer within 20s", unavailable: [] });
    wrap(<ModelStep runtime={rt({ provider: "antigravity" })} chosen={null} appliesHere={false} onBack={() => {}} onPick={() => {}} />);
    expect(await screen.findByText(/Couldn't list models: `agy models` didn't answer/)).toBeInTheDocument();
    expect(screen.getByText(/answered on another member's device/)).toBeInTheDocument();
    expect(screen.getByText("Other…")).toBeInTheDocument();
  });

  it("lists again on request, skipping the cache", async () => {
    vi.mocked(ipc.listRuntimeModels).mockResolvedValue(copilotList);
    wrap(<ModelStep runtime={rt({})} chosen={null} appliesHere onBack={() => {}} onPick={() => {}} />);
    await screen.findByText("gpt-5.3-codex");
    fireEvent.click(screen.getByRole("button", { name: "List models again" }));
    await waitFor(() => expect(ipc.listRuntimeModels).toHaveBeenCalledWith({ runtimeId: "rt-copilot", refresh: true }));
  });
});

describe("ModelFieldHelp (Settings)", () => {
  it("lists a subscription CLI's models at once and flags a model a complete list leaves out", async () => {
    vi.mocked(ipc.listRuntimeModels).mockResolvedValue({ ...ollamaList, models: [{ id: "gemini-3.8-flash-low" }] });
    const { container, rerender } = wrap(<ModelFieldHelp listId="l" provider="antigravity" model="" />);
    expect(await screen.findByText(/1 model to pick from/)).toBeInTheDocument();
    expect(container.querySelector("datalist#l option")?.getAttribute("value")).toBe("gemini-3.8-flash-low");
    rerender(
      <QueryClientProvider client={new QueryClient()}>
        <ModelFieldHelp listId="l" provider="antigravity" model="gemini-9" />
      </QueryClientProvider>,
    );
    expect(await screen.findByRole("alert")).toHaveTextContent("gemini-9 isn't one of its models, so it won't run.");
  });

  it("asks an HTTP provider only when told to", async () => {
    vi.mocked(ipc.listRuntimeModels).mockResolvedValue(ollamaList);
    wrap(<ModelFieldHelp listId="l" provider="openai" endpoint="http://box:8080" model="" />);
    expect(ipc.listRuntimeModels).not.toHaveBeenCalled();
    await userEvent.click(screen.getByText("List this provider's models"));
    await waitFor(() => expect(ipc.listRuntimeModels).toHaveBeenCalledWith({ provider: "openai", endpoint: "http://box:8080" }));
  });

  it("asks again before listing an edited endpoint", async () => {
    vi.mocked(ipc.listRuntimeModels).mockResolvedValue(ollamaList);
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const at = (endpoint: string) => (
      <QueryClientProvider client={qc}>
        <ModelFieldHelp listId="l" provider="custom" endpoint={endpoint} model="" />
      </QueryClientProvider>
    );
    const { rerender } = render(at("http://box:8080"));
    await userEvent.click(screen.getByText("List this provider's models"));
    await waitFor(() => expect(ipc.listRuntimeModels).toHaveBeenCalledTimes(1));
    for (const typed of ["http://evil.example", "http://evil.example/x"]) {
      rerender(at(typed));
      expect(await screen.findByText("List this provider's models")).toBeInTheDocument();
    }
    expect(ipc.listRuntimeModels).toHaveBeenCalledTimes(1);
    await userEvent.click(screen.getByText("List this provider's models"));
    await waitFor(() =>
      expect(ipc.listRuntimeModels).toHaveBeenLastCalledWith({ provider: "custom", endpoint: "http://evil.example/x" }),
    );
    expect(ipc.listRuntimeModels).toHaveBeenCalledTimes(2);
  });
});
