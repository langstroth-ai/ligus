/// First-run state (onboarding done, tour seen) follows Ligus's own data dir:
/// the backend keeps it in settings.json and returns it from getAppSettings.
/// It deliberately ignores the webview's localStorage, which outlives a deleted
/// or factory-reset data dir (WebKit keeps it in its own folder), so a fresh
/// identity always gets the first-run flow.

type Gate = { onboarded: boolean } | null | undefined;

/// Show onboarding until the backend says this data dir finished it. A display
/// name left over from anywhere does not count.
export function needsOnboarding(settings: Gate): boolean {
  return settings != null && !settings.onboarded;
}

/// Auto-run the tour once per data dir, after onboarding.
export function shouldAutoTour(settings: { onboarded: boolean; tourSeen: boolean } | null | undefined): boolean {
  return settings != null && settings.onboarded && !settings.tourSeen;
}

/// The first run's screens, in order. It opens on `fork` ("Is this your
/// first device with Ligus?"):
///
/// - `link`: this device belongs to someone who already uses Ligus. It is
///   linked to their account, or nothing happens: no name is asked and no
///   workspace is made or joined.
/// - `you` → `secure` → `agent`: a first device. Secure (a passkey, then the
///   recovery key) can't be left half-done: its only way out before it is
///   finished is `finishLater`, which asks first.
export type FirstRunStep = "fork" | "link" | "you" | "secure" | "finishLater" | "agent";

export type FirstRunEvent =
  | "chooseFirst"
  | "chooseLink"
  | "back"
  | "named"
  | "secured"
  | "askFinishLater"
  | "keepGoing"
  | "finishLater";

/// The screen after `event` on `step`. An event a screen doesn't take
/// leaves it where it is.
export function firstRunNext(step: FirstRunStep, event: FirstRunEvent): FirstRunStep {
  switch (step) {
    case "fork":
      if (event === "chooseFirst") return "you";
      if (event === "chooseLink") return "link";
      return step;
    case "link":
      return event === "back" ? "fork" : step;
    case "you":
      if (event === "back") return "fork";
      if (event === "named") return "secure";
      return step;
    case "secure":
      // No Back: the workspace exists now, and Secure is finished or put off.
      if (event === "secured") return "agent";
      if (event === "askFinishLater") return "finishLater";
      return step;
    case "finishLater":
      if (event === "keepGoing") return "secure";
      if (event === "finishLater") return "agent";
      return step;
    case "agent":
      return step;
  }
}

/// Which progress dot a screen lights (the fork and link screens show none).
export function firstRunDot(step: FirstRunStep): number {
  switch (step) {
    case "fork":
    case "link":
      return 0;
    case "you":
      return 1;
    case "secure":
    case "finishLater":
      return 2;
    case "agent":
      return 3;
  }
}

export const FIRST_RUN_DOTS = 3;

/// The name of the workspace the first run makes for the person.
export function firstWorkspaceName(displayName: string): string {
  const first = displayName.trim().split(/\s+/)[0] ?? "";
  return first ? `${first}'s workspace` : "My workspace";
}
