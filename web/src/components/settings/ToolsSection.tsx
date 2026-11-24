import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  installMcpServer,
  addRemoteMcpServer,
  authorizeMcpServer,
  setMcpOauthClient,
  listMcpServers,
  inspectMcpServer,
  removeMcpServer,
  setMcpEnabled,
  setMcpBearerToken,
} from "@/lib/ipc";
import { Button, Field, FormDisclosure, ListRow, RowList, Section, Switch, fieldStyle } from "@/components/ui";
import type { RowAction } from "@/components/RowMenu";
import type { McpServerDto } from "@/lib/ipc";
import { IconPlus } from "@/lib/icons";
import { toast, errMsg } from "@/components/Toast";
import { promptDialog } from "@/components/Dialog";

/// Tools & MCP: install and configure MCP servers. Settings owns installing +
/// configuring; the chat Tools pane links here and only attaches/selects.
export function ToolsSection() {
  const qc = useQueryClient();
  const servers = useQuery({ queryKey: ["mcp"], queryFn: listMcpServers });
  const inspect = useMutation({
    mutationFn: (id: string) => inspectMcpServer(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["mcp"] }),
    onError: (e) => toast.error(`Couldn't list the server's tools: ${errMsg(e)}`),
  });
  const [source, setSource] = useState("");
  const toggle = useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) => setMcpEnabled(id, enabled),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["mcp"] }),
  });
  const install = useMutation({
    mutationFn: () => installMcpServer(source.trim()),
    onSuccess: () => {
      setSource("");
      qc.invalidateQueries({ queryKey: ["mcp"] });
    },
  });
  const remove = useMutation({
    mutationFn: (serverId: string) => removeMcpServer(serverId),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["mcp"] }),
  });
  const addLinear = useMutation({
    mutationFn: () => addRemoteMcpServer("linear", "https://mcp.linear.app/sse"),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["mcp"] });
      toast.success("Linear added — enable it, then Connect to authorize.");
    },
    onError: (e) => toast.error(`Couldn't add Linear: ${errMsg(e)}`),
  });
  const connect = useMutation({
    mutationFn: async (id: string) => {
      // First connect: capture the OAuth app's Client ID (+ secret for
      // confidential providers like Linear). Leave blank to reuse stored creds.
      const clientId = await promptDialog(`OAuth Client ID for "${id}"`, {
        placeholder: "blank = already configured",
      });
      if (clientId && clientId.trim()) {
        const secret =
          (await promptDialog("Client secret", {
            placeholder: "blank for public/PKCE-only clients",
            password: true,
          })) ?? "";
        await setMcpOauthClient(id, clientId.trim(), secret.trim() || undefined);
      }
      await authorizeMcpServer(id, id === "linear" ? "read,write,issues:create" : undefined);
    },
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["mcp"] });
      toast.success("Authorized — your browser completed the sign-in.");
    },
    onError: (e) => toast.error(`Authorization failed: ${errMsg(e)}`),
  });
  // Add an HTTP server by URL, optionally with a static bearer token. The
  // token is sent once, stored on this device only, and never read back.
  const [addOpen, setAddOpen] = useState(false);
  const [httpId, setHttpId] = useState("");
  const [httpUrl, setHttpUrl] = useState("");
  const [httpToken, setHttpToken] = useState("");
  const addHttp = useMutation({
    mutationFn: () =>
      addRemoteMcpServer(httpId.trim(), httpUrl.trim(), httpToken.trim() || undefined),
    onSuccess: () => {
      const id = httpId.trim();
      setHttpId("");
      setHttpUrl("");
      setHttpToken("");
      setAddOpen(false);
      qc.invalidateQueries({ queryKey: ["mcp"] });
      toast.success(`${id} added — enable it to use its tools.`);
    },
    onError: (e) => toast.error(`Couldn't add server: ${errMsg(e)}`),
  });
  const setToken = useMutation({
    mutationFn: async ({ id, clear }: { id: string; clear: boolean }) => {
      if (clear) return setMcpBearerToken(id, "");
      const token = await promptDialog(`Bearer token for "${id}"`, {
        placeholder: "Stored on this device only",
        password: true,
      });
      if (token === null || !token.trim()) return null; // cancelled
      return setMcpBearerToken(id, token);
    },
    onSuccess: (set, { id }) => {
      if (set === null) return;
      qc.invalidateQueries({ queryKey: ["mcp"] });
      toast.success(set ? `Token saved for ${id}.` : `Token removed for ${id}.`);
    },
    onError: (e) => toast.error(`Couldn't save token: ${errMsg(e)}`),
  });
  const hasLinear = (servers.data ?? []).some((s) => s.id === "linear");
  return (
    <Section
      title="MCP servers"
      info="Servers stay inert until enabled. Add a hosted server (e.g. Linear) and Connect to authorize it, or install a workspace-scoped entry from a manifest URL or GitHub reference."
    >
      {!hasLinear && (
        <Button onClick={() => addLinear.mutate()} disabled={addLinear.isPending} className="self-start">
          <IconPlus size={15} /> Add Linear (issues)
        </Button>
      )}
      <div className="flex gap-2">
        <input
          value={source}
          onChange={(e) => setSource(e.target.value)}
          placeholder="owner/repo/mcp.json or https://…"
          aria-label="MCP manifest URL or GitHub reference"
          className="flex-1 rounded-xl border px-3 py-2 font-mono text-sm"
          style={fieldStyle}
        />
        <Button variant="primary" size="md" onClick={() => install.mutate()}>
          Install
        </Button>
      </div>
      <FormDisclosure label="Add HTTP server" open={addOpen} onToggle={() => setAddOpen((v) => !v)}>
        <form
          className="flex flex-col gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            addHttp.mutate();
          }}
        >
          <Field label="Name">
            <input
              value={httpId}
              onChange={(e) => setHttpId(e.target.value)}
              placeholder="docs"
              className="rounded-xl border px-3 py-2 text-sm"
              style={fieldStyle}
            />
          </Field>
          <Field label="URL">
            <input
              value={httpUrl}
              onChange={(e) => setHttpUrl(e.target.value)}
              placeholder="https://host:8090/mcp"
              className="rounded-xl border px-3 py-2 font-mono text-sm"
              style={fieldStyle}
            />
          </Field>
          <Field
            label="Bearer token (optional)"
            hint="Kept on this device only — never synced, exported or written to ligus.config.toml."
          >
            <input
              type="password"
              autoComplete="off"
              value={httpToken}
              onChange={(e) => setHttpToken(e.target.value)}
              placeholder="For servers protected by a fixed token"
              className="rounded-xl border px-3 py-2 font-mono text-sm"
              style={fieldStyle}
            />
          </Field>
          <Button
            type="submit"
            variant="primary"
            size="md"
            className="self-start"
            disabled={addHttp.isPending || !httpId.trim() || !httpUrl.trim()}
          >
            Add server
          </Button>
        </form>
      </FormDisclosure>
      {servers.data?.length === 0 && <p className="text-sm text-ink-soft">None configured.</p>}
      {(servers.data ?? []).length > 0 && (
        <RowList label="MCP servers">
          {(servers.data ?? []).map((s) => (
            <McpRow
              key={s.id}
              s={s}
              onToggle={(v) => toggle.mutate({ id: s.id, enabled: v })}
              onToken={() => setToken.mutate({ id: s.id, clear: s.hasBearerToken })}
              tokenBusy={setToken.isPending}
              onConnect={() => connect.mutate(s.id)}
              connectBusy={connect.isPending}
              onInspect={() => inspect.mutate(s.id)}
              inspectBusy={inspect.isPending}
              onRemove={() => remove.mutate(s.id)}
            />
          ))}
        </RowList>
      )}
    </Section>
  );
}

/// One MCP server: a single line (id, transport, status, switch). The token,
/// connect, check and remove actions live in the row menu (right-click or ⋯).
/// An HTTP server that isn't authorized yet keeps a visible Connect, and the
/// shell warning is security information, so it stays under the line.
function McpRow({
  s,
  onToggle,
  onToken,
  tokenBusy,
  onConnect,
  connectBusy,
  onInspect,
  inspectBusy,
  onRemove,
}: {
  s: McpServerDto;
  onToggle: (v: boolean) => void;
  onToken: () => void;
  tokenBusy: boolean;
  onConnect: () => void;
  connectBusy: boolean;
  onInspect: () => void;
  inspectBusy: boolean;
  onRemove: () => void;
}) {
  const http = s.transport === "http";
  const needsAuth = http && !s.hasBearerToken && !s.oauthConnected;
  const actions: RowAction[] = [
    ...(http
      ? [
          {
            label: s.hasBearerToken ? "Clear token" : "Set token",
            onSelect: onToken,
            disabled: tokenBusy,
            title: s.hasBearerToken ? "Remove the saved bearer token" : "Save a static bearer token (stored on this device only)",
          },
          { label: "Connect", onSelect: onConnect, disabled: connectBusy, title: "Authorize this server in your browser (OAuth)" },
        ]
      : []),
    ...(s.enabled
      ? [
          {
            label: "Check tools",
            onSelect: onInspect,
            disabled: inspectBusy,
            title: "List this server's tools now and flag any that run shell commands",
          },
        ]
      : []),
    ...(s.isManaged ? [{ label: "Remove", onSelect: onRemove, danger: true }] : []),
  ];
  return (
    <ListRow
      title={s.id}
      actions={actions}
      meta={
        <>
          [{s.transport}] {s.detail}
          {s.hasBearerToken && (
            <span className="ml-1" data-tip="A static bearer token is saved on this device">
              · token set
            </span>
          )}
          {!s.hasBearerToken && s.oauthConnected && <span className="ml-1">· connected</span>}
          {s.enabled && s.shellTools === null && (
            <span className="ml-1" data-tip="Until this device has listed the server's tools, turns without the shell don't get it.">
              · tools not checked yet
            </span>
          )}
          {s.secretForOtherUrl && (
            <span
              className="ml-1"
              data-tip="This server's URL changed since its token was saved, so the token is not sent. Set the token again or reconnect."
            >
              · token saved for a different URL
            </span>
          )}
        </>
      }
      trailing={
        <>
          {needsAuth && (
            <button
              className="text-xs underline text-ink-soft hover:text-ink disabled:opacity-40"
              disabled={connectBusy}
              onClick={onConnect}
              data-tip="Authorize this server in your browser (OAuth)"
            >
              Connect
            </button>
          )}
          <Switch on={s.enabled} onChange={onToggle} label={`Enable ${s.id}`} />
        </>
      }
    >
      {s.shellTools && s.shellTools.length > 0 ? (
        <div
          className="px-1 text-xs font-medium"
          style={{ color: "var(--ligus-danger-ink)" }}
          data-tip="A tool that runs shell commands is a shell: only turns that have the shell (the workspace Owner's, or an agent the Owner granted it) get this server."
        >
          · offers a shell ({s.shellTools.join(", ")}): Owner turns only
        </div>
      ) : null}
    </ListRow>
  );
}
