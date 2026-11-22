import { useEffect, useState } from "react";
import {
  checkForAppUpdate,
  installUpdate,
  onUpdateProgress,
  openExternal,
  type AppUpdateInfo,
} from "@/lib/ipc";
import { IconSparkle } from "@/lib/icons";
import { Button, Modal } from "@/components/ui";
import { StatusBarItem } from "@/components/kit";

const DISMISS_KEY = "ligus.updateDismissed";

function skipped(tag: string): boolean {
  try {
    return window.localStorage.getItem(DISMISS_KEY) === tag;
  } catch {
    return false;
  }
}

/// A newer published release, if the backend reports one (dev builds never
/// do) and it wasn't skipped. Checked once per launch; failures are silent.
/// "Later" hides it until the next launch; "Skip" hides that version for good.
export function useAppUpdate() {
  const [info, setInfo] = useState<AppUpdateInfo | null>(null);
  // Install state: null = idle, else a 0–100 percent (or -1 while starting).
  const [pct, setPct] = useState<number | null>(null);

  useEffect(() => {
    let alive = true;
    void checkForAppUpdate()
      .then((u) => {
        if (alive && u && !skipped(u.tag)) setInfo(u);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => {
    const un = onUpdateProgress((p) => {
      if (p.done) setPct(100);
      else if (p.total && p.downloaded != null) setPct(Math.round((p.downloaded / p.total) * 100));
    });
    return () => {
      void un.then((f) => f());
    };
  }, []);

  return {
    info,
    pct,
    later: () => setInfo(null),
    skip: () => {
      if (info) {
        try {
          window.localStorage.setItem(DISMISS_KEY, info.tag);
        } catch {
          // Storage unavailable: it hides for this session only.
        }
      }
      setInfo(null);
    },
    // Try the in-place updater (download → verify → install → relaunch). If
    // it isn't available (no signed release, unconfigured), open the release
    // page instead, so the button always does something.
    install: async () => {
      setPct(-1);
      try {
        await installUpdate(); // relaunches on success; never returns
      } catch {
        setPct(null);
        void openExternal(info?.url || "https://github.com/langstroth-ai/ligus/releases/latest").catch(() => {});
      }
    },
  };
}

/// "Update available" in the status bar: one quiet item that opens the
/// release details with Download, Later and Skip. Nothing when up to date.
export function UpdateStatusItem() {
  const u = useAppUpdate();
  const [open, setOpen] = useState(false);
  if (!u.info) return null;
  const installing = u.pct !== null;
  const label =
    u.pct === null ? "Download" : u.pct < 0 ? "Starting…" : u.pct >= 100 ? "Restarting…" : `Installing… ${u.pct}%`;
  return (
    <>
      <StatusBarItem
        icon={<IconSparkle size={11} />}
        tone="info"
        onClick={() => setOpen(true)}
        describe={`${u.info.name || u.info.tag} is available`}
      >
        {installing ? label : "Update available"}
      </StatusBarItem>
      {open && (
        <Modal
          onClose={() => setOpen(false)}
          panelClassName="w-[380px] max-w-full rounded-2xl border p-5"
          panelStyle={{ borderColor: "var(--ligus-line)", background: "var(--ligus-panel)", color: "var(--ligus-ink)" }}
        >
          <h2 className="ligus-section">Update available</h2>
          <div className="mt-0.5 text-sm text-ink-soft">{u.info.name || u.info.tag}</div>
          {u.info.notes && <div className="mt-2 text-xs text-ink-soft">{u.info.notes}</div>}
          <div className="mt-4 flex items-center justify-end gap-2">
            {!installing && (
              <>
                <Button size="sm" variant="quiet" onClick={u.skip} title="Don't show this version again">
                  Skip this version
                </Button>
                <Button
                  size="sm"
                  onClick={() => {
                    u.later();
                    setOpen(false);
                  }}
                >
                  Later
                </Button>
              </>
            )}
            <Button size="sm" variant="primary" onClick={() => void u.install()} disabled={installing}>
              {label}
            </Button>
          </div>
        </Modal>
      )}
    </>
  );
}
