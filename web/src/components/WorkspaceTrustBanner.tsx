import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { getWorkspaceTrust, setWorkspaceTrust, trustWorkspace, type WorkspaceTrustItem } from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";
import { Modal } from "@/components/ui";
import { IconShield, IconX } from "@/lib/icons";

/// S1: the open repository ships runtimes or MCP servers (commands Ligus
/// would run, endpoints it would send prompts to). They stay inert until
/// enabled on this device. The banner says how many are waiting; the sheet
/// lists each exact command line and the file it came from.
export function WorkspaceTrustBanner() {
  const qc = useQueryClient();
  const [open, setOpen] = useState(false);
  const trust = useQuery({ queryKey: ["workspace-trust"], queryFn: getWorkspaceTrust, refetchInterval: 30000 });
  const refresh = async () => {
    await Promise.all([
      qc.invalidateQueries({ queryKey: ["workspace-trust"] }),
      qc.invalidateQueries({ queryKey: ["runtimes"] }),
      qc.invalidateQueries({ queryKey: ["mcp"] }),
    ]);
  };
  const toggle = useMutation({
    mutationFn: ({ digest, enabled }: { digest: string; enabled: boolean }) => setWorkspaceTrust(digest, enabled),
    onSuccess: refresh,
    onError: (e) => toast.error(`Couldn't change trust: ${errMsg(e)}`),
  });
  const all = useMutation({
    mutationFn: trustWorkspace,
    onSuccess: async () => {
      await refresh();
      setOpen(false);
      toast.success("Trusted this workspace's current commands.");
    },
    onError: (e) => toast.error(`Couldn't trust the workspace: ${errMsg(e)}`),
  });

  const items = trust.data?.items ?? [];
  const waiting = items.filter((i) => !i.enabled);
  if (items.length === 0 || (waiting.length === 0 && !open)) return null;

  return (
    <>
      {waiting.length > 0 && (
        <div
          role="status"
          className="fixed bottom-4 right-4 z-[60] flex w-[340px] items-start gap-3 rounded-2xl border p-4 shadow-xl"
          style={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)", color: "var(--ligus-ink)" }}
        >
          <div className="mt-0.5" style={{ color: "var(--ligus-accent-ink)" }} aria-hidden>
            <IconShield size={18} />
          </div>
          <div className="min-w-0 flex-1 text-sm">
            This workspace wants to run {waiting.length} command{waiting.length === 1 ? "" : "s"} (MCP servers, runtimes).
            <div className="mt-2">
              <button
                onClick={() => setOpen(true)}
                className="rounded-lg px-3 py-1.5 text-sm font-medium hover:brightness-110"
                style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
              >
                Review
              </button>
            </div>
          </div>
        </div>
      )}
      {open && (
        <Modal onClose={() => setOpen(false)} panelClassName="w-[640px] max-w-full rounded-2xl border p-5" panelStyle={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)", color: "var(--ligus-ink)" }}>
          <div className="flex items-start justify-between gap-3">
            <div>
              <div className="text-base font-semibold">Commands this workspace ships</div>
              <div className="mt-1 text-sm text-ink-soft">
                Nothing here runs until you enable it on this device. A command that changes needs enabling again.
              </div>
            </div>
            <button onClick={() => setOpen(false)} aria-label="Close" className="opacity-60 hover:opacity-100">
              <IconX size={16} />
            </button>
          </div>
          <ul className="mt-4 flex max-h-[50vh] flex-col gap-2 overflow-y-auto">
            {items.map((it: WorkspaceTrustItem) => (
              <li key={it.digest} className="rounded-xl border p-3" style={{ borderColor: "var(--ligus-line)" }}>
                <div className="flex items-center justify-between gap-2 text-sm">
                  <span className="font-medium">
                    {it.kind === "mcpServer" ? "MCP server" : "Runtime"} <code>{it.id}</code>
                  </span>
                  <label className="flex items-center gap-2 text-xs">
                    <input
                      type="checkbox"
                      checked={it.enabled}
                      disabled={toggle.isPending}
                      onChange={(e) => toggle.mutate({ digest: it.digest, enabled: e.target.checked })}
                    />
                    {it.enabled ? "Enabled" : "Inert"}
                  </label>
                </div>
                <pre className="mt-2 overflow-x-auto whitespace-pre-wrap break-all text-xs">{it.commandLine}</pre>
                <div className="mt-1 text-xs text-ink-soft">from {it.source}</div>
              </li>
            ))}
          </ul>
          <div className="mt-4 flex justify-end gap-2">
            <button onClick={() => setOpen(false)} className="rounded-lg px-3 py-1.5 text-sm opacity-70 hover:opacity-100">
              Close
            </button>
            <button
              onClick={() => all.mutate()}
              disabled={all.isPending || waiting.length === 0}
              className="rounded-lg px-3 py-1.5 text-sm font-medium hover:brightness-110 disabled:opacity-60"
              style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
            >
              Trust this workspace
            </button>
          </div>
        </Modal>
      )}
    </>
  );
}
