import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ligusAuthStatus,
  ligusAuthStart,
  ligusAuthPoll,
  ligusAuthSignOut,
  openExternal,
  type LigusAuthStartDto,
} from "@/lib/ipc";
import { Button, Section, fieldStyle } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";

/// Organization sign-in: sign this device in at your organization's identity
/// provider through langstroth-auth. The relay token it gets is short-lived and
/// refreshed automatically. It reaches the relay only; a workspace Admin still
/// adds people as members.
export function OrgSignInSection() {
  const qc = useQueryClient();
  const status = useQuery({ queryKey: ["ligus-auth-status"], queryFn: ligusAuthStatus });
  const [url, setUrl] = useState("");
  const [flow, setFlow] = useState<LigusAuthStartDto | null>(null);

  const signOut = useMutation({
    mutationFn: ligusAuthSignOut,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["ligus-auth-status"] });
      qc.invalidateQueries({ queryKey: ["sync-status"] });
    },
    onError: (e) => toast.error(errMsg(e)),
  });

  async function start() {
    try {
      const s = await ligusAuthStart(url.trim());
      setFlow(s);
      void openExternal(s.verificationUriComplete ?? s.verificationUri).catch(() => {
        /* the link is shown below */
      });
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  useEffect(() => {
    if (!flow) return;
    let alive = true;
    let interval = Math.max(flow.interval, 1) * 1000;
    let timer: ReturnType<typeof setTimeout>;
    const tick = async () => {
      if (!alive) return;
      try {
        const r = await ligusAuthPoll();
        if (!alive) return;
        if (r === "success") {
          setFlow(null);
          setUrl("");
          qc.invalidateQueries({ queryKey: ["ligus-auth-status"] });
          qc.invalidateQueries({ queryKey: ["sync-status"] });
          toast.success("Signed in. The relay token now refreshes automatically.");
          return;
        }
        if (r === "denied" || r === "expired" || r === "none") {
          setFlow(null);
          toast.error(r === "denied" ? "Sign-in was denied." : "The code expired. Try again.");
          return;
        }
        if (r === "slowDown") interval += 5000;
      } catch (e) {
        setFlow(null);
        toast.error(errMsg(e));
        return;
      }
      if (alive) timer = setTimeout(tick, interval);
    };
    timer = setTimeout(tick, interval);
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [flow, qc]);

  const st = status.data;

  return (
    <Section title="Organization sign-in">
      {st ? (
        <div className="space-y-1 rounded-xl border px-3 py-2.5" style={fieldStyle}>
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <div className="truncate font-medium">{st.tenant}</div>
              <div className="truncate text-xs text-ink-soft">{st.authUrl}</div>
            </div>
            <button
              className="shrink-0 text-xs hover:opacity-80"
              style={{ color: "var(--ligus-danger-ink)" }}
              onClick={() => signOut.mutate()}
            >
              Sign out
            </button>
          </div>
          <div className="text-xs text-ink-soft">
            Relay access to {st.workspaces.length} workspace{st.workspaces.length === 1 ? "" : "s"}
            {st.active ? "" : " · waiting for a fresh token"}
          </div>
        </div>
      ) : flow ? (
        <div className="space-y-2 rounded-xl border px-3 py-3" style={{ borderColor: "var(--ligus-accent-cool)" }}>
          <p className="text-xs text-ink-soft">
            Go to <code>{flow.verificationUri}</code> and enter this code:
          </p>
          <div className="flex items-center gap-3">
            <code className="text-xl font-bold tracking-[0.25em]">{flow.userCode}</code>
            <button
              className="text-xs underline text-ink-soft hover:text-ink"
              onClick={() => void openExternal(flow.verificationUriComplete ?? flow.verificationUri).catch((e) => toast.error(errMsg(e)))}
            >
              open
            </button>
            <button className="text-xs underline text-ink-soft hover:text-ink" onClick={() => setFlow(null)}>
              cancel
            </button>
          </div>
          <p className="text-xs text-ink-soft">Check that the code matches before you approve.</p>
        </div>
      ) : (
        <div className="space-y-2">
          <p className="text-xs text-ink-soft">
            If your organization runs langstroth-auth, sign in with your company account instead of
            pasting a relay token. Paste the address your admin gave you.
          </p>
          <div className="flex gap-2">
            <input
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="https://auth.example.com/t/your-team"
              className="flex-1 rounded-xl border px-3 py-2 text-sm"
              style={fieldStyle}
            />
            <Button variant="primary" size="md" disabled={!url.trim()} onClick={() => void start()}>
              Sign in
            </Button>
          </div>
        </div>
      )}
      <p className="mt-2 text-xs text-ink-soft">
        This only lets you reach the relay. To join a workspace, an Admin still has to add you as a
        member.
      </p>
    </Section>
  );
}
