import { useState } from "react";
import { admitBox, inspectBoxRequest, type BoxAdmission, type BoxRequest } from "@/lib/ipc";
import { copyWhenReady } from "@/lib/clipboard";
import { toast, errMsg } from "@/components/Toast";
import { Modal } from "@/components/ui";

type Mode = "request" | "secret";

/// Admit a `ligus serve` or `ligus worker` box to a workspace as its own
/// service member (capped at Contributor; its votes never count as a
/// person's). Either paste the join request the box printed (the code comes
/// back sealed to that box) or make a single-use code for a Kubernetes
/// Secret. Both are confirmed with this person's passkey.
export function AdmitBoxModal({ workspaceId, workspaceName, onClose }: { workspaceId: string; workspaceName: string; onClose: () => void }) {
  const [mode, setMode] = useState<Mode>("request");
  const [request, setRequest] = useState("");
  const [checked, setChecked] = useState<BoxRequest | null>(null);
  const [hours, setHours] = useState(24);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<BoxAdmission | null>(null);

  async function check() {
    try {
      setChecked(await inspectBoxRequest(request.trim()));
    } catch (e) {
      setChecked(null);
      toast.error(errMsg(e));
    }
  }

  async function admit() {
    if (busy) return;
    setBusy(true);
    try {
      const r = mode === "request" ? await admitBox(workspaceId, request.trim(), null) : await admitBox(workspaceId, null, hours * 60);
      setResult(r);
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  async function copy(code: string) {
    try {
      await copyWhenReady(Promise.resolve(code));
      toast.success("Admission code copied.");
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  const tab = (m: Mode, label: string) => (
    <button
      role="tab"
      aria-selected={mode === m}
      className="rounded-md px-3 py-1 text-sm"
      style={{ background: mode === m ? "var(--ligus-accent-fill)" : "transparent", color: mode === m ? "var(--ligus-on-accent)" : "var(--ligus-ink)" }}
      onClick={() => {
        setMode(m);
        setResult(null);
      }}
    >
      {label}
    </button>
  );

  return (
    <Modal onClose={onClose} panelClassName="w-full max-w-lg rounded-xl border p-5" panelStyle={{ background: "var(--ligus-panel)", borderColor: "var(--ligus-line)", color: "var(--ligus-ink)" }}>
      <h2 className="ligus-title">Admit a box to {workspaceName}</h2>
      <p className="mt-1 text-sm text-ink-soft">
        A serve or worker box joins as its own service member: at most Contributor, and its votes never count as a person's approval.
        You confirm with your passkey.
      </p>
      <div role="tablist" className="mt-3 flex gap-1">
        {tab("request", "From the box's join request")}
        {tab("secret", "For Kubernetes")}
      </div>
      {result ? (
        <div className="mt-4 space-y-2">
          <p className="text-sm">
            {result.sealed
              ? "Sealed to that box: only it can open this code. Run `ligus admit redeem <code>` on the box."
              : "A single-use bearer code: put it in a Kubernetes Secret (or the file LIGUS_ADMISSION_FILE names) and delete it once the box has joined."}{" "}
            It expires in {result.expiresInMinutes} min.
          </p>
          <textarea readOnly aria-label="Admission code" className="h-24 w-full rounded border p-2 font-mono text-xs" value={result.code} style={{ borderColor: "var(--ligus-line)", background: "transparent" }} />
          <div className="flex justify-end gap-2">
            <button className="rounded-md border px-3 py-1 text-sm" style={{ borderColor: "var(--ligus-line)" }} onClick={() => void copy(result.code)}>
              Copy code
            </button>
            <button className="rounded-md px-3 py-1 text-sm" style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }} onClick={onClose}>
              Done
            </button>
          </div>
        </div>
      ) : mode === "request" ? (
        <div className="mt-4 space-y-2">
          <label className="block text-sm" htmlFor="box-request">
            Join request (run <code>ligus admit request</code> on the box)
          </label>
          <textarea
            id="box-request"
            className="h-20 w-full rounded border p-2 font-mono text-xs"
            style={{ borderColor: "var(--ligus-line)", background: "transparent" }}
            value={request}
            onChange={(e) => {
              setRequest(e.target.value);
              setChecked(null);
            }}
            placeholder="ligusjoin1:…"
          />
          {checked ? (
            <p className="text-sm">
              Box <strong>{checked.label || "unnamed"}</strong>, fingerprint <code>{checked.fingerprint}</code>. Check it matches what the box printed.
            </p>
          ) : null}
          <div className="flex justify-end gap-2">
            <button className="rounded-md border px-3 py-1 text-sm" style={{ borderColor: "var(--ligus-line)" }} disabled={!request.trim()} onClick={() => void check()}>
              Check
            </button>
            <button
              className="rounded-md px-3 py-1 text-sm disabled:opacity-50"
              style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
              disabled={!checked || busy}
              onClick={() => void admit()}
            >
              Admit this box
            </button>
          </div>
        </div>
      ) : (
        <div className="mt-4 space-y-2">
          <label className="block text-sm" htmlFor="box-hours">
            Redeemable for (hours, up to 6 days)
          </label>
          <input
            id="box-hours"
            type="number"
            min={1}
            max={144}
            className="w-24 rounded border p-1 text-sm"
            style={{ borderColor: "var(--ligus-line)", background: "transparent" }}
            value={hours}
            onChange={(e) => setHours(Math.min(144, Math.max(1, Number(e.target.value) || 1)))}
          />
          <p className="text-xs text-ink-soft">Anyone holding this code before the box redeems it can join as that box: keep it in a Secret, never in a values file.</p>
          <div className="flex justify-end">
            <button
              className="rounded-md px-3 py-1 text-sm disabled:opacity-50"
              style={{ background: "var(--ligus-accent-fill)", color: "var(--ligus-on-accent)" }}
              disabled={busy}
              onClick={() => void admit()}
            >
              Make an admission code
            </button>
          </div>
        </div>
      )}
    </Modal>
  );
}
