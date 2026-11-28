// My devices → Add a computer: "Link a device: code K7Q-4MX". The new
// device types the code (I already use Ligus, lt's Add to my devices, or
// `ligus devices request --code`), its request comes to the Inbox, and the
// approval sheet opens here for it. See app/src/link_code_ui.rs.

import { useEffect, useRef, useState } from "react";
import { linkCodeClose, linkCodeShow, onLinkCodeChanged, type LinkCodeDto } from "@/lib/ipc";
import { relayHost } from "@/lib/shortCode";
import { approvalSheet, useApprovals } from "@/state/approvals";
import { LinkButton } from "@/components/ui";

/// The code, large, in its two groups; spelled for screen readers.
function BigCode({ code, spelled }: { code: string; spelled: string | null }) {
  return (
    <p
      role="img"
      aria-label={`Code: ${spelled ?? code}`}
      data-testid="link-code"
      className="whitespace-nowrap py-1 text-[28px] font-semibold leading-9 tabular-nums"
      style={{ fontFamily: "var(--ligus-mono)", letterSpacing: "0.08em" }}
    >
      {code}
    </p>
  );
}

function minutesLeft(expiresAt: number | null): number | null {
  if (!expiresAt) return null;
  return Math.max(0, Math.ceil((expiresAt - Date.now()) / 60_000));
}

/// The code card. `onRequest` runs when the new device's request is here
/// and its approval sheet has opened (the panel closes then).
export function LinkCode({
  workspaceId,
  onRequest,
  onState,
}: {
  workspaceId: string;
  onRequest: () => void;
  /// Where the code stands (`noRoutes`: show the approval address instead).
  onState?: (state: string) => void;
}) {
  const [view, setView] = useState<LinkCodeDto | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const shownAt = useRef(Date.now());
  const { requests } = useApprovals();

  async function show() {
    setProblem(null);
    shownAt.current = Date.now();
    try {
      setView(await linkCodeShow(workspaceId));
    } catch (e) {
      setProblem(String(e));
    }
  }

  useEffect(() => {
    let live = true;
    let unlisten: (() => void) | undefined;
    void onLinkCodeChanged((v) => {
      if (live && v.workspaceId === workspaceId) setView(v);
    })
      .then((u) => {
        if (live) unlisten = u;
        else u();
      })
      .catch(() => {});
    void show();
    return () => {
      live = false;
      unlisten?.();
      // Closing the panel takes a code nobody typed back.
      void linkCodeClose().catch(() => {});
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [workspaceId]);

  const state = view?.state ?? "posting";
  useEffect(() => onState?.(state), [state, onState]);

  // Used: open the request it brought as soon as it is in the Inbox.
  const used = view?.state === "used";
  useEffect(() => {
    if (!used) return;
    const r = requests
      .filter((q) => q.workspaceId === workspaceId && q.kind === "computer" && q.createdAt >= shownAt.current - 60_000)
      .sort((a, b) => b.createdAt - a.createdAt)[0];
    if (r) {
      approvalSheet.open(r.workspaceId, r.rid);
      onRequest();
    }
  }, [used, requests, workspaceId, onRequest]);

  if (problem) {
    return (
      <p className="text-xs" style={{ color: "var(--ligus-danger-ink)" }} role="alert" data-testid="link-code-problem">
        {problem}
      </p>
    );
  }
  if (!view || view.state === "posting" || view.state === "idle") {
    return (
      <p className="text-xs text-ink-soft" data-testid="link-code-posting">
        Getting a code…
      </p>
    );
  }
  if (view.state === "noRoutes" || view.state === "failed") {
    return (
      <p className="text-xs text-ink-soft" data-testid={`link-code-${view.state}`}>
        {view.state === "noRoutes"
          ? "Your relay doesn't know short codes yet: give the new device the approval address below instead."
          : view.error}{" "}
        {view.state === "failed" && (
          <LinkButton tone="ink" onClick={() => void show()}>
            Try again
          </LinkButton>
        )}
      </p>
    );
  }
  if (view.state === "used") {
    return (
      <div className="space-y-1" data-testid="link-code-used">
        <p className="text-sm">The new device has the code.</p>
        <p className="text-xs text-ink-soft">Its request is on the way to your Inbox; it opens here when it arrives.</p>
      </div>
    );
  }
  if (view.state === "expired") {
    return (
      <p className="text-xs text-ink-soft" data-testid="link-code-expired">
        The code expired.{" "}
        <LinkButton tone="ink" onClick={() => void show()}>
          New code
        </LinkButton>
      </p>
    );
  }
  const left = minutesLeft(view.expiresAt);
  return (
    <div className="space-y-1" data-testid="link-code-live">
      <div className="text-xs font-medium">Link a device</div>
      {view.code && <BigCode code={view.code} spelled={view.codeSpelled} />}
      <p className="text-xs text-ink-soft">
        On the new device, choose <em>I already use Ligus</em> and type this code
        {view.relay ? (
          <>
            {" "}
            with the relay <strong>{relayHost(view.relay)}</strong>
          </>
        ) : null}
        . It works once
        {left !== null ? `, for ${left} more minute${left === 1 ? "" : "s"}` : ""}.
      </p>
      <p className="text-xs text-ink-soft">
        In a terminal:{" "}
        <code>
          ligus devices request --code {view.code}
          {view.relay ? ` --relay ${view.relay}` : ""}
        </code>
      </p>
      {view.notice && (
        <p className="text-xs text-ink-soft" data-testid="link-code-notice">
          {view.notice}
        </p>
      )}
    </div>
  );
}
