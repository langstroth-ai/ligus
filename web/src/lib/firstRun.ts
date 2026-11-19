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
