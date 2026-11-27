import { useEffect, useState, type ReactNode } from "react";
import { openSetup } from "@/lib/nav";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { RuntimeSummaryDto } from "@/bindings/RuntimeSummaryDto";
import { GatewayPicker, gatewayModels } from "./GatewayPicker";
import { ListToolsRow } from "./ListToolsRow";
import {
  addRuntime,
  listWorkspaceHosts,
  endpointIsPrivate,
  getAppSettings,
  getClaudeCodeModel,
  setClaudeCodeModel,
  listClaudeCodeModels,
  setDefaultModel,
  listRuntimes,
  removeRuntime,
  testRuntime,
  probeProvider,
  type RuntimeTestResult,
  setDefaultRuntime,
  listProviders,
  detectProviders,
  type DetectedProviderDto,
  exportConfig,
  importConfig,
  listProviderPresets,
  modelProfileHint,
  setProviderKey,
  setProviderBaseUrl,
  type ProviderDto,
  getContextCommands,
  setContextCommands,
  getUtilityRuntime,
  setUtilityRuntime,
  getRetrievalSettings,
  setRetrievalSettings,
  testRetrievalSettings,
  type RetrievalSettings,
  listOllamaModels,
  pullOllamaModel,
  cancelOllamaPull,
  deleteOllamaModel,
  onOllamaPull,
  openExternal,
  type OllamaPullEvent,
} from "@/lib/ipc";
import { IconChevronDown, IconChevronRight } from "@/lib/icons";
import { Button, FormDisclosure, ListRow, RowList, Section, Switch, fieldStyle, InfoTip } from "@/components/ui";
import { Sheet } from "@/components/kit/Sheet";
import { SELECT_CLASS } from "@/components/settings/shared";
import { useRowMenu, type RowAction } from "@/components/RowMenu";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";
import { loadTemplates, addTemplate, removeTemplate, type PromptTemplate } from "@/lib/templates";
import { quickTaskOptions } from "@/lib/utilityRuntime";
import { CliRuntimesPanel } from "@/components/settings/CliRuntimesPanel";
import { ModelFieldHelp } from "@/components/ModelPicker";

/// Models & runtimes: providers + their credentials, the runtimes (models)
/// built on them, reusable agent personas, prompt templates, and the context
/// commands. Settings owns installing/configuring; the Tools pane attaches.
export function ModelsSection() {
  return (
    <>
      <ProvidersSection />
      <RuntimesSection />
      <QuickTasksSection />
      <AgentsSection />
      <TemplatesSection />
      <ContextCommandsSection />
      <VaultRetrievalSection />
      <PortableConfigSection />
    </>
  );
}

/// Export the runtime/MCP setup as portable TOML (no secrets), or import
/// one — so an architect can version-control a setup and stamp it across a team.
function PortableConfigSection() {
  const qc = useQueryClient();
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  // The TOML opens in a sheet: after Export (to read or copy it), or for Import.
  const [sheet, setSheet] = useState<"export" | "import" | null>(null);

  async function doExport() {
    setBusy(true);
    try {
      const toml = await exportConfig();
      setText(toml);
      setSheet("export");
      await navigator.clipboard.writeText(toml).catch(() => {});
      toast.success("Config exported + copied to clipboard.");
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }
  async function doImport() {
    if (!text.trim()) return;
    setBusy(true);
    try {
      const summary = await importConfig(text);
      qc.invalidateQueries({ queryKey: ["runtimes"] });
      qc.invalidateQueries({ queryKey: ["providers"] });
      qc.invalidateQueries({ queryKey: ["mcp"] });
      toast.success(summary);
      setSheet(null);
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Section
      title="Portable config"
      info="Export your runtimes and MCP servers as TOML to version-control and share with a team. No API keys or tokens are included (each records only the env var its key comes from). Paste one here and Import to stamp that setup onto this machine (additive; nothing is deleted)."
      action={
        <span className="flex gap-1.5">
          <Button size="sm" variant="ghost" disabled={busy} onClick={() => void doExport()}>
            Export
          </Button>
          <Button
            size="sm"
            disabled={busy}
            onClick={() => {
              setText("");
              setSheet("import");
            }}
          >
            Import…
          </Button>
        </span>
      }
    >
      <p className="px-1 text-[11.5px] text-ink-soft">No API keys or tokens are included.</p>
      {sheet && (
        <Sheet
          title={sheet === "export" ? "Exported config" : "Import a config"}
          description={sheet === "export" ? "Copied to the clipboard." : "Additive: nothing is deleted."}
          side="center"
          width={560}
          onClose={() => setSheet(null)}
          footer={
            sheet === "import" ? (
              <>
                <Button onClick={() => setSheet(null)}>Cancel</Button>
                <Button variant="primary" disabled={busy || !text.trim()} onClick={() => void doImport()}>
                  Import
                </Button>
              </>
            ) : (
              <Button onClick={() => setSheet(null)}>Done</Button>
            )
          }
        >
          <textarea
            value={text}
            onChange={(e) => setText(e.target.value)}
            readOnly={sheet === "export"}
            aria-label={sheet === "export" ? "Exported config" : "Config to import"}
            placeholder="# Paste a ligus.toml to import…"
            spellCheck={false}
            className="h-56 w-full rounded-xl border p-3 font-mono text-xs"
            style={fieldStyle}
          />
        </Sheet>
      )}
    </Section>
  );
}

/// A provider counts as "configured" once it has a key or a custom base URL —
/// i.e. the user set it up. Those show in the list; everything else is tucked
/// behind the "Add a provider…" selector so the section stays short.
function isProviderConfigured(p: ProviderDto): boolean {
  return p.hasKey || (p.supportsBaseUrl && p.baseUrl.trim() !== "");
}

/// "Detect & configure": sweep the machine for providers that would answer now
/// (saved/env keys, local Ollama, claude/codex CLIs) and add runtimes for them
/// in one click — the power-user / fresh-machine fast path.
function DetectPanel({ onConfigured }: { onConfigured: () => void }) {
  const [found, setFound] = useState<DetectedProviderDto[] | null>(null);
  const [busy, setBusy] = useState(false);

  async function detect() {
    setBusy(true);
    try {
      setFound(await detectProviders());
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  async function addOne(p: DetectedProviderDto) {
    const m = MODEL_SUGGESTIONS[p.kind]?.[0] ?? "";
    const id = `${p.kind}-${m || "default"}`.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
    await addRuntime(id, `${p.label}${m ? ` · ${m}` : ""}`, p.kind, p.needsKey ? "remote" : "local", "", m, true, false);
  }

  async function addAll() {
    setBusy(true);
    try {
      for (const p of (found ?? []).filter((p) => p.addable)) await addOne(p);
      onConfigured();
      toast.success("Added detected models — pick one from the route pill in any chat.");
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  const addable = (found ?? []).filter((p) => p.addable);
  return (
    <div className="mb-3 border-b pb-3" style={{ borderColor: "var(--ligus-line)" }}>
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-1.5">
          <div className="text-sm font-medium">Detect my setup</div>
          <InfoTip
            label="Detect my setup"
            text="Finds providers that would answer right now: saved or environment API keys, a local Ollama server, and the Claude Code / Codex CLIs on your PATH."
          />
        </div>
        <Button size="sm" variant="ghost" disabled={busy} onClick={() => void detect()}>
          {busy ? "Scanning…" : found ? "Re-scan" : "Detect"}
        </Button>
      </div>
      {found && found.length === 0 && (
        <p className="mt-2 text-xs text-ink-soft">
          Nothing detected. Add a provider, or set a key or start Ollama and re-scan.
        </p>
      )}
      {found && found.length > 0 && (
        <div className="mt-2 space-y-1.5">
          <RowList label="Detected providers">
            {found.map((p) => (
              <ListRow
                key={p.kind}
                title={p.label}
                meta={p.source}
                trailing={
                  p.addable ? (
                    <Button size="sm" variant="ghost" disabled={busy} onClick={() => void addOne(p).then(onConfigured)}>
                      Add
                    </Button>
                  ) : (
                    <span className="text-[11.5px] text-ink-soft">available as @ligus</span>
                  )
                }
              />
            ))}
          </RowList>
          {addable.length > 1 && (
            <div className="pt-1">
              <Button size="sm" variant="primary" disabled={busy} onClick={() => void addAll()}>
                Add all {addable.length} working
              </Button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function ProvidersSection() {
  const qc = useQueryClient();
  const providers = useQuery({ queryKey: ["providers"], queryFn: listProviders });
  const refresh = () => qc.invalidateQueries({ queryKey: ["providers"] });
  // Providers the user opened via "Add" but hasn't saved a key for yet, and which
  // row is currently expanded.
  const [added, setAdded] = useState<string[]>([]);
  const [openKind, setOpenKind] = useState<string | null>(null);

  const all = providers.data ?? [];
  const visible = all.filter((p) => isProviderConfigured(p) || added.includes(p.kind));
  const available = all.filter((p) => !visible.includes(p));

  return (
    <Section
      title="LLM providers"
      info="A provider is a backend and how to reach it (key and/or base URL). Add the ones you use; the rest stay tucked away. Models (below) pick a provider; agents pick a model."
      action={
        available.length > 0 ? (
          <select
            value=""
            aria-label="Add a provider"
            onChange={(e) => {
              const kind = e.target.value;
              if (!kind) return;
              setAdded((a) => (a.includes(kind) ? a : [...a, kind]));
              setOpenKind(kind);
            }}
            className="rounded-lg border px-2 py-1 text-[12.5px]"
            style={fieldStyle}
          >
            <option value="">Add a provider…</option>
            {available.map((p) => (
              <option key={p.kind} value={p.kind}>
                {p.name}
              </option>
            ))}
          </select>
        ) : undefined
      }
    >
      <DetectPanel onConfigured={() => qc.invalidateQueries({ queryKey: ["runtimes"] })} />
      <CliRuntimesPanel />
      <ListToolsRow />
      {visible.length === 0 && <p className="px-1 py-2 text-xs text-ink-soft">No providers yet.</p>}
      <RowList label="Providers">
        {visible.map((p) => (
          <ProviderRow
            key={p.kind}
            provider={p}
            open={openKind === p.kind}
            configured={isProviderConfigured(p)}
            onToggle={() => setOpenKind(openKind === p.kind ? null : p.kind)}
            onChanged={refresh}
            onHide={() => {
              setAdded((a) => a.filter((k) => k !== p.kind));
              if (openKind === p.kind) setOpenKind(null);
            }}
          />
        ))}
      </RowList>
    </Section>
  );
}

// Suggested model ids per provider, for the one-step add-model datalist. Just
// hints — any model string is accepted.
const MODEL_SUGGESTIONS: Record<string, string[]> = {
  anthropic: ["claude-opus-4-8", "claude-sonnet-4-6", "claude-haiku-4-5-20251001"],
  openAI: ["gpt-4o", "gpt-4o-mini", "o3-mini"],
  openRouter: ["openai/gpt-4o-mini", "anthropic/claude-3.5-sonnet"],
  ollama: ["llama3.2", "qwen2.5-coder", "deepseek-r1"],
};

function ProviderRow({
  provider,
  open,
  configured,
  onToggle,
  onChanged,
  onHide,
}: {
  provider: ProviderDto;
  open: boolean;
  configured: boolean;
  onToggle: () => void;
  onChanged: () => void;
  onHide: () => void;
}) {
  const [key, setKey] = useState("");
  const [baseUrl, setBaseUrl] = useState(provider.baseUrl);
  const qc = useQueryClient();
  // One-step "add model": the typed model the Add / Test buttons act on.
  const [model, setModel] = useState("");
  // Inline key/connection test result (null = not run, "running" = in flight).
  const [probe, setProbe] = useState<RuntimeTestResult | "running" | null>(null);

  async function runProbe() {
    setProbe("running");
    try {
      // Test the just-typed key/base URL if present (before Save), else stored.
      setProbe(
        await probeProvider(
          provider.kind,
          model.trim() || undefined,
          key.trim() || undefined,
          baseUrl.trim() || undefined,
        ),
      );
    } catch (e) {
      setProbe({ ok: false, latency_ms: 0, reply: "", error: errMsg(e) });
    }
  }

  // Add a runtime for this provider with sane defaults — collapses the separate
  // provider→runtime step for the common cloud case. Advanced knobs (endpoint,
  // context window, embeddings) stay in the Runtimes section below.
  const addModel = useMutation({
    mutationFn: async () => {
      const m = model.trim();
      if (!m) throw new Error("Enter a model name first.");
      const id = `${provider.kind}-${m}`.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
      const local = !provider.needsKey;
      await addRuntime(
        id,
        `${provider.name} · ${m}`,
        provider.kind,
        local ? "local" : "remote",
        provider.baseUrl.trim(), // "" lets the backend use the provider default
        m,
        true, // supportsTools
        false, // supportsEmbeddings
      );
    },
    onSuccess: () => {
      setModel("");
      qc.invalidateQueries({ queryKey: ["runtimes"] });
      toast.success("Model added — pick it from the route pill in any chat.");
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const saveKey = useMutation({
    mutationFn: () => setProviderKey(provider.kind, key.trim()),
    onSuccess: () => {
      setKey("");
      onChanged();
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const saveBase = useMutation({
    mutationFn: () => setProviderBaseUrl(provider.kind, baseUrl.trim()),
    onSuccess: () => onChanged(),
    onError: (e) => toast.error(errMsg(e)),
  });
  // Remove = clear whatever made it configured (key + base URL) and refresh; for
  // a not-yet-configured row the user just added, hide it again.
  const remove = useMutation({
    mutationFn: async () => {
      if (provider.hasKey) await setProviderKey(provider.kind, "");
      if (provider.supportsBaseUrl && provider.baseUrl.trim() !== "")
        await setProviderBaseUrl(provider.kind, "");
    },
    onSuccess: () => {
      onChanged();
      onHide();
    },
    onError: (e) => toast.error(errMsg(e)),
  });

  const localOnly = !provider.needsKey && !provider.supportsBaseUrl;
  const rm = useRowMenu(`Actions for ${provider.name}`, [
    ...(provider.needsKey
      ? [
          {
            label: probe === "running" ? "Testing…" : "Test connection",
            onSelect: () => void runProbe(),
            disabled: probe === "running" || !provider.hasKey,
          },
        ]
      : []),
    {
      label: configured ? "Remove provider" : "Hide",
      danger: configured,
      onSelect: () => (configured ? remove.mutate() : onHide()),
    },
  ]);
  return (
    <li className="px-1" {...rm.rowProps}>
      <div className="flex items-center gap-1">
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={open}
        className="ligus-listrow flex min-w-0 flex-1 items-center justify-between gap-2 rounded-lg text-left text-[13px] hover:bg-[color:var(--ligus-overlay)]"
      >
        <span className="flex items-center gap-2">
          <span className="text-ink-soft">{open ? <IconChevronDown size={13} /> : <IconChevronRight size={13} />}</span>
          <span className="font-medium">{provider.name}</span>
        </span>
        <span className="flex items-center gap-2 text-[11.5px]">
          {provider.needsKey ? (
            <span
              data-tip={provider.hasKey ? "API key set" : "No API key yet"}
              style={{ color: provider.hasKey ? "var(--ligus-success-ink)" : "var(--ligus-accent-warm)" }}
            >
              {provider.hasKey ? "● key set" : "○ no key"}
            </span>
          ) : configured && provider.baseUrl.trim() !== "" ? (
            <span className="text-ink-soft">custom URL</span>
          ) : (
            <span className="text-ink-soft">{localOnly ? "local" : "—"}</span>
          )}
        </span>
      </button>
      {rm.button}
      </div>
      {rm.menu}
      {open && (
        <div className="pb-3 pl-5 pr-1">
          <div className="text-xs text-ink-soft">{provider.note}</div>
          {provider.needsKey && (
            <div className="mt-2 flex items-center gap-2">
              <input
                type="password"
                value={key}
                onChange={(e) => setKey(e.target.value)}
                placeholder={provider.hasKey ? "configured — enter to replace" : "API key"}
                className="flex-1 rounded-xl border px-3 py-2 font-mono text-sm"
                style={fieldStyle}
              />
              <Button variant="primary" size="md" onClick={() => saveKey.mutate()}>
                Save
              </Button>
              <Button
                variant="ghost"
                size="md"
                disabled={probe === "running" || (provider.needsKey && !provider.hasKey && !key.trim())}
                onClick={() => void runProbe()}
              >
                {probe === "running" ? "Testing…" : "Test"}
              </Button>
            </div>
          )}
          {provider.supportsBaseUrl && (
            <div className="mt-2 flex items-center gap-2">
              <input
                value={baseUrl}
                onChange={(e) => setBaseUrl(e.target.value)}
                placeholder="Base URL (optional override)"
                className="flex-1 rounded-xl border px-3 py-2 font-mono text-xs"
                style={fieldStyle}
              />
              <Button variant="primary" size="md" onClick={() => saveBase.mutate()}>
                Save
              </Button>
            </div>
          )}

          {/* Inline connection/key test result. */}
          {probe && probe !== "running" && (
            <div
              className="mt-2 rounded-lg px-2.5 py-1.5 text-xs"
              style={{
                background: probe.ok
                  ? "color-mix(in srgb, var(--ligus-success) 12%, transparent)"
                  : "color-mix(in srgb, var(--ligus-danger) 12%, transparent)",
                color: probe.ok ? "var(--ligus-success-ink)" : "var(--ligus-danger-ink)",
              }}
            >
              {probe.ok
                ? `✓ Connected (${probe.latency_ms} ms)`
                : `✕ ${probe.error ?? "Failed to connect"}`}
            </div>
          )}

          {/* One-step add-model: type a model → Add (creates a runtime with sane
              defaults). The full runtime form (endpoint/context/embeddings) stays
              in the Runtimes section below for advanced cases. */}
          {configured && (
            <div className="mt-2">
              <div className="flex items-center gap-2">
                <input
                  list={`models-${provider.kind}`}
                  value={model}
                  onChange={(e) => setModel(e.target.value)}
                  placeholder={`Model (e.g. ${MODEL_SUGGESTIONS[provider.kind]?.[0] ?? "gpt-4o"})`}
                  className="flex-1 rounded-xl border px-3 py-2 font-mono text-sm"
                  style={fieldStyle}
                />
                <datalist id={`models-${provider.kind}`}>
                  {(MODEL_SUGGESTIONS[provider.kind] ?? []).map((m) => (
                    <option key={m} value={m} />
                  ))}
                </datalist>
                <Button
                  variant="primary"
                  size="md"
                  disabled={addModel.isPending || !model.trim()}
                  onClick={() => addModel.mutate()}
                >
                  Add model
                </Button>
              </div>
              <p className="mt-1 text-xs text-ink-soft">
                Adds a ready-to-use runtime for this model. Test above validates the key first.
              </p>
            </div>
          )}
        </div>
      )}
    </li>
  );
}

// The Primary Runtime's model (the default when no runtime is configured) is
// editable here. Common Anthropic models + whatever's currently set.
const DEFAULT_MODELS = [
  "claude-opus-4-8",
  "claude-sonnet-4-6",
  "claude-haiku-4-5-20251001",
  "claude-3-5-sonnet-latest",
];
function DefaultModelPicker() {
  const qc = useQueryClient();
  const settings = useQuery({ queryKey: ["settings"], queryFn: getAppSettings });
  const current = settings.data?.model ?? "";
  const options = current && !DEFAULT_MODELS.includes(current) ? [current, ...DEFAULT_MODELS] : DEFAULT_MODELS;
  const save = useMutation({
    mutationFn: setDefaultModel,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["settings"] });
      qc.invalidateQueries({ queryKey: ["runtimes"] });
    },
    onError: (e) => toast.error(`Couldn't set model: ${errMsg(e)}`),
  });
  return (
    <div className="mb-2 px-1">
      <div className="flex items-center gap-1.5">
        <label className="block text-xs text-ink-soft">Default model (Primary Runtime · Anthropic)</label>
        <InfoTip
          label="Default model"
          text="Used when a chat has no specific runtime selected. Separate from the local Claude Code model (set on its runtime row above)."
        />
      </div>
      <select
        value={current}
        onChange={(e) => save.mutate(e.target.value)}
        className={`mt-1 w-full ${SELECT_CLASS}`}
        style={fieldStyle}
      >
        {options.map((m) => (
          <option key={m} value={m}>
            {m}
          </option>
        ))}
      </select>
    </div>
  );
}

/// Whether the add-runtime form lists an Ollama server's models: only for
/// the Ollama provider, and only once there's a server to ask (a typed
/// address, an Ollama runtime already configured, or the user asked to look
/// on this machine). Never a background probe of localhost:11434.
export function formOllamaListEnabled(o: { provider: string; endpoint: string; ollamaConfigured: boolean; asked: boolean }): boolean {
  return o.provider === "ollama" && (o.endpoint.trim() !== "" || o.ollamaConfigured || o.asked);
}

/// The CLI harnesses: a gateway routes their own model calls, so picking one
/// keeps the harness.
const CLI_PROVIDERS = ["claude-code", "pi", "aider", "codex", "hermes", "antigravity", "copilot"];

/// The provider to use once a model gateway is picked: a CLI harness stays as
/// it is; any HTTP provider becomes the OpenAI-compatible "custom" one (what
/// the setup engine writes), so Ollama's address, keep-loaded and num_ctx
/// never apply to a gateway.
export function providerForGateway(provider: string, gateway: string): string {
  if (!gateway) return provider;
  return CLI_PROVIDERS.includes(provider) ? provider : "custom";
}

function RuntimesSection() {
  const qc = useQueryClient();
  const runtimes = useQuery({ queryKey: ["runtimes"], queryFn: listRuntimes });
  const [runtimeId, setRuntimeId] = useState("");
  const [runtimeName, setRuntimeName] = useState("");
  const [runtimeProvider, setRuntimeProvider] = useState("ollama");
  // `location` doesn't drive any behavior (dispatch routes by provider +
  // endpoint); we derive it from the endpoint just for the list label.
  const deriveLocation = () => {
    if (["claude-code", "pi", "aider", "codex", "hermes", "antigravity", "copilot"].includes(runtimeProvider)) return "local";
    const ep = `${runtimeEndpoint} ${runtimeBaseUrl}`.toLowerCase();
    return /localhost|127\.0\.0\.1|0\.0\.0\.0|::1/.test(ep) ? "local" : "remote";
  };
  const [runtimeEndpoint, setRuntimeEndpoint] = useState("");
  const [runtimeBaseUrl, setRuntimeBaseUrl] = useState("");
  const [runtimeModel, setRuntimeModel] = useState("");
  const [runtimeSupportsTools, setRuntimeSupportsTools] = useState(true);
  const [runtimeSupportsEmbeddings, setRuntimeSupportsEmbeddings] = useState(false);
  const [runtimeContextWindow, setRuntimeContextWindow] = useState("");
  // Native-Ollama request settings (shown only for the ollama provider).
  const [runtimeKeepAlive, setRuntimeKeepAlive] = useState("");
  // Thinking: native Ollama `think`, or chat_template_kwargs.enable_thinking
  // (+ gpt-oss reasoning_effort) for a local OpenAI-compatible server.
  const [runtimeThink, setRuntimeThink] = useState(false);
  // HTTP model runtimes: model rounds per tool-using turn ("" = default 24).
  const [runtimeMaxToolRounds, setRuntimeMaxToolRounds] = useState("");
  // Offer the built-in file tools to an endpoint that isn't local/private.
  const [runtimeAllowRemoteFileTools, setRuntimeAllowRemoteFileTools] = useState(false);
  // An operator-listed model gateway, picked by name from what hosts offer
  // ("" = the runtime's own endpoint).
  const [runtimeGateway, setRuntimeGateway] = useState("");
  // The models the hosts offering the picked gateway report (their signed
  // beats), for the model field's suggestions.
  const hosts = useQuery({ queryKey: ["workspace-hosts"], queryFn: listWorkspaceHosts, retry: false });
  const gatewayModelList = runtimeGateway ? gatewayModels(hosts.data ?? [], runtimeGateway) : [];
  const thinkProviders = ["ollama", "custom", "ligus-daemon"];
  const httpProviders = ["ollama", "openAI", "openRouter", "azure", "custom", "ligus-daemon"];
  // Providers whose "Supports tools" runs Ligus's agent tool loop (MCP + file
  // tools): ones that may front a local or self-hosted server. Hosted APIs
  // never get it, so workspace files never go to a third-party cloud.
  const agentToolProviders = ["ollama", "custom", "ligus-daemon"];
  // null = the form is adding; an id = editing that runtime in place (add_runtime
  // upserts by id, so the same form both adds and edits).
  const [editingId, setEditingId] = useState<string | null>(null);
  // The add/edit form opens in a sheet (from "Add a runtime" or a row's Edit).
  const [formOpen, setFormOpen] = useState(false);
  const [moreOptions, setMoreOptions] = useState(false);
  const presets = useQuery({ queryKey: ["provider-presets"], queryFn: listProviderPresets });

  // Per-runtime preflight state: { pending } while testing, { result } after.
  const [tests, setTests] = useState<
    Record<string, { pending?: boolean; result?: RuntimeTestResult }>
  >({});
  const runTest = async (id: string) => {
    setTests((t) => ({ ...t, [id]: { pending: true } }));
    try {
      const result = await testRuntime(id);
      setTests((t) => ({ ...t, [id]: { result } }));
    } catch (e) {
      setTests((t) => ({
        ...t,
        [id]: { result: { ok: false, latency_ms: 0, reply: "", error: errMsg(e) } },
      }));
    }
  };

  function resetRuntimeForm() {
    setEditingId(null);
    setRuntimeId("");
    setRuntimeName("");
    setRuntimeProvider("ollama");
    setRuntimeEndpoint("");
    setRuntimeBaseUrl("");
    setRuntimeModel("");
    setRuntimeContextWindow("");
    setRuntimeKeepAlive("");
    setRuntimeThink(false);
    setRuntimeMaxToolRounds("");
    setRuntimeAllowRemoteFileTools(false);
    setRuntimeGateway("");
    setRuntimeSupportsTools(true);
    setRuntimeSupportsEmbeddings(false);
  }

  function startEdit(runtime: RuntimeSummaryDto) {
    setEditingId(runtime.id);
    setRuntimeId(runtime.id);
    setRuntimeName(runtime.name);
    setRuntimeProvider(runtime.provider);
    setRuntimeEndpoint(runtime.endpoint);
    setRuntimeBaseUrl(runtime.modelBaseUrl ?? "");
    setRuntimeModel(runtime.model);
    setRuntimeContextWindow(runtime.contextWindow ? String(runtime.contextWindow) : "");
    setRuntimeKeepAlive(runtime.keepAlive ?? "");
    setRuntimeThink(runtime.think ?? false);
    setRuntimeMaxToolRounds(runtime.maxToolRounds ? String(runtime.maxToolRounds) : "");
    setRuntimeAllowRemoteFileTools(runtime.allowRemoteFileTools ?? false);
    setRuntimeGateway(runtime.gateway ?? "");
    setRuntimeSupportsTools(runtime.supportsTools);
    setRuntimeSupportsEmbeddings(runtime.supportsEmbeddings);
    setFormOpen(true);
  }

  const addRuntimeMutation = useMutation({
    mutationFn: () =>
      addRuntime(
        runtimeId.trim(),
        runtimeName.trim(),
        runtimeProvider,
        deriveLocation(),
        runtimeGateway ? "" : runtimeEndpoint.trim(),
        runtimeModel.trim(),
        runtimeSupportsTools,
        runtimeSupportsEmbeddings,
        runtimeGateway ? null : runtimeBaseUrl.trim() || null,
        // pi targets an OpenAI-compatible provider; default that id to "ollama".
        runtimeBaseUrl.trim() && runtimeProvider === "pi" ? "ollama" : null,
        Number(runtimeContextWindow) > 0 ? Number(runtimeContextWindow) : null,
        runtimeProvider === "ollama" ? runtimeKeepAlive.trim() || null : null,
        thinkProviders.includes(runtimeProvider) ? runtimeThink : null,
        agentToolProviders.includes(runtimeProvider) && Number(runtimeMaxToolRounds) > 0
          ? Number(runtimeMaxToolRounds)
          : null,
        agentToolProviders.includes(runtimeProvider) && runtimeAllowRemoteFileTools ? true : null,
        runtimeGateway || null,
      ),
    onSuccess: () => {
      resetRuntimeForm();
      setFormOpen(false);
      qc.invalidateQueries({ queryKey: ["runtimes"] });
    },
    onError: (e) => toast.error(`Couldn't save runtime: ${errMsg(e)}`),
  });
  const removeRuntimeMutation = useMutation({
    mutationFn: (id: string) => removeRuntime(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["runtimes"] }),
    onError: (e) => toast.error(`Couldn't remove runtime: ${errMsg(e)}`),
  });
  const setDefaultMutation = useMutation({
    mutationFn: (id: string) => setDefaultRuntime(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["runtimes"] }),
    onError: (e) => toast.error(`Couldn't set default: ${errMsg(e)}`),
  });

  // Ollama rows can expand a "Models on this server" panel (list / pull /
  // update / remove). Which rows are expanded, by runtime id.
  const [openModels, setOpenModels] = useState<Record<string, boolean>>({});
  // "Use" in that panel: re-save the runtime with the chosen model (add_runtime
  // upserts by id, so every other field round-trips unchanged).
  const useModelMutation = useMutation({
    mutationFn: ({ runtime, model }: { runtime: RuntimeSummaryDto; model: string }) =>
      addRuntime(
        runtime.id,
        runtime.name,
        runtime.provider,
        runtime.location,
        runtime.endpoint,
        model,
        runtime.supportsTools,
        runtime.supportsEmbeddings,
        runtime.modelBaseUrl,
        runtime.modelProviderId,
        runtime.contextWindow,
        runtime.keepAlive ?? null,
        runtime.think ?? null,
        runtime.maxToolRounds ?? null,
        runtime.allowRemoteFileTools ?? null,
        runtime.gateway ?? null,
      ),
    onSuccess: (_d, { model }) => {
      toast.success(`Runtime now uses ${model}.`);
      qc.invalidateQueries({ queryKey: ["runtimes"] });
    },
    onError: (e) => toast.error(`Couldn't switch model: ${errMsg(e)}`),
  });

  // Add-runtime form, Ollama provider: offer the server's installed models as
  // completions for the model field. Debounced so typing an endpoint doesn't
  // hit every partial host.
  const [formEndpoint, setFormEndpoint] = useState("");
  useEffect(() => {
    const t = setTimeout(() => setFormEndpoint(runtimeEndpoint.trim()), 400);
    return () => clearTimeout(t);
  }, [runtimeEndpoint]);
  // Role hint for the typed model, from Ligus's model-family profiles.
  const [formModel, setFormModel] = useState("");
  useEffect(() => {
    const t = setTimeout(() => setFormModel(runtimeModel.trim()), 250);
    return () => clearTimeout(t);
  }, [runtimeModel]);
  const modelProfile = useQuery({
    queryKey: ["model-profile", formModel],
    queryFn: () => modelProfileHint(formModel),
    enabled: formModel !== "",
    staleTime: Infinity,
  });
  const profile = formModel ? modelProfile.data ?? null : null;
  // Built-in file tools go only to local/private endpoints unless opted in.
  // Native Ollama with no endpoint talks to localhost.
  // A gateway has no endpoint here (its address stays on the host).
  const fileToolsEndpoint = runtimeGateway ? "" : formEndpoint || (runtimeProvider === "ollama" ? "http://localhost:11434" : "");
  const endpointPrivate = useQuery({
    queryKey: ["endpoint-private", fileToolsEndpoint],
    queryFn: () => endpointIsPrivate(fileToolsEndpoint),
    enabled: agentToolProviders.includes(runtimeProvider) && fileToolsEndpoint !== "",
    staleTime: Infinity,
  });
  // Known-public endpoint: show the opt-in (unknown/empty: nothing to say yet).
  const endpointIsRemote = fileToolsEndpoint !== "" && endpointPrivate.data === false;
  // Only ask an Ollama server for its models once there is one to ask: an
  // address typed here, an Ollama runtime already set up, or "look on this
  // machine" clicked. The form starts on Ollama, so probing by default hit
  // localhost:11434 for everyone who opened Settings.
  const [lookForLocalOllama, setLookForLocalOllama] = useState(false);
  const ollamaConfigured = (runtimes.data ?? []).some((r) => r.provider === "ollama");
  const formModels = useQuery({
    queryKey: ["ollama-models", "form", formEndpoint],
    queryFn: () => listOllamaModels({ endpoint: formEndpoint || null }),
    enabled: formOllamaListEnabled({ provider: runtimeProvider, endpoint: formEndpoint, ollamaConfigured, asked: lookForLocalOllama }),
    retry: false,
    refetchOnWindowFocus: false,
    staleTime: 30_000,
  });

  return (
    <Section
      title="Models (runtimes)"
      info="A model on a provider (above): what powers a chat or agent. Pick a provider kind and model; the key and base URL come from the provider. Mark one default for new chats."
      action={
        <Button
          size="sm"
          onClick={() => {
            resetRuntimeForm();
            setFormOpen(true);
          }}
        >
          Add a runtime
        </Button>
      }
    >
      {/* The "Default model" picker only configures the synthesized Anthropic
          Primary Runtime, which is surfaced only when no real runtime is
          configured. Once you have runtimes, the default is chosen per-row
          below, so this picker would point at a runtime that isn't listed. */}
      {(runtimes.data ?? []).some((r) => r.name === "Primary Runtime") && <DefaultModelPicker />}
      <RowList label="Models">
        {(runtimes.data ?? []).map((runtime) => {
          const result = tests[runtime.id]?.result;
          const ccModel = runtime.provider === "claude-code" && !runtime.isManaged;
          const ollamaOpen = runtime.provider === "ollama" && !!openModels[runtime.id];
          const actions: RowAction[] = [
            {
              label: tests[runtime.id]?.pending ? "Testing…" : "Test",
              onSelect: () => runTest(runtime.id),
              disabled: tests[runtime.id]?.pending,
              title: "Send a trivial prompt and verify this runtime answers",
            },
            ...(runtime.provider === "ollama"
              ? [
                  {
                    label: openModels[runtime.id] ? "Hide models" : "Models",
                    onSelect: () => setOpenModels((o) => ({ ...o, [runtime.id]: !o[runtime.id] })),
                    title: "See, pull, update, or remove models on this Ollama server",
                  },
                ]
              : []),
            ...(runtime.isManaged ? [{ label: "Edit", onSelect: () => startEdit(runtime) }] : []),
            ...(!runtime.isDefault
              ? [{ label: "Set default", onSelect: () => setDefaultMutation.mutate(runtime.id), disabled: setDefaultMutation.isPending }]
              : []),
            ...(runtime.isManaged
              ? [
                  {
                    label: "Remove",
                    danger: true,
                    onSelect: () =>
                      confirmThen(`Remove runtime "${runtime.label}"?`, () => removeRuntimeMutation.mutate(runtime.id)),
                  },
                ]
              : []),
          ];
          return (
            <ListRow
              key={runtime.id}
              title={runtime.label}
              actions={actions}
              meta={
                <>
                  {runtime.location} · {runtime.provider}
                  {runtime.gateway ? ` · via gateway ${runtime.gateway}` : runtime.endpoint ? ` · ${runtime.endpoint}` : ""}
                </>
              }
              trailing={
                runtime.isDefault ? (
                  <span className="text-[11px] text-ink-soft" data-tip="New chats use this runtime">
                    default
                  </span>
                ) : undefined
              }
            >
              {(ccModel || result || ollamaOpen) && (
                <div className="space-y-2 px-1">
                  {ccModel && <ClaudeCodeRowModel />}
                  {result && (
                    <div className="text-xs">
                      {result.ok ? (
                        <span style={{ color: "var(--ligus-success-ink)" }}>
                          ✓ Responded in {result.latency_ms} ms
                          {result.reply ? ` — "${result.reply.slice(0, 60)}"` : ""}
                          {result.capabilities ? ` · model: ${result.capabilities}` : ""}
                        </span>
                      ) : (
                        <span style={{ color: "var(--ligus-danger-ink)" }}>✕ {result.error ?? "No response"}</span>
                      )}
                    </div>
                  )}
                  {ollamaOpen && (
                    <OllamaModelsPanel
                      runtime={runtime}
                      switching={useModelMutation.isPending}
                      onUse={(model) => useModelMutation.mutate({ runtime, model })}
                    />
                  )}
                </div>
              )}
            </ListRow>
          );
        })}
      </RowList>
      {formOpen && (
      <Sheet
        title={editingId ? `Edit runtime · ${editingId}` : "Add a runtime"}
        side="center"
        width={540}
        onClose={() => {
          resetRuntimeForm();
          setFormOpen(false);
        }}
        footer={
          <>
            <Button
              onClick={() => {
                resetRuntimeForm();
                setFormOpen(false);
              }}
            >
              Cancel
            </Button>
            <Button variant="primary" disabled={addRuntimeMutation.isPending} onClick={() => addRuntimeMutation.mutate()}>
              {editingId ? "Update runtime" : "Add runtime"}
            </Button>
          </>
        }
      >
      <div className="space-y-2">
        <div className="grid gap-2 md:grid-cols-2">
          <input
            value={runtimeName}
            onChange={(e) => setRuntimeName(e.target.value)}
            placeholder="Display name"
            aria-label="Display name"
            className="rounded-xl border px-3 py-2 text-sm"
            style={fieldStyle}
          />
          <select
            defaultValue=""
            title="Quick-fill a known OpenAI-compatible backend"
            onChange={(e) => {
              const p = (presets.data ?? []).find((x) => x.label === e.target.value);
              e.target.value = "";
              if (!p) return;
              setRuntimeProvider(p.provider);
              setRuntimeEndpoint(p.endpoint);
              setRuntimeSupportsTools(p.tools);
              if (!runtimeName.trim()) setRuntimeName(p.label);
              if (!runtimeId.trim())
                setRuntimeId(p.label.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, ""));
            }}
            className="rounded-xl border px-3 py-2 text-sm"
            style={fieldStyle}
          >
            <option value="">Preset…</option>
            {(presets.data ?? []).map((p) => (
              <option key={p.label} value={p.label}>
                {p.label}
              </option>
            ))}
          </select>
          <select
            value={runtimeProvider}
            onChange={(e) => setRuntimeProvider(e.target.value)}
            aria-label="Provider"
            className="rounded-xl border px-3 py-2 text-sm"
            style={fieldStyle}
          >
            <option value="anthropic">Anthropic</option>
            <option value="openAI">OpenAI</option>
            <option value="openRouter">OpenRouter</option>
            {!runtimeGateway && <option value="ollama">Ollama</option>}
            <option value="azure">Azure OpenAI</option>
            <option value="custom">OpenAI-compatible (custom)</option>
            <option value="claude-code">Claude Code</option>
            <option value="pi">Pi</option>
            <option value="aider">Aider</option>
            <option value="codex">Codex</option>
            <option value="hermes">Hermes (generic CLI)</option>
            <option value="antigravity">Google Antigravity CLI (agy)</option>
            <option value="copilot">GitHub Copilot CLI</option>
          </select>
        </div>
        <GatewayPicker
          hosts={hosts.data ?? []}
          value={runtimeGateway}
          onChange={(g) => {
            setRuntimeGateway(g);
            setRuntimeProvider((p) => providerForGateway(p, g));
          }}
          style={fieldStyle}
        />
        {!runtimeGateway && (
        <input
          value={runtimeEndpoint}
          onChange={(e) => setRuntimeEndpoint(e.target.value)}
          placeholder={
            ["pi", "aider", "codex", "hermes", "antigravity", "copilot"].includes(runtimeProvider)
              ? "Executable path (blank = found on PATH)"
              : "Endpoint URL"
          }
          className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
          style={fieldStyle}
        />
        )}
        {!runtimeGateway && runtimeEndpoint.includes("<host>") && (
          <p className="text-xs text-ink-soft">
            Replace <code>&lt;host&gt;</code> with your gateway's address (e.g. its Tailscale IP).
          </p>
        )}
        {runtimeProvider === "pi" && !runtimeGateway && (
          <>
            <input
              value={runtimeBaseUrl}
              onChange={(e) => setRuntimeBaseUrl(e.target.value)}
              placeholder="Ollama base URL — e.g. http://localhost:11434"
              className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
              style={fieldStyle}
            />
            <p className="text-xs text-ink-soft">
              Points <code>pi</code> at an OpenAI-compatible backend (a local Ollama). Leave blank
              to use your own <code>pi</code> provider config / <code>pi login</code>.
            </p>
          </>
        )}
        <input
          value={runtimeModel}
          onChange={(e) => setRuntimeModel(e.target.value)}
          placeholder={
            runtimeGateway ? "Model the gateway serves" : runtimeProvider === "pi" ? "Model — e.g. qwen2.5-coder" : "Preferred model"
          }
          aria-label="Model"
          list={
            runtimeGateway
              ? gatewayModelList.length > 0
                ? "gateway-form-models"
                : undefined
              : runtimeProvider === "ollama"
                ? "ollama-form-models"
                : runtimeProvider
                  ? "runtime-form-models"
                  : undefined
          }
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={fieldStyle}
        />
        {runtimeGateway && gatewayModelList.length > 0 && (
          <datalist id="gateway-form-models" data-testid="gateway-form-models">
            {gatewayModelList.map((m) => (
              <option key={m} value={m} />
            ))}
          </datalist>
        )}
        {!runtimeGateway && runtimeProvider !== "ollama" && runtimeProvider !== "" && (
          <ModelFieldHelp
            listId="runtime-form-models"
            runtimeId={editingId}
            provider={runtimeProvider}
            endpoint={formEndpoint}
            model={runtimeModel}
          />
        )}
        {profile && (
          <p className="text-xs text-ink-soft" data-testid="model-profile-hint">
            <span className="font-semibold">
              {profile.roleLabel ? `Recommended for: ${profile.roleLabel}` : "No recommended role"}
            </span>
            {" · "}
            {profile.note}
            {!profile.tools && runtimeSupportsTools && " This family doesn't call tools well; consider turning tools off."}
          </p>
        )}
        {runtimeProvider === "ollama" && (
          <>
            <datalist id="ollama-form-models">
              {(formModels.data?.models ?? []).map((m) => (
                <option key={m.name} value={m.name}>
                  {[m.parameterSize, m.quantization].filter(Boolean).join(" · ")}
                </option>
              ))}
            </datalist>
            {formModels.data &&
              formModels.data.models.length > 0 &&
              runtimeModel.trim() !== "" &&
              !formModels.data.models.some((m) => m.name.replace(/:latest$/, "") === runtimeModel.trim().replace(/:latest$/, "")) && (
                <p className="text-xs" role="alert" style={{ color: "var(--ligus-danger-ink)" }}>
                  {runtimeModel.trim()} isn't installed on {formModels.data.host}, so it won't run. Pull it from the
                  runtime's Models panel first.
                </p>
              )}
            <p className="text-xs text-ink-soft">
              {formModels.data
                ? formModels.data.models.length
                  ? `${formModels.data.models.length} model${formModels.data.models.length === 1 ? "" : "s"} installed on ${formModels.data.host} — pick one above, or add the runtime and pull more from its Models panel.`
                  : `No models installed on ${formModels.data.host} yet — add the runtime, then pull one from its Models panel.`
                : formModels.isError
                  ? `Couldn't list models: ${errMsg(formModels.error)}`
                  : formModels.isFetching
                    ? "Looking up installed models…"
                    : (
                      <>
                        Type the server's address to see its models, or{" "}
                        <button type="button" className="underline" onClick={() => setLookForLocalOllama(true)}>
                          look for Ollama on this machine
                        </button>
                        .
                      </>
                    )}
            </p>
          </>
        )}
        {agentToolProviders.includes(runtimeProvider) && runtimeSupportsTools && (
          <>
            {(endpointIsRemote || runtimeAllowRemoteFileTools) && (
              <div className="space-y-1">
                <div className="flex items-center gap-2 text-sm text-ink-soft">
                  <Switch
                    on={runtimeAllowRemoteFileTools}
                    onChange={setRuntimeAllowRemoteFileTools}
                    label="Allow workspace file tools for this endpoint"
                  />
                  <span>Allow workspace file tools for this endpoint</span>
                </div>
                <p className="text-xs" style={{ color: "var(--ligus-warn-ink)" }}>
                  This endpoint isn't on this machine or a private network (LAN, Tailscale). With this
                  on, Ligus's file tools send the contents of files the model reads from your workspace
                  to it. Off: the model here only gets your MCP tools.
                </p>
              </div>
            )}
          </>
        )}
        <FormDisclosure label="More options" open={moreOptions} onToggle={() => setMoreOptions((v) => !v)}>
          <div className="space-y-2">
            <input
              value={runtimeId}
              onChange={(e) => setRuntimeId(e.target.value)}
              placeholder="ID (optional)"
              aria-label="Runtime ID"
              readOnly={editingId !== null}
              title={editingId !== null ? "The id is fixed while editing" : undefined}
              className="w-full rounded-xl border px-3 py-2 font-mono text-sm read-only:opacity-60"
              style={fieldStyle}
            />
        <input
          value={runtimeContextWindow}
          onChange={(e) => setRuntimeContextWindow(e.target.value.replace(/[^0-9]/g, ""))}
          inputMode="numeric"
          placeholder="Context window in tokens (optional — e.g. 32768)"
          className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
          style={fieldStyle}
        />
        <p className="text-xs text-ink-soft">
          Overrides the window the context planner budgets against. Useful for Ollama/custom
          models whose window can't be inferred from the model name.
          {runtimeProvider === "ollama" && " Ollama is told to allocate this window (num_ctx) on every request."}
          {runtimeProvider === "custom" && " Left blank, a llama.cpp server's own per-slot window (/props) is used when it reports one."}
        </p>
        {runtimeProvider === "ollama" && (
          <input
            value={runtimeKeepAlive}
            onChange={(e) => setRuntimeKeepAlive(e.target.value)}
            placeholder="Keep model loaded — e.g. 5m, 1h, or -1 for always (optional)"
            className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
            style={fieldStyle}
          />
        )}
        {thinkProviders.includes(runtimeProvider) && (
          <div className="flex items-center gap-2 text-sm text-ink-soft">
            <Switch on={runtimeThink} onChange={setRuntimeThink} label="Let the model think" />
            <span>
              Let the model think (reasoning models; slower, shown live, never in the transcript)
              {runtimeProvider !== "ollama" &&
                ". Sent to a llama.cpp server as enable_thinking; gpt-oss always reasons and gets low effort when this is off"}
            </span>
          </div>
        )}
        {agentToolProviders.includes(runtimeProvider) && runtimeSupportsTools && (
          <>
            <input
              value={runtimeMaxToolRounds}
              onChange={(e) => setRuntimeMaxToolRounds(e.target.value.replace(/[^0-9]/g, ""))}
              inputMode="numeric"
              placeholder="Max tool rounds per turn (optional — default 24)"
              className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
              style={fieldStyle}
            />
            <p className="text-xs text-ink-soft">
              How many times the model may call tools before it must answer. Tools also stop being
              offered when the conversation nears the context window.
            </p>
          </>
        )}
        <div className="flex items-center gap-2 text-sm text-ink-soft">
          <Switch on={runtimeSupportsTools} onChange={setRuntimeSupportsTools} label="Supports tools" />
          <span>
            {httpProviders.includes(runtimeProvider) && !agentToolProviders.includes(runtimeProvider) ? (
              <>
                Supports tools (not used for hosted APIs: Ligus's MCP and file tools only run against
                local or self-hosted servers — Ollama, Custom, Ligus daemon — so workspace files never
                go to a third-party cloud)
              </>
            ) : (
              <>
                Supports tools (offer your enabled MCP tools
                {agentToolProviders.includes(runtimeProvider) ? " and Ligus's built-in file tools" : ""} to this model
                {runtimeProvider === "ollama"
                  ? " when the server's probe says it can call them"
                  : ["custom", "ligus-daemon"].includes(runtimeProvider)
                    ? " over the OpenAI tools API; a server that rejects tools falls back to plain chat"
                    : ""}
                )
              </>
            )}
          </span>
        </div>
        <div className="flex items-center gap-2 text-sm text-ink-soft">
          <Switch on={runtimeSupportsEmbeddings} onChange={setRuntimeSupportsEmbeddings} label="Supports embeddings" />
          <span>Supports embeddings</span>
        </div>
          </div>
        </FormDisclosure>
      </div>
      </Sheet>
      )}
    </Section>
  );
}

function fmtBytes(n: number): string {
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)} GB`;
  if (n >= 1e6) return `${Math.round(n / 1e6)} MB`;
  return `${n} B`;
}

function fmtCtx(n: number): string {
  return n >= 1024 ? `${Math.round(n / 1024)}k ctx` : `${n} ctx`;
}

/// One Ollama server's model catalogue, on the runtime's row: what's installed
/// (and loaded, with the window it was loaded at), pull a new model or update an
/// installed one with live progress, remove one, or point this runtime at a
/// listed model. Ollama has no API for browsing its remote library, so that is
/// a link.
function OllamaModelsPanel({
  runtime,
  switching,
  onUse,
}: {
  runtime: RuntimeSummaryDto;
  switching: boolean;
  onUse: (model: string) => void;
}) {
  const qc = useQueryClient();
  const server = { runtimeId: runtime.id };
  const list = useQuery({
    queryKey: ["ollama-models", runtime.id],
    queryFn: () => listOllamaModels(server),
    retry: false,
  });
  const base = list.data?.base;
  const [pullName, setPullName] = useState("");
  // model → its latest progress event. Terminal events (done/canceled) are
  // removed once the list has refreshed; errors stay until dismissed.
  const [pulls, setPulls] = useState<Record<string, OllamaPullEvent>>({});

  // A pull started before this panel opened (or from another panel on the same
  // server) shows as in progress until its next event arrives.
  useEffect(() => {
    if (!list.data) return;
    const { base, pulling } = list.data;
    setPulls((p) => {
      const next = { ...p };
      for (const m of pulling) {
        if (!next[m]) next[m] = { base, model: m, status: "pulling…", done: false, canceled: false };
      }
      return next;
    });
  }, [list.data]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void onOllamaPull((e) => {
      if (base && e.base !== base) return;
      if (e.done || e.canceled) {
        setPulls((p) => {
          const { [e.model]: _gone, ...rest } = p;
          return rest;
        });
        if (e.done) {
          toast.success(`Pulled ${e.model}.`);
          void qc.invalidateQueries({ queryKey: ["ollama-models"] });
        }
        return;
      }
      setPulls((p) => ({ ...p, [e.model]: e }));
    }).then((u) => {
      if (disposed) u();
      else unlisten = u;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [base, qc]);

  async function startPull(name: string) {
    const model = name.trim();
    if (!model) return;
    try {
      // The backend emits "starting" before the command resolves, so the row
      // comes from the event stream. Adding it here too could resurrect a pull
      // whose `done` already arrived (updating an up-to-date tag takes one
      // round trip), leaving a row that never clears.
      await pullOllamaModel(server, model);
      setPullName("");
    } catch (e) {
      toast.error(errMsg(e));
    }
  }
  async function cancel(model: string) {
    try {
      await cancelOllamaPull(server, model);
    } catch (e) {
      toast.error(errMsg(e));
    }
  }
  const remove = useMutation({
    mutationFn: (model: string) => deleteOllamaModel(server, model),
    onSuccess: (_d, model) => {
      toast.success(`Removed ${model} from ${list.data?.host ?? "the server"}.`);
      void qc.invalidateQueries({ queryKey: ["ollama-models"] });
    },
    onError: (e) => toast.error(`Couldn't remove model: ${errMsg(e)}`),
  });

  const models = list.data?.models ?? [];
  // Pulls for models not (yet) in the installed list get their own rows.
  const newPulls = Object.values(pulls).filter((p) => !models.some((m) => m.name === p.model));

  const renderPull = (p: OllamaPullEvent) => {
    const pct = p.total && p.completed !== undefined ? Math.min(100, Math.round((p.completed / p.total) * 100)) : null;
    return (
      <div className="mt-1 space-y-1">
        <div className="flex items-center justify-between gap-2 text-xs">
          <span style={{ color: p.error ? "var(--ligus-danger-ink)" : undefined }} className={p.error ? "" : "opacity-70"}>
            {p.error
              ? `✕ ${p.error}`
              : pct !== null
                ? `${p.status.startsWith("pulling") ? "Downloading" : p.status} · ${fmtBytes(p.completed!)} / ${fmtBytes(p.total!)} (${pct}%)`
                : p.status}
          </span>
          {p.error ? (
            <div className="flex items-center gap-2">
              <button onClick={() => void startPull(p.model)} className="text-xs hover:opacity-80">
                Retry
              </button>
              <button
                onClick={() =>
                  setPulls((all) => {
                    const { [p.model]: _gone, ...rest } = all;
                    return rest;
                  })
                }
                className="text-xs hover:opacity-80"
              >
                Dismiss
              </button>
            </div>
          ) : (
            <button onClick={() => void cancel(p.model)} className="text-xs hover:opacity-80" style={{ color: "var(--ligus-danger-ink)" }}>
              Cancel
            </button>
          )}
        </div>
        {!p.error && (
          <div className="h-1.5 w-full overflow-hidden rounded-full" style={{ background: "var(--ligus-line)" }} role="progressbar" aria-valuenow={pct ?? undefined} aria-valuemin={0} aria-valuemax={100}>
            <div
              className={`h-full rounded-full transition-[width] ${pct === null ? "animate-pulse" : ""}`}
              style={{ width: `${pct ?? 15}%`, background: "var(--ligus-accent-cool)" }}
            />
          </div>
        )}
      </div>
    );
  };

  return (
    <div className="mt-2 space-y-2 border-l pl-3" style={{ borderColor: "var(--ligus-line)" }}>
      <div className="flex items-center justify-between gap-2">
        <div className="text-xs font-semibold uppercase tracking-wide text-ink-soft">
          Models on {list.data?.host ?? "this server"}
        </div>
        <div className="flex items-center gap-3">
          <button onClick={() => void list.refetch()} disabled={list.isFetching} className="text-xs hover:opacity-80 disabled:opacity-50">
            {list.isFetching ? "Refreshing…" : "Refresh"}
          </button>
          <button
            onClick={() => void openExternal("https://ollama.com/library")}
            className="text-xs hover:opacity-80"
            title="Browse models you can pull (opens ollama.com)"
          >
            Browse library ↗
          </button>
        </div>
      </div>
      {list.isError && (
        <p className="text-xs" style={{ color: "var(--ligus-danger-ink)" }}>
          ✕ {errMsg(list.error)}
        </p>
      )}
      {list.isLoading && <p className="text-xs text-ink-soft">Loading…</p>}
      {list.data && models.length === 0 && newPulls.length === 0 && (
        <p className="text-xs text-ink-soft">No models installed yet.</p>
      )}
      <div className="space-y-1">
        {models.map((m) => {
          const current = m.name === runtime.model || `${m.name}` === `${runtime.model}:latest`;
          const pull = pulls[m.name];
          const meta = [m.parameterSize, m.quantization, m.sizeBytes ? fmtBytes(m.sizeBytes) : ""].filter(Boolean).join(" · ");
          return (
            <OllamaModelLine
              key={m.name}
              name={m.name}
              current={current}
              loaded={m.loaded}
              loadedTip={m.expiresAt ? `Loaded in memory; unloads at ${new Date(m.expiresAt).toLocaleTimeString()}` : "Loaded in memory"}
              loadedLabel={`loaded${m.loadedContextLength ? ` · ${fmtCtx(m.loadedContextLength)}` : ""}`}
              meta={meta}
              actions={
                pull
                  ? []
                  : [
                      ...(!current
                        ? [{ label: "Use", onSelect: () => onUse(m.name), disabled: switching, title: "Point this runtime at this model" }]
                        : []),
                      { label: "Update", onSelect: () => void startPull(m.name), title: "Re-pull this tag: fetches only what changed upstream" },
                      {
                        label: "Remove",
                        danger: true,
                        disabled: remove.isPending,
                        onSelect: () =>
                          confirmThen(
                            `Remove ${m.name} from ${list.data?.host ?? "the server"}? Runtimes using it will fail until it's pulled again.`,
                            () => remove.mutate(m.name),
                          ),
                      },
                    ]
              }
            >
              {pull && renderPull(pull)}
            </OllamaModelLine>
          );
        })}
        {newPulls.map((p) => (
          <div key={p.model} className="border-t px-1 py-1.5 first:border-t-0" style={{ borderColor: "var(--ligus-line)" }}>
            <span className="font-mono text-sm">{p.model}</span>
            {renderPull(p)}
          </div>
        ))}
      </div>
      <form
        className="flex items-center gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          void startPull(pullName);
        }}
      >
        <input
          value={pullName}
          onChange={(e) => setPullName(e.target.value)}
          placeholder="Pull a model — e.g. qwen3.5:latest"
          className="min-w-0 flex-1 rounded-xl border px-3 py-1.5 font-mono text-sm"
          style={fieldStyle}
          spellCheck={false}
        />
        <Button type="submit" variant="primary" size="sm" disabled={!pullName.trim()}>
          Pull
        </Button>
      </form>
      <p className="text-[11.5px] text-ink-soft" data-tip="A cancelled pull keeps what it downloaded and resumes on the next attempt.">
        Pulls run on the server and resume if cancelled.
      </p>
    </div>
  );
}

// Lets the user pick which model their local Claude Code CLI uses (`--model`).
// Auto-saves on change; applies to the next turn (no restart).
const CLAUDE_MODELS = [
  { value: "", label: "Default (CLI's own setting)" },
  { value: "sonnet", label: "Sonnet" },
  { value: "opus", label: "Opus" },
  { value: "haiku", label: "Haiku" },
];

// Compact model selector shown inline on the Claude Code runtime row. Claude
// Code isn't a config runtime (it's the local `claude` CLI), so its "model" is
// the `--model` alias in settings, not something the generic Edit form touches —
// this edits it in place where the runtime is listed.
function ClaudeCodeRowModel() {
  const qc = useQueryClient();
  const model = useQuery({ queryKey: ["claude-model"], queryFn: getClaudeCodeModel });
  const models = useQuery({ queryKey: ["claude-model-options"], queryFn: listClaudeCodeModels });
  const [custom, setCustom] = useState(false);
  const save = useMutation({
    mutationFn: setClaudeCodeModel,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["claude-model"] });
      qc.invalidateQueries({ queryKey: ["claude-model-options"] });
      qc.invalidateQueries({ queryKey: ["runtimes"] });
    },
    onError: (e) => toast.error(`Couldn't set model: ${errMsg(e)}`),
  });
  const options = models.data?.length ? models.data : CLAUDE_MODELS;
  const current = model.data ?? "";
  const known = options.some((m) => m.value === current);
  const showCustom = custom || (!known && current !== "");
  return (
    <div className="mt-2 flex items-center gap-2">
      <select
        value={showCustom ? "__custom__" : current}
        onChange={(e) => {
          if (e.target.value === "__custom__") setCustom(true);
          else {
            setCustom(false);
            save.mutate(e.target.value);
          }
        }}
        className="rounded-lg border px-2 py-1 text-xs"
        style={fieldStyle}
        aria-label="Claude Code model"
      >
        {options.map((m) => (
          <option key={m.value || "default"} value={m.value}>{`Model: ${m.label}`}</option>
        ))}
        <option value="__custom__">Model: Custom…</option>
      </select>
      {showCustom && (
        <input
          defaultValue={known ? "" : current}
          onBlur={(e) => save.mutate(e.target.value.trim())}
          placeholder="alias or id (e.g. fable)"
          className="rounded-lg border px-2 py-1 text-xs"
          style={fieldStyle}
          aria-label="Custom Claude Code model"
        />
      )}
    </div>
  );
}

/// Agents: one way to add one (the Add an agent sheet, where the model is
/// per agent and no runtime comes first).
function AgentsSection() {
  return (
    <Section
      title="Agents"
      info="Add an agent in one step: what answers, its model, and where it runs. The workspace's agents are in each chat's Tools pane."
      action={
        <Button size="sm" onClick={() => openSetup("agent")}>
          Add an agent
        </Button>
      }
    >
      {null}
    </Section>
  );
}

function TemplatesSection() {
  const [items, setItems] = useState<PromptTemplate[]>(() => loadTemplates());
  const [showForm, setShowForm] = useState(false);
  const [name, setName] = useState("");
  const [body, setBody] = useState("");

  function add() {
    if (!name.trim() || !body.trim()) return;
    setItems(addTemplate(name, body));
    setName("");
    setBody("");
    setShowForm(false);
  }

  return (
    <Section
      title="Prompt templates"
      info="Reusable prompts you can drop into the composer with /."
      action={
        <Button size="sm" onClick={() => setShowForm(true)}>
          Add a template
        </Button>
      }
    >
      {items.length === 0 && <p className="px-1 text-[13px] text-ink-soft">No templates yet</p>}
      <RowList label="Prompt templates">
        {items.map((t) => (
          <ListRow
            key={t.id}
            title={t.name}
            meta={t.body}
            tip={t.body}
            actions={[
              {
                label: "Delete",
                danger: true,
                onSelect: () => confirmThen(`Delete template "${t.name}"?`, () => setItems(removeTemplate(t.id))),
              },
            ]}
          />
        ))}
      </RowList>
      {showForm && (
      <Sheet
        title="Add a template"
        side="center"
        width={460}
        onClose={() => setShowForm(false)}
        footer={
          <>
            <Button onClick={() => setShowForm(false)}>Cancel</Button>
            <Button variant="primary" disabled={!name.trim() || !body.trim()} onClick={add}>
              Add template
            </Button>
          </>
        }
      >
      <div className="space-y-2">
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          aria-label="Template name"
          placeholder="Template name (e.g. Bug report)"
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={fieldStyle}
        />
        <textarea
          value={body}
          onChange={(e) => setBody(e.target.value)}
          placeholder="Prompt text…"
          rows={3}
          className="w-full resize-y rounded-xl border px-3 py-2 text-sm"
          style={fieldStyle}
        />
      </div>
      </Sheet>
      )}
    </Section>
  );
}

/// The "Quick tasks model": a runtime for side jobs like chat titles, so a slow
/// or reasoning chat model doesn't spend seconds naming a chat. Falls back to
/// the chat's runtime on any error. Compaction summaries never use it.
function QuickTasksSection() {
  const qc = useQueryClient();
  const runtimes = useQuery({ queryKey: ["runtimes"], queryFn: listRuntimes });
  const current = useQuery({ queryKey: ["utility-runtime"], queryFn: getUtilityRuntime });
  const save = useMutation({
    mutationFn: (id: string) => setUtilityRuntime(id || null),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["utility-runtime"] }),
    onError: (e) => toast.error(`Couldn't set quick tasks model: ${errMsg(e)}`),
  });
  const options = quickTaskOptions(runtimes.data ?? [], current.data);
  return (
    <Section
      title="Quick tasks model"
      info="Used for chat titles. A small non-reasoning model works best, e.g. lfm2:8b on CPU."
    >
      <select
        value={current.data ?? ""}
        onChange={(e) => save.mutate(e.target.value)}
        className={`w-full ${SELECT_CLASS}`}
        style={fieldStyle}
        aria-label="Quick tasks model"
      >
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
    </Section>
  );
}

/// Customize the instructions behind /summarize and /compact (and the automatic
/// overflow summarization, which reuses the /summarize one). Writes through on
/// blur (D18) — no Save button.
function ContextCommandsSection() {
  const qc = useQueryClient();
  const cmds = useQuery({ queryKey: ["context-commands"], queryFn: getContextCommands });
  const [summarize, setSummarize] = useState("");
  const [compact, setCompact] = useState("");
  const [editing, setEditing] = useState(false);

  useEffect(() => {
    if (cmds.data) {
      setSummarize(cmds.data.summarizePrompt);
      setCompact(cmds.data.compactPrompt);
    }
  }, [cmds.data]);

  async function commit(nextSummarize: string, nextCompact: string) {
    if (
      nextSummarize === (cmds.data?.summarizePrompt ?? "") &&
      nextCompact === (cmds.data?.compactPrompt ?? "")
    )
      return;
    try {
      await setContextCommands(nextSummarize, nextCompact);
      qc.invalidateQueries({ queryKey: ["context-commands"] });
    } catch (e) {
      toast.error(`Couldn't save context commands: ${errMsg(e)}`);
    }
  }

  const dflt = cmds.data?.defaultPrompt ?? "";
  const custom = (cmds.data?.summarizePrompt ?? "") !== "" || (cmds.data?.compactPrompt ?? "") !== "";
  const fieldClass = "w-full resize-none rounded-xl border px-3 py-2 text-sm leading-5";

  return (
    <Section
      title="Context commands"
      info="The instructions the model follows for /summarize and /compact. Blank = the built-in default (shown as the placeholder). The /summarize instruction also guides the automatic summarization when a long chat overflows the model's context window."
    >
      <p className="px-1 text-[11.5px] text-ink-soft">{custom ? "Custom instructions" : "Built-in instructions"}</p>
      <FormDisclosure label="Edit instructions" open={editing} onToggle={() => setEditing((v) => !v)}>
      <div className="space-y-2">
      <label className="block text-sm text-ink-soft">
        <code>/summarize</code> instruction
      </label>
      <textarea
        value={summarize}
        onChange={(e) => setSummarize(e.target.value)}
        onBlur={() => void commit(summarize, compact)}
        placeholder={dflt}
        rows={3}
        className={fieldClass}
        style={fieldStyle}
      />
      <label className="block text-sm text-ink-soft">
        <code>/compact</code> instruction
      </label>
      <textarea
        value={compact}
        onChange={(e) => setCompact(e.target.value)}
        onBlur={() => void commit(summarize, compact)}
        placeholder={dflt}
        rows={3}
        className={fieldClass}
        style={fieldStyle}
      />
      {(summarize.trim() !== "" || compact.trim() !== "") && (
        <button
          onClick={() => {
            setSummarize("");
            setCompact("");
            void commit("", "");
          }}
          className="text-xs underline underline-offset-2 opacity-60 hover:opacity-100"
        >
          Reset to defaults
        </button>
      )}
      </div>
      </FormDisclosure>
    </Section>
  );
}

const RETRIEVAL_DEFAULTS: RetrievalSettings = {
  embedUrl: "",
  embedModel: "",
  rerankUrl: "",
  rerankModel: "",
  topK: 5,
  candidates: 20,
};

type RetrievalTextKey = "embedUrl" | "embedModel" | "rerankUrl" | "rerankModel";
type RetrievalCountKey = "topK" | "candidates";

/// Vault retrieval: instead of pasting whole vaults into the system prompt,
/// embed them once and add only the passages that answer each message, at the
/// end of the prompt (keeps the system prompt stable for prefix caching).
/// Writes through on blur (D18), like the context commands.
function VaultRetrievalSection() {
  const qc = useQueryClient();
  const saved = useQuery({ queryKey: ["retrieval-settings"], queryFn: getRetrievalSettings });
  const [draft, setDraft] = useState<RetrievalSettings>(RETRIEVAL_DEFAULTS);
  const [testing, setTesting] = useState(false);
  const [more, setMore] = useState(false);

  useEffect(() => {
    if (saved.data) setDraft(saved.data);
  }, [saved.data]);

  async function commit(next: RetrievalSettings) {
    const cur = saved.data ?? RETRIEVAL_DEFAULTS;
    if (JSON.stringify(next) === JSON.stringify(cur)) return;
    try {
      await setRetrievalSettings(next);
      qc.invalidateQueries({ queryKey: ["retrieval-settings"] });
    } catch (e) {
      toast.error(`Couldn't save vault retrieval: ${errMsg(e)}`);
    }
  }

  async function runTest() {
    setTesting(true);
    try {
      toast.success(await testRetrievalSettings(draft));
    } catch (e) {
      toast.error(`Vault retrieval test failed: ${errMsg(e)}`);
    } finally {
      setTesting(false);
    }
  }

  const on = draft.embedUrl.trim() !== "" && draft.embedModel.trim() !== "";
  const text = (key: RetrievalTextKey, label: string, placeholder: string) => (
    <label className="block text-sm">
      <span className="text-ink-soft">{label}</span>
      <input
        value={draft[key]}
        onChange={(e) => setDraft((d) => ({ ...d, [key]: e.target.value }))}
        onBlur={() => void commit(draft)}
        placeholder={placeholder}
        spellCheck={false}
        className={`mt-1 w-full ${SELECT_CLASS}`}
        style={fieldStyle}
      />
    </label>
  );
  const count = (key: RetrievalCountKey, label: string, max: number) => (
    <label className="block text-sm">
      <span className="text-ink-soft">{label}</span>
      <input
        type="number"
        min={1}
        max={max}
        value={draft[key]}
        onChange={(e) =>
          setDraft((d) => ({ ...d, [key]: Math.max(0, Math.floor(Number(e.target.value) || 0)) }))
        }
        onBlur={() => void commit(draft)}
        className={`mt-1 w-full ${SELECT_CLASS}`}
        style={fieldStyle}
      />
    </label>
  );

  return (
    <Section
      title="Vault retrieval"
      info="Optional. With an embeddings endpoint set, chats search their vaults and add only the passages that answer each message, instead of pasting whole documents into the system prompt. Any OpenAI-compatible server works, e.g. a llama.cpp gateway. If retrieval fails, the chat falls back to pasting."
    >
      <div className="grid gap-3 sm:grid-cols-2">
        {text("embedUrl", "Embeddings URL", "http://host:8080/v1")}
        {text("embedModel", "Embeddings model", "embeddinggemma-300m")}
      </div>
      <FormDisclosure label="More options" open={more} onToggle={() => setMore((v) => !v)}>
        <div className="grid gap-3 sm:grid-cols-2">
          {text("rerankUrl", "Rerank URL (optional)", "http://host:8080/v1")}
          {text("rerankModel", "Rerank model", "bge-reranker-v2-m3")}
          {count("topK", "Passages per message", 20)}
          {count("candidates", "Candidates to rerank", 100)}
        </div>
      </FormDisclosure>
      <div className="flex items-center gap-3">
        <Button disabled={!on || testing} onClick={() => void runTest()}>
          {testing ? "Testing…" : "Test"}
        </Button>
        <span className="text-xs text-ink-soft">
          {on ? "On: vaults are searched for each message." : "Off: vaults are pasted into the system prompt."}
        </span>
      </div>
    </Section>
  );
}

/// One installed Ollama model: name, current/loaded tags and size on one line;
/// Use, Update and Remove live in the row menu.
function OllamaModelLine({
  name,
  current,
  loaded,
  loadedTip,
  loadedLabel,
  meta,
  actions,
  children,
}: {
  name: string;
  current: boolean;
  loaded: boolean;
  loadedTip: string;
  loadedLabel: string;
  meta: string;
  actions: RowAction[];
  children?: ReactNode;
}) {
  const rm = useRowMenu(`Actions for ${name}`, actions);
  return (
    <div className="ligus-listrow border-t px-1 first:border-t-0" style={{ borderColor: "var(--ligus-line)" }} {...rm.rowProps}>
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 items-baseline gap-2">
          <span className="truncate font-mono text-sm">{name}</span>
          {current && (
            <span className="text-[11px] font-semibold" style={{ color: "var(--ligus-accent-ink)" }} data-tip="This runtime uses it">
              current
            </span>
          )}
          {loaded && (
            <span className="text-[11px]" style={{ color: "var(--ligus-success-ink)" }} data-tip={loadedTip}>
              {loadedLabel}
            </span>
          )}
          {meta && <span className="truncate text-xs text-ink-soft">{meta}</span>}
        </div>
        {rm.button}
      </div>
      {children}
      {rm.menu}
    </div>
  );
}
