import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { addRuntime, detectCliRuntimes, listRuntimes, type DetectedCliDto } from "@/lib/ipc";
import { Button, fieldStyle, InfoTip, ListRow, RowList } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import { ModelFieldHelp } from "@/components/ModelPicker";

const LOGIN_TEXT: Record<DetectedCliDto["login"], string> = {
  loggedIn: "signed in",
  notLoggedIn: "not signed in",
  unknown: "sign-in not checked",
};

/// Subscription CLIs (Google `agy`, GitHub `copilot`): what's installed on this
/// machine, whether it's signed in, and a one-click "Add runtime". Keyless —
/// the login stays in the CLI's own config / OS keychain; Ligus only runs it.
export function CliRuntimesPanel() {
  const qc = useQueryClient();
  const [probe, setProbe] = useState(false);
  const clis = useQuery({
    queryKey: ["cli-runtimes", probe],
    queryFn: () => detectCliRuntimes(probe),
  });
  const runtimes = useQuery({ queryKey: ["runtimes"], queryFn: listRuntimes });
  const [models, setModels] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);

  async function add(c: DetectedCliDto) {
    if (!c.provider) return;
    setBusy(c.id);
    try {
      const model = (models[c.id] ?? "").trim();
      const id = model ? `${c.id}-${model}`.toLowerCase().replace(/[^a-z0-9]+/g, "-") : c.id;
      const name = model ? `${c.label} · ${model}` : c.label;
      await addRuntime(id, name, c.provider, "local", "", model, true, false);
      await qc.invalidateQueries({ queryKey: ["runtimes"] });
      toast.success(`Added ${name} — pick it from the route pill in any chat.`);
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(null);
    }
  }

  const list = clis.data ?? [];
  const added = (provider: string | null) => !!provider && (runtimes.data ?? []).some((r) => r.provider === provider);

  return (
    <div className="mb-3 border-b pb-3" style={{ borderColor: "var(--ligus-line)" }}>
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-1.5">
          <div className="text-sm font-medium">Subscription CLIs</div>
          <InfoTip
            label="Subscription CLIs"
            text="Use a Google AI Pro / Ultra or GitHub Copilot subscription through its official CLI. Ligus runs the CLI headless with your permission setting."
          />
        </div>
        <div className="flex gap-1.5">
          <Button size="sm" variant="ghost" disabled={clis.isFetching} onClick={() => void clis.refetch()}>
            {clis.isFetching ? "Scanning…" : "Re-scan"}
          </Button>
        </div>
      </div>
      <p className="mt-0.5 text-[11.5px] text-ink-soft">The login stays in the CLI; Ligus never sees it.</p>
      {clis.isError && <p className="mt-2 text-xs text-ink-soft">{errMsg(clis.error)}</p>}
      <RowList label="Subscription CLIs" className="mt-1">
        {list.map((c) => {
          const usable = c.installed && c.supported;
          const details =
            (usable && c.login !== "loggedIn") || (usable && !added(c.provider)) || !c.installed || !!c.note;
          return (
            <ListRow
              key={c.id}
              testId={`cli-${c.id}`}
              title={c.label}
              actions={
                c.id === "antigravity" && c.installed
                  ? [
                      {
                        label: "Check Google sign-in",
                        disabled: clis.isFetching,
                        title: "Runs `agy models` (read-only, uses no quota) to confirm the Google sign-in",
                        onSelect: () => (probe ? void clis.refetch() : setProbe(true)),
                      },
                    ]
                  : undefined
              }
              meta={
                <>
                  <span className="font-mono">
                    {c.binary}
                    {c.version ? ` ${c.version}` : ""}
                  </span>
                  {usable && (
                    <>
                      {" · "}
                      <span
                        style={{
                          color:
                            c.login === "loggedIn"
                              ? "var(--ligus-success-ink)"
                              : c.login === "notLoggedIn"
                                ? "var(--ligus-danger-ink)"
                                : undefined,
                        }}
                      >
                        {LOGIN_TEXT[c.login]}
                      </span>
                      {c.loginDetail && <span> · {c.loginDetail}</span>}
                    </>
                  )}
                </>
              }
              trailing={
                !c.installed ? (
                  <span className="text-xs text-ink-soft">not installed</span>
                ) : !c.supported ? (
                  <span className="text-xs text-ink-soft">not usable for subscriptions</span>
                ) : added(c.provider) ? (
                  <span className="text-xs text-ink-soft">runtime added</span>
                ) : (
                  <Button size="sm" variant="primary" disabled={busy === c.id} onClick={() => void add(c)}>
                    Add runtime
                  </Button>
                )
              }
            >
              {details && (
                <div className="space-y-1 px-1 text-xs text-ink-soft">
                  {usable && c.login !== "loggedIn" && (
                    <div>
                      To sign in, run <code>{c.loginCommand}</code> in a terminal on this machine.
                    </div>
                  )}
                  {usable && !added(c.provider) && (
                    <>
                      <input
                        value={models[c.id] ?? ""}
                        onChange={(e) => setModels((m) => ({ ...m, [c.id]: e.target.value }))}
                        placeholder="Model (blank = the CLI's default)"
                        list={`cli-models-${c.id}`}
                        className="w-full rounded-lg border px-2 py-1 font-mono text-xs"
                        style={fieldStyle}
                      />
                      {c.provider && (
                        <ModelFieldHelp listId={`cli-models-${c.id}`} provider={c.provider} model={models[c.id] ?? ""} />
                      )}
                    </>
                  )}
                  {!c.installed && (
                    <div>
                      Install: <code>{c.installCommand}</code>
                    </div>
                  )}
                  {c.note && <div>{c.note}</div>}
                </div>
              )}
            </ListRow>
          );
        })}
      </RowList>
    </div>
  );
}
