import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  getEgressPresets,
  setChatEgressPresets,
  setWorkspaceEgressPresets,
  type EgressPresetDto,
  type EgressPresetsDto,
} from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";
import { Badge, ErrorState, LoadingState, Section, SelectField, Switch } from "@/components/ui";

/// What a preset means for the people who switch it on.
const NOTE: Record<string, string> = {
  "package-registries": "npm, crates.io, PyPI and GitHub, so agents and their commands can install dependencies.",
  "claude-connectors":
    "Claude Code turns only: lets the agent use your claude.ai connectors, which means it can reach every service you connected there (Drive, Gmail and so on). Telemetry stays blocked.",
};

/// The chat-level choice for one preset: follow the workspace, or force it.
export type ChatChoice = "inherit" | "on" | "off";

export function chatChoice(d: EgressPresetsDto, name: string): ChatChoice {
  if (d.chat == null) return "inherit";
  return d.chat.includes(name) ? "on" : "off";
}

/// The chat's preset list after setting `name` to `choice` (`null` = the
/// chat follows the workspace again).
export function nextChatPresets(d: EgressPresetsDto, name: string, choice: ChatChoice): string[] | null {
  const others = (d.chat ?? d.workspace).filter((n) => n !== name);
  if (choice === "inherit") {
    if (d.chat == null) return null;
    // The chat keeps its other choices; this one takes the workspace's.
    const next = d.workspace.includes(name) ? [...others, name] : others;
    const same = next.length === d.workspace.length && next.every((n) => d.workspace.includes(n));
    return same ? null : next;
  }
  return choice === "on" ? [...others, name] : others;
}

/// Settings → Guards → Network presets: opt-in egress allow-list additions,
/// per workspace and per chat, on this device only. Off by default; a
/// lockdown or dearth mode overrides them.
export function EgressPresetsPanel({ chat }: { chat?: { id: string; title: string } | null }) {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["egress-presets", chat?.id ?? null], queryFn: () => getEgressPresets(chat?.id ?? null) });
  const refresh = () => qc.invalidateQueries({ queryKey: ["egress-presets"] });
  const ws = useMutation({
    mutationFn: (presets: string[]) => setWorkspaceEgressPresets(presets),
    onSuccess: refresh,
    onError: (e) => toast.error(errMsg(e)),
  });
  const ch = useMutation({
    mutationFn: (presets: string[] | null) => setChatEgressPresets(chat!.id, presets),
    onSuccess: refresh,
    onError: (e) => toast.error(errMsg(e)),
  });
  const d = q.data;
  return (
    <Section
      title="Network presets"
      info="Agents reach only their model provider and the chat's MCP servers. A preset adds a named set of hosts to that allow-list. Presets are off by default, apply to turns that run on this device, and are ignored while a chat is locked down or the workspace is in dearth mode."
    >
      {q.isError && <ErrorState text="Couldn't load network presets." onRetry={() => void q.refetch()} />}
      {q.isPending && <LoadingState rows={1} label="Loading network presets" />}
      {d && (
        <ul className="ligus-rowlist" data-testid="egress-presets">
          {d.available.map((p) => (
            <PresetRow
              key={p.name}
              p={p}
              d={d}
              chat={chat ?? null}
              busy={ws.isPending || ch.isPending}
              onWorkspace={(on) => ws.mutate(on ? [...d.workspace.filter((n) => n !== p.name), p.name] : d.workspace.filter((n) => n !== p.name))}
              onChat={(choice) => ch.mutate(nextChatPresets(d, p.name, choice))}
            />
          ))}
        </ul>
      )}
    </Section>
  );
}

function PresetRow({
  p,
  d,
  chat,
  busy,
  onWorkspace,
  onChat,
}: {
  p: EgressPresetDto;
  d: EgressPresetsDto;
  chat: { id: string; title: string } | null;
  busy: boolean;
  onWorkspace: (on: boolean) => void;
  onChat: (c: ChatChoice) => void;
}) {
  const wsOn = d.workspace.includes(p.name);
  const viaHost = d.host.includes(p.name);
  return (
    <li className="ligus-listrow space-y-2 px-1 text-[13px]">
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-medium">{p.label}</span>
            <code className="text-[11px] text-ink-soft">{p.name}</code>
            {viaHost && <Badge tone="warn">on via LIGUS_SANDBOX_EGRESS_PRESETS</Badge>}
          </div>
          <p className="mt-0.5 text-xs leading-[1.5] text-ink-soft">{NOTE[p.name] ?? ""}</p>
          <p className="mt-0.5 break-words text-[11px] leading-[1.5] text-ink-faint">{p.hosts.join(", ")} (port 443)</p>
        </div>
        <label className="flex items-center gap-2 text-xs">
          <span>This workspace</span>
          <Switch on={wsOn} onChange={onWorkspace} disabled={busy} label={`${p.label} for this workspace`} />
        </label>
      </div>
      {chat && (
        <div className="flex flex-wrap items-center gap-2 text-xs">
          <span className="min-w-0 flex-1 truncate">This chat ({chat.title || "untitled"})</span>
          {d.restrictedBy && (
            <Badge tone="danger">{d.restrictedBy === "lockdown" ? "ignored: chat locked down" : "ignored: dearth mode"}</Badge>
          )}
          <div className="w-44">
            <SelectField value={chatChoice(d, p.name)} onChange={(v) => onChat(v as ChatChoice)} disabled={busy} ariaLabel={`${p.label} for this chat`}>
              <option value="inherit">Workspace setting ({wsOn ? "on" : "off"})</option>
              <option value="on">On for this chat</option>
              <option value="off">Off for this chat</option>
            </SelectField>
          </div>
        </div>
      )}
    </li>
  );
}
