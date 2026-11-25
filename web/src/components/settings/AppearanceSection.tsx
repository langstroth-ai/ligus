import { useRef, useState } from "react";
import { THEMES, systemPrefersDark, type AppearanceMode, type ThemeName } from "@/lib/theme";
import { DENSITIES, applyDensity, loadDensity, saveDensity, type Density } from "@/lib/density";
import { Button, InfoTip, ListRow, RowList, Section } from "@/components/ui";
import { confirmDialog, promptDialog } from "@/components/Dialog";
import { errMsg, toast } from "@/components/Toast";
import { exportThemeFile } from "@/lib/ipc";
import {
  MAX_THEME_BYTES,
  addCustomTheme,
  exportFileName,
  exportThemeToml,
  parseThemeFile,
  removeCustomTheme,
  renameCustomTheme,
  resolveCustom,
  setActiveCustomTheme,
  useCustomThemes,
  type CustomTheme,
} from "@/lib/customThemes";

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
  const custom = useCustomThemes();
  const active = custom.active;
  const pickBuiltIn = (t: ThemeName) => {
    setActiveCustomTheme(null);
    onPaletteChange(t);
  };
  const pickDensity = (d: Density) => {
    setDensity(d);
    saveDensity(d);
    applyDensity(d);
  };
  return (
    <Section title="Appearance">
      <div className="flex items-center gap-1.5">
        <label className="block text-sm text-ink-soft">Mode</label>
        <InfoTip
          label="Mode"
          text="Auto follows your operating system's light/dark setting. Every theme has a light and a dark variant."
        />
      </div>
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

      <label className="mt-3 block text-sm text-ink-soft">Theme</label>
      <div className="flex flex-wrap gap-2">
        {(Object.keys(THEMES) as ThemeName[]).map((t) => (
          <button
            key={t}
            onClick={() => pickBuiltIn(t)}
            aria-pressed={!active && t === palette}
            className="flex items-center gap-2 rounded-xl border px-3 py-2 capitalize"
            style={{
              borderColor: !active && t === palette ? "var(--ligus-accent-cool)" : "var(--ligus-line)",
              background: !active && t === palette ? "var(--ligus-mist)" : "transparent",
            }}
          >
            <span className="h-3 w-3 rounded-full" style={{ background: THEMES[t].light.accentCool }} />
            {t}
          </button>
        ))}
      </div>

      <CustomThemes palette={palette} onPaletteChange={onPaletteChange} appearanceMode={appearanceMode} />

      <div className="mt-3 flex items-center gap-1.5">
        <label className="block text-sm text-ink-soft" id="density-label">
          Density
        </label>
        <InfoTip
          label="Density"
          text="How much room rows, cards and messages take. Separate from zoom (⌘+ and ⌘−)."
        />
      </div>
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
    </Section>
  );
}

type ImportNote = { title: string; lines: string[]; error: boolean };

/// Imported themes (this device only), Import and Export. A theme file is
/// colours only; lib/customThemes.ts reads and checks it.
function CustomThemes({
  palette,
  onPaletteChange,
  appearanceMode,
}: {
  palette: ThemeName;
  onPaletteChange: (t: ThemeName) => void;
  appearanceMode: AppearanceMode;
}) {
  const { themes, active } = useCustomThemes();
  const fileRef = useRef<HTMLInputElement>(null);
  const [note, setNote] = useState<ImportNote | null>(null);

  const use = (t: CustomTheme) => {
    setActiveCustomTheme(t.id);
    onPaletteChange(t.extends);
  };

  const importFile = async (f: File | undefined) => {
    if (!f) return;
    if (f.size > MAX_THEME_BYTES) {
      setNote({ title: `Couldn't import ${f.name}`, lines: [`The file is larger than ${MAX_THEME_BYTES / 1024} KB.`], error: true });
      return;
    }
    let text: string;
    try {
      text = await f.text();
    } catch (e) {
      setNote({ title: `Couldn't read ${f.name}`, lines: [errMsg(e)], error: true });
      return;
    }
    const r = parseThemeFile(text, f.name);
    if (!r.ok) {
      setNote({ title: `Couldn't import ${f.name}`, lines: [...r.errors, ...r.warnings], error: true });
      return;
    }
    try {
      const t = addCustomTheme(r.theme);
      onPaletteChange(t.extends);
      setNote(r.warnings.length ? { title: `Imported ${t.name}`, lines: r.warnings, error: false } : null);
      if (!r.warnings.length) toast.success(`Imported ${t.name}`);
    } catch (e) {
      setNote({ title: `Couldn't import ${f.name}`, lines: [errMsg(e)], error: true });
    }
  };

  const exportCurrent = async () => {
    const source = active ?? {
      name: `${palette[0].toUpperCase()}${palette.slice(1)} (mine)`,
      extends: palette as CustomTheme["extends"],
      light: {},
      dark: {},
    };
    try {
      const path = await exportThemeFile(exportFileName(source.name), exportThemeToml(source));
      if (path) toast.success(`Saved ${path}`);
    } catch (e) {
      toast.error(`Couldn't save the theme: ${errMsg(e)}`);
    }
  };

  const variant = appearanceMode === "dark" || (appearanceMode === "auto" && systemPrefersDark()) ? "dark" : "light";
  return (
    <div className="mt-3">
      {themes.length > 0 && (
        <RowList label="Your themes" className="mb-2">
          {themes.map((t) => {
            const { palette: p } = resolveCustom(t, variant);
            return (
              <ListRow
                key={t.id}
                testId={`custom-theme-${t.id}`}
                leading={<span className="h-3 w-3 rounded-full" style={{ background: p.accentCool }} />}
                title={t.name}
                meta={`based on ${t.extends}`}
                selected={active?.id === t.id}
                trailing={active?.id === t.id ? <span className="text-xs text-ink-soft">In use</span> : undefined}
                onClick={() => use(t)}
                actions={[
                  {
                    label: "Rename",
                    onSelect: async () => {
                      const name = await promptDialog("Theme name", { defaultValue: t.name });
                      if (name === null || name.trim() === t.name) return;
                      if (!renameCustomTheme(t.id, name)) toast.error("Use 1 to 40 characters.");
                    },
                  },
                  {
                    label: "Remove",
                    danger: true,
                    onSelect: async () => {
                      const ok = await confirmDialog("It goes from this device. Your file stays where it is.", {
                        title: `Remove ${t.name}?`,
                        confirmLabel: "Remove",
                        danger: true,
                      });
                      if (ok) removeCustomTheme(t.id);
                    },
                  },
                ]}
              />
            );
          })}
        </RowList>
      )}
      <div className="flex flex-wrap items-center gap-2">
        <Button onClick={() => fileRef.current?.click()}>Import a theme…</Button>
        <Button onClick={() => void exportCurrent()}>Export current theme</Button>
        <InfoTip
          label="Custom themes"
          text="A theme file (TOML or JSON) sets colours for light, dark or both; lt reads the same file. Imported themes stay on this device."
        />
        <input
          ref={fileRef}
          type="file"
          accept=".toml,.json,application/json,application/toml,text/plain"
          className="hidden"
          data-testid="theme-file"
          onChange={(e) => {
            const f = e.target.files?.[0];
            e.target.value = "";
            void importFile(f);
          }}
        />
      </div>
      {note && (
        <div
          role={note.error ? "alert" : "status"}
          className="mt-2 rounded-xl border px-3 py-2 text-xs"
          style={{ borderColor: note.error ? "var(--ligus-danger)" : "var(--ligus-warn)" }}
        >
          <div className="flex items-baseline justify-between gap-2">
            <span className="font-medium">{note.title}</span>
            <button type="button" className="text-ink-soft underline" onClick={() => setNote(null)}>
              Dismiss
            </button>
          </div>
          <ul className="mt-1 list-disc pl-4 text-ink-soft">
            {note.lines.slice(0, 12).map((l, i) => (
              <li key={i}>{l}</li>
            ))}
            {note.lines.length > 12 && <li>and {note.lines.length - 12} more</li>}
          </ul>
        </div>
      )}
    </div>
  );
}
