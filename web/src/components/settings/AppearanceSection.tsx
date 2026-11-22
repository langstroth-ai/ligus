import { useState } from "react";
import { THEMES, type AppearanceMode, type ThemeName } from "@/lib/theme";
import { DENSITIES, applyDensity, loadDensity, saveDensity, type Density } from "@/lib/density";
import { Section } from "@/components/ui";

/// Appearance: light/dark mode, accent family and density. Every control
/// applies live.
export function AppearanceSection({
  palette,
  onPaletteChange,
  appearanceMode,
  onAppearanceModeChange,
}: {
  palette: ThemeName;
  onPaletteChange: (t: ThemeName) => void;
  appearanceMode: AppearanceMode;
  onAppearanceModeChange: (m: AppearanceMode) => void;
}) {
  const [density, setDensity] = useState<Density>(loadDensity);
  const pickDensity = (d: Density) => {
    setDensity(d);
    saveDensity(d);
    applyDensity(d);
  };
  return (
    <Section title="Appearance">
      <label className="block text-sm text-ink-soft">Mode</label>
      <div className="flex gap-2">
        {(["auto", "light", "dark"] as AppearanceMode[]).map((m) => (
          <button
            key={m}
            onClick={() => onAppearanceModeChange(m)}
            className="rounded-xl border px-3 py-2 capitalize"
            style={{
              borderColor: m === appearanceMode ? "var(--ligus-accent-cool)" : "var(--ligus-line)",
              background: m === appearanceMode ? "var(--ligus-mist)" : "transparent",
            }}
          >
            {m === "auto" ? "Auto (system)" : m}
          </button>
        ))}
      </div>
      <p className="text-xs text-ink-soft">
        Auto follows your operating system's light/dark setting. Every theme has a light and a dark
        variant.
      </p>

      <label className="mt-3 block text-sm text-ink-soft">Theme</label>
      <div className="flex flex-wrap gap-2">
        {(Object.keys(THEMES) as ThemeName[]).map((t) => (
          <button
            key={t}
            onClick={() => onPaletteChange(t)}
            className="flex items-center gap-2 rounded-xl border px-3 py-2 capitalize"
            style={{
              borderColor: t === palette ? "var(--ligus-accent-cool)" : "var(--ligus-line)",
              background: t === palette ? "var(--ligus-mist)" : "transparent",
            }}
          >
            <span className="h-3 w-3 rounded-full" style={{ background: THEMES[t].light.accentCool }} />
            {t}
          </button>
        ))}
      </div>

      <label className="mt-3 block text-sm text-ink-soft" id="density-label">
        Density
      </label>
      <div className="flex gap-2" role="radiogroup" aria-labelledby="density-label">
        {DENSITIES.map((d) => (
          <button
            key={d.value}
            role="radio"
            aria-checked={d.value === density}
            onClick={() => pickDensity(d.value)}
            className="rounded-xl border px-3 py-2"
            style={{
              borderColor: d.value === density ? "var(--ligus-accent-cool)" : "var(--ligus-line)",
              background: d.value === density ? "var(--ligus-mist)" : "transparent",
            }}
          >
            {d.label}
          </button>
        ))}
      </div>
      <p className="text-xs text-ink-soft">
        How much room rows, cards and messages take. Separate from zoom (⌘+ and ⌘−).
      </p>
    </Section>
  );
}
