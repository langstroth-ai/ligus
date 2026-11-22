// The banner budget: at most one banner across the top of the window, and
// only for a state that blocks work. Everything else goes to the Inbox
// (needs you) or the status bar (state).

export type BlockingBanner = "lockdown" | "dearth" | null;

/// Which blocking state gets the one banner. A lockdown of the open chat
/// wins: it stops that chat's agents now. Dearth mode, if it's also on,
/// moves to the status bar meanwhile.
export function blockingBanner(s: { lockdown: boolean; dearth: boolean }): BlockingBanner {
  if (s.lockdown) return "lockdown";
  if (s.dearth) return "dearth";
  return null;
}
