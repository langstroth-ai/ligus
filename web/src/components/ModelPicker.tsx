// The model behind a chat, picked from the route pill's popover: the second
// step after a runtime (Default, the harness's list, Other…). The choice is
// kept on this device, per chat and per runtime, and applies only to the
// chat's untagged turns this device runs (docs/design/chat-model-picker.md).
import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  getChatModel,
  listRuntimeModels,
  type ModelProblemDto,
  type RuntimeModelsDto,
  type RuntimeSummaryDto,
} from "@/lib/ipc";
import { IconCheck, IconChevronLeft, IconRefresh } from "@/lib/icons";
import { tip } from "@/components/Tooltip";

/// The chat's model choice on this device (null = the runtime's own).
export function useChatModel(sessionId: string) {
  return useQuery({
    queryKey: ["chat-model", sessionId],
    queryFn: () => getChatModel(sessionId),
    enabled: sessionId.length > 0,
  });
}

/// The model a chat's runtime runs here: the choice for that runtime, else
/// the runtime's own.
export function effectiveModel(runtime: RuntimeSummaryDto, chosen: string | null | undefined): string {
  return chosen?.trim() || runtime.model.trim();
}

/// "provider / model" with the chat's choice in place of the runtime's model.
export function runtimeLabelWithModel(runtime: RuntimeSummaryDto, chosen: string | null | undefined): string {
  const provider = runtime.provider.trim();
  const model = effectiveModel(runtime, chosen);
  if (provider && model) return `${provider} / ${model}`;
  if (provider) return provider;
  if (model) return model;
  return runtime.label.trim() || runtime.name.trim() || "runtime";
}

const HARNESS: Record<string, string> = {
  "claude-code": "Claude Code",
  copilot: "Copilot",
  codex: "Codex",
  antigravity: "Antigravity",
  ollama: "Ollama",
  anthropic: "Anthropic",
  openai: "OpenAI",
  openrouter: "OpenRouter",
};

/// The harness's name in a sentence ("this Copilot account").
export function harnessName(runtime: RuntimeSummaryDto | undefined): string {
  if (!runtime) return "runtime";
  return HARNESS[runtime.provider.trim()] ?? (runtime.label.trim() || runtime.name.trim() || "runtime");
}

/// The one line shown when a chat's model won't run.
export function modelProblemText(p: Pick<ModelProblemDto, "model" | "reason">, runtime: RuntimeSummaryDto | undefined): string {
  const h = harnessName(runtime);
  if (p.reason === "notListed") return `${p.model} isn't one of the models ${h} lists.`;
  const provider = runtime?.provider ?? "";
  if (["claude-code", "copilot", "codex", "antigravity"].includes(provider)) return `${p.model} isn't available to this ${h} account.`;
  if (provider === "ollama") return `${p.model} isn't on this Ollama server.`;
  return `${p.model} isn't available on ${h}.`;
}

/// A failed send whose error says the model isn't available (the same
/// phrases the backend recognizes), so the chat shows its one line instead
/// of a toast.
export function looksModelUnavailable(message: string): boolean {
  const t = message.toLowerCase();
  if (/temporarily|overloaded|high demand|try again/.test(t)) return false;
  return (
    t.includes("model_not_found") ||
    /unknown model|invalid model|unsupported model|no such model/.test(t) ||
    /model.{0,100}(is not available|not available (to|for)|is not supported|not found|does not exist)/.test(t)
  );
}

/// Arrow keys move between the popover's items; Home/End jump.
export function popoverArrowKeys(e: KeyboardEvent<HTMLElement>) {
  if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
  const items = Array.from(e.currentTarget.querySelectorAll<HTMLElement>("[data-pop-item]")).filter(
    (el) => !(el as HTMLButtonElement).disabled,
  );
  if (items.length === 0) return;
  e.preventDefault();
  const at = items.indexOf(document.activeElement as HTMLElement);
  const next =
    e.key === "Home"
      ? 0
      : e.key === "End"
        ? items.length - 1
        : e.key === "ArrowDown"
          ? (at + 1) % items.length
          : (at - 1 + items.length) % items.length;
  items[next]?.focus();
}

type Row = { id: string; hint: string | null; unavailable: boolean };

/// The rows of the list: the harness's models, with the current choice kept
/// visible even when the harness doesn't list it, filtered by `query`.
export function modelRows(list: RuntimeModelsDto | undefined, chosen: string | null, query: string): Row[] {
  const bad = new Set((list?.unavailable ?? []).map((m) => m.toLowerCase()));
  const rows: Row[] = (list?.models ?? []).map((m) => ({
    id: m.id,
    hint: bad.has(m.id.toLowerCase()) ? "unavailable here" : m.label && m.label !== m.id ? m.label : null,
    unavailable: bad.has(m.id.toLowerCase()),
  }));
  if (chosen && !rows.some((r) => r.id.toLowerCase() === chosen.toLowerCase())) {
    rows.unshift({
      id: chosen,
      hint: bad.has(chosen.toLowerCase()) ? "unavailable here" : list?.complete ? "not on the list" : "typed",
      unavailable: bad.has(chosen.toLowerCase()),
    });
  }
  const q = query.trim().toLowerCase();
  return q ? rows.filter((r) => r.id.toLowerCase().includes(q) || (r.hint ?? "").toLowerCase().includes(q)) : rows;
}

/// The second step: the models for `runtime`. `onPick(null)` = Default.
export function ModelStep({
  runtime,
  chosen,
  appliesHere,
  onBack,
  onPick,
}: {
  runtime: RuntimeSummaryDto;
  chosen: string | null;
  appliesHere: boolean;
  onBack: () => void;
  onPick: (model: string | null) => void;
}) {
  const qc = useQueryClient();
  const key = ["runtime-models", runtime.id];
  const list = useQuery({
    queryKey: key,
    queryFn: () => listRuntimeModels({ runtimeId: runtime.id }),
    staleTime: 60_000,
  });
  const [refreshing, setRefreshing] = useState(false);
  const [query, setQuery] = useState("");
  const [other, setOther] = useState<string | null>(null);
  const boxRef = useRef<HTMLDivElement>(null);
  const rows = useMemo(() => modelRows(list.data, chosen, query), [list.data, chosen, query]);
  const showFilter = (list.data?.models.length ?? 0) > 8;

  // Focus the current choice (else Default) when the step opens.
  useEffect(() => {
    const box = boxRef.current;
    if (!box) return;
    const sel = box.querySelector<HTMLElement>("[data-pop-item][aria-checked='true']") ?? box.querySelector<HTMLElement>("[data-pop-item]");
    sel?.focus();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [list.isSuccess]);

  async function refresh() {
    setRefreshing(true);
    try {
      qc.setQueryData(key, await listRuntimeModels({ runtimeId: runtime.id, refresh: true }));
    } catch {
      void list.refetch();
    } finally {
      setRefreshing(false);
    }
  }

  const typed = (other ?? "").trim();
  const typedBlocked =
    typed.length > 0 &&
    Boolean(list.data?.complete) &&
    (list.data?.models.length ?? 0) > 0 &&
    !list.data!.models.some((m) => m.id.toLowerCase() === typed.toLowerCase() || `${m.id}:latest`.toLowerCase() === typed.toLowerCase() || m.id.toLowerCase() === `${typed}:latest`.toLowerCase());

  const item = (id: string | null, title: string, hint: string | null, unavailable = false) => {
    const selected = (id ?? null) === (chosen ?? null) || (id !== null && chosen !== null && id.toLowerCase() === chosen.toLowerCase());
    return (
      <button
        key={id ?? "default"}
        type="button"
        role="menuitemradio"
        aria-checked={selected}
        data-pop-item
        className={`pop-i${selected ? " sel" : ""}`}
        onMouseDown={(e) => e.preventDefault()}
        onClick={() => onPick(id)}
      >
        <span className="pop-t">
          <b style={unavailable ? { textDecoration: "line-through", opacity: 0.7 } : undefined}>{title}</b>
          {hint && <span>{hint}</span>}
        </span>
        {selected && <IconCheck size={12} />}
      </button>
    );
  };

  return (
    <div
      ref={boxRef}
      role="menu"
      aria-label={`Model for ${runtime.label || runtime.name}`}
      data-testid="model-step"
      onKeyDown={(e) => {
        if (e.key === "ArrowLeft" && other === null && (e.target as HTMLElement).tagName !== "INPUT") {
          e.preventDefault();
          onBack();
          return;
        }
        popoverArrowKeys(e);
      }}
    >
      <div className="pop-h" style={{ display: "flex", alignItems: "center", gap: 6 }}>
        <button
          type="button"
          className="pop-back"
          aria-label="Back to runtimes"
          {...tip("Back to runtimes", "←")}
          onMouseDown={(e) => e.preventDefault()}
          onClick={onBack}
        >
          <IconChevronLeft size={11} />
        </button>
        <span style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          Model · {runtime.label || runtime.name}
        </span>
        <button
          type="button"
          className="pop-back"
          aria-label="List models again"
          data-tip="List models again"
          disabled={refreshing || list.isFetching}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => void refresh()}
        >
          <IconRefresh size={11} />
        </button>
      </div>
      {item(
        null,
        "Default",
        runtime.model.trim()
          ? (list.data?.unavailable ?? []).some((m) => m.toLowerCase() === runtime.model.trim().toLowerCase())
            ? `${runtime.model.trim()} · unavailable here`
            : runtime.model.trim()
          : "the harness's own choice",
      )}
      {showFilter && (
        <input
          className="pop-in"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Filter models"
          aria-label="Filter models"
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              boxRef.current?.querySelectorAll<HTMLElement>("[data-pop-item]")[1]?.focus();
            }
          }}
        />
      )}
      {list.isLoading && <div className="pop-s">Listing models…</div>}
      {rows.map((r) => item(r.id, r.id, r.hint, r.unavailable))}
      {list.data && !list.isLoading && rows.length === 0 && list.data.models.length > 0 && <div className="pop-s">No model matches.</div>}
      {other === null ? (
        <button
          type="button"
          data-pop-item
          className="pop-i"
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => setOther(query)}
        >
          <span className="pop-t">
            <b>Other…</b>
            <span>type a model name</span>
          </span>
        </button>
      ) : (
        <div style={{ padding: "4px 5px" }}>
          <input
            autoFocus
            className="pop-in"
            style={{ margin: 0 }}
            value={other}
            onChange={(e) => setOther(e.target.value)}
            placeholder="Model name, e.g. gpt-5.3-codex"
            aria-label="Model name"
            aria-invalid={typedBlocked}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                if (typed && !typedBlocked) onPick(typed);
              } else if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                setOther(null);
              }
            }}
          />
          <div className="pop-s" style={{ padding: "4px 4px 0" }}>
            {typedBlocked ? `Not on ${harnessName(runtime)}'s list, so it won't run. List again if it's new.` : "Enter to use it, Esc to cancel."}
          </div>
        </div>
      )}
      {list.data?.error && <div className="pop-n">Couldn't list models: {list.data.error}</div>}
      {list.isError && <div className="pop-n">Couldn't list models.</div>}
      {list.data?.note && !list.data.error && <div className="pop-n">{list.data.note}</div>}
      {!appliesHere && (
        <div className="pop-n">
          Untagged messages here are answered on another member's device, with its own model. Your choice applies when
          this device answers.
        </div>
      )}
    </div>
  );
}

/// Whether `model` would be refused by a complete list (Ollama's implicit
/// `:latest` counts as listed). Incomplete lists never refuse.
export function refusedByList(list: RuntimeModelsDto | undefined, model: string): boolean {
  const m = model.trim().toLowerCase();
  if (!m || !list?.complete || list.models.length === 0) return false;
  const norm = (s: string) => s.toLowerCase().replace(/:latest$/, "");
  return !list.models.some((o) => norm(o.id) === norm(m));
}

const LISTS_ON_ITS_OWN = new Set(["claude-code", "copilot", "codex", "antigravity"]);

/// Settings' runtime model field: the harness's models as completions
/// (`<datalist id={listId}>`) and one quiet line saying where they came
/// from, or that the typed model won't run. Subscription CLIs are listed at
/// once (a local read-only command); an HTTP provider only on request, and
/// only for the provider and endpoint it was asked for: editing the endpoint
/// afterwards asks again rather than sending the stored key to each value
/// typed.
export function ModelFieldHelp({
  listId,
  runtimeId,
  provider,
  endpoint,
  model,
}: {
  listId: string;
  runtimeId?: string | null;
  provider: string;
  endpoint?: string;
  model: string;
}) {
  const target = `${provider}|${endpoint ?? ""}`;
  const [askedFor, setAskedFor] = useState<string | null>(null);
  const asked = askedFor === target;
  const auto = LISTS_ON_ITS_OWN.has(provider);
  const list = useQuery({
    queryKey: runtimeId ? ["runtime-models", runtimeId] : ["runtime-models", "form", provider, endpoint ?? ""],
    queryFn: () => (runtimeId ? listRuntimeModels({ runtimeId }) : listRuntimeModels({ provider, endpoint: endpoint ?? "" })),
    enabled: provider !== "" && (auto || asked || Boolean(runtimeId)),
    retry: false,
    refetchOnWindowFocus: false,
    staleTime: 60_000,
  });
  const d = list.data;
  const refused = refusedByList(d, model);
  return (
    <>
      <datalist id={listId}>
        {(d?.models ?? []).map((m) => (
          <option key={m.id} value={m.id}>
            {m.label ?? ""}
          </option>
        ))}
      </datalist>
      <p className="text-xs text-ink-soft" data-testid="model-field-help" role={refused ? "alert" : undefined}>
        {refused ? (
          <span style={{ color: "var(--ligus-danger-ink)" }}>{model.trim()} isn't one of its models, so it won't run.</span>
        ) : d?.error ? (
          `Couldn't list models: ${d.error}`
        ) : d && d.models.length > 0 ? (
          `${d.models.length} model${d.models.length === 1 ? "" : "s"} to pick from${d.note ? `. ${d.note}` : "."}`
        ) : d?.note ? (
          d.note
        ) : list.isFetching ? (
          "Listing models…"
        ) : !auto && !asked && !runtimeId ? (
          <button type="button" className="underline" onClick={() => setAskedFor(target)}>
            List this provider's models
          </button>
        ) : null}
      </p>
    </>
  );
}
