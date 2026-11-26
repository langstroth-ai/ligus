import { useSyncExternalStore } from "react";
import { IconX } from "@/lib/icons";

export type ToastKind = "success" | "error" | "info";
export interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
  /** Optional action button (e.g. Undo / Retry). */
  action?: { label: string; run: () => void };
  /** A summary of several errors folded together ("3 problems · Show"). */
  folded?: number;
}

// External store: dispatching a toast re-renders only <ToastHost>, never the
// component that fired it — so toasts are free to call from anywhere (including
// non-React mutation callbacks) with zero render cost on the caller side.
//
// One toast is visible at a time; the rest wait their turn (a burst of the
// same message counts once). Every error also lands in the error log, which
// stays reopenable after its toast is gone. An error is never dropped from
// the queue: when too many wait, notes and successes go first, then the
// waiting errors fold into one "N problems · Show" that opens the log.
let queue: Toast[] = [];
const listeners = new Set<() => void>();
let nextId = 1;
let timer: ReturnType<typeof setTimeout> | undefined;

function emit() {
  // New array identity so useSyncExternalStore detects the change.
  queue = queue.slice();
  for (const l of listeners) l();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

const DEFAULT_TTL: Record<ToastKind, number> = {
  success: 3000,
  info: 4000,
  error: 7000, // errors linger longer so they're not missed
};

/// How many toasts may wait behind the visible one. Older notes and
/// successes drop first; errors fold into one summary instead.
const MAX_WAITING = 3;

const ttls = new Map<number, number>();

/// Opens the error log (the app sets it once; the summary's Show uses it).
let showErrorLog: (() => void) | undefined;
export function setShowErrorLog(fn: (() => void) | undefined) {
  showErrorLog = fn;
}

function foldedToast(id: number, count: number): Toast {
  return {
    id,
    kind: "error",
    message: `${count} problems`,
    folded: count,
    action: { label: "Show", run: () => showErrorLog?.() },
  };
}

/// Trim the waiting line to MAX_WAITING without losing an error.
function trim(waiting: Toast[]): Toast[] {
  const out = waiting.slice();
  while (out.length > MAX_WAITING) {
    const quiet = out.findIndex((t) => t.kind !== "error");
    if (quiet >= 0) {
      ttls.delete(out[quiet].id);
      out.splice(quiet, 1);
      continue;
    }
    // Only errors wait: fold the oldest one into the summary (made from the
    // two oldest the first time).
    const at = out.findIndex((t) => t.folded !== undefined);
    if (at >= 0) {
      const victim = out.findIndex((t, i) => i !== at && t.folded === undefined);
      ttls.delete(out[victim].id);
      out[at] = foldedToast(out[at].id, (out[at].folded ?? 0) + 1);
      out.splice(victim, 1);
    } else {
      const [a, b] = [out[0], out[1]];
      ttls.delete(a.id);
      ttls.delete(b.id);
      out.splice(0, 2, foldedToast(nextId++, 2));
    }
  }
  return out;
}

/// Start the visible toast's clock (once it is the one on screen).
function arm() {
  if (timer) clearTimeout(timer);
  timer = undefined;
  const head = queue[0];
  if (!head) return;
  const life = ttls.get(head.id) ?? DEFAULT_TTL[head.kind];
  if (life > 0) timer = setTimeout(() => dismissToast(head.id), life);
}

export function dismissToast(id: number) {
  const wasHead = queue[0]?.id === id;
  queue = queue.filter((t) => t.id !== id);
  ttls.delete(id);
  for (const l of listeners) l();
  if (wasHead) arm();
}

function push(kind: ToastKind, message: string, action?: Toast["action"], ttl?: number) {
  const same = queue.find((t) => t.kind === kind && t.message === message);
  if (same) return same.id;
  if (kind === "error") logError(message);
  const id = nextId++;
  if (ttl !== undefined) ttls.set(id, ttl);
  const head = queue[0];
  const waiting = trim([...queue.slice(1), { id, kind, message, action }]);
  queue = head ? [head, ...waiting] : waiting;
  emit();
  if (!head) arm();
  return id;
}

/** Fire-and-forget toast API; safe to call from anywhere. */
export const toast = {
  success: (message: string, action?: Toast["action"]) => push("success", message, action),
  error: (message: string, action?: Toast["action"]) => push("error", message, action),
  info: (message: string, action?: Toast["action"]) => push("info", message, action),
};

// --- Error log --------------------------------------------------------------

export interface LoggedError {
  id: number;
  message: string;
  /** Epoch ms. */
  at: number;
}

/// Most recent last; the oldest drop beyond the cap.
const LOG_CAP = 50;
let errorLog: LoggedError[] = [];
let unseenErrors = 0;
const logListeners = new Set<() => void>();
let logSnapshot = { entries: errorLog, unseen: 0 };

function emitLog() {
  logSnapshot = { entries: errorLog, unseen: unseenErrors };
  for (const l of logListeners) l();
}

function logError(message: string) {
  errorLog = [...errorLog, { id: nextId++, message, at: Date.now() }].slice(-LOG_CAP);
  unseenErrors += 1;
  emitLog();
}

export const errorLogStore = {
  subscribe(l: () => void) {
    logListeners.add(l);
    return () => {
      logListeners.delete(l);
    };
  },
  snapshot() {
    return logSnapshot;
  },
  /// The person opened the log.
  markSeen() {
    if (unseenErrors === 0) return;
    unseenErrors = 0;
    emitLog();
  },
  clear() {
    errorLog = [];
    unseenErrors = 0;
    emitLog();
  },
};

/** The error log (most recent last) and how many errors arrived since it was last opened. */
export function useErrorLog() {
  return useSyncExternalStore(errorLogStore.subscribe, errorLogStore.snapshot, errorLogStore.snapshot);
}

/** Tests only: empty the queue and the log. */
export function _resetToasts() {
  if (timer) clearTimeout(timer);
  timer = undefined;
  queue = [];
  ttls.clear();
  errorLog = [];
  unseenErrors = 0;
  emit();
  emitLog();
}

/** Coerce an unknown thrown value into a readable message. */
export function errMsg(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  try {
    return JSON.stringify(e);
  } catch {
    return String(e);
  }
}

const KIND_STYLE: Record<ToastKind, { bar: string; label: string }> = {
  success: { bar: "var(--ligus-success)", label: "Done" },
  error: { bar: "var(--ligus-danger)", label: "Error" },
  info: { bar: "var(--ligus-accent-cool)", label: "Note" },
};

/** Mount once near the app root. Shows one toast at a time, top right under
 *  the header, clear of the composer and the window's edges. */
export function ToastHost() {
  const items = useSyncExternalStore(subscribe, () => queue);
  const t = items[0];
  if (!t) return null;
  const s = KIND_STYLE[t.kind];
  const waiting = items.length - 1;
  return (
    <div className="pointer-events-none fixed right-4 top-[68px] z-[1000] flex w-[300px] max-w-[calc(100vw-2rem)] flex-col">
      <div
        key={t.id}
        role={t.kind === "error" ? "alert" : "status"}
        className="ligus-enter pointer-events-auto flex items-start gap-3 rounded-xl border px-3 py-2.5 text-sm shadow-lg"
        style={{
          borderColor: "var(--ligus-line)",
          background: "var(--ligus-panel)",
          color: "var(--ligus-ink)",
          borderLeft: `3px solid ${s.bar}`,
        }}
      >
        <div className="min-w-0 flex-1">
          <div className="text-[11px] font-semibold uppercase tracking-[0.12em] text-ink-soft">
            {s.label}
            {waiting > 0 && <span className="ml-1.5 font-normal normal-case tracking-normal">· {waiting} more</span>}
          </div>
          <div className="mt-0.5 break-words">{t.message}</div>
        </div>
        {t.action && (
          <button
            className="shrink-0 rounded-md px-2 py-1 text-xs font-medium text-ink-soft underline hover:text-ink"
            onClick={() => {
              t.action!.run();
              dismissToast(t.id);
            }}
          >
            {t.action.label}
          </button>
        )}
        <button
          className="shrink-0 px-1 leading-none text-ink-soft hover:text-ink"
          aria-label="Dismiss"
          onClick={() => dismissToast(t.id)}
        >
          <IconX size={13} />
        </button>
      </div>
    </div>
  );
}
