import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

const exportThemeFile = vi.fn(async (_name: string, _contents: string) => "/tmp/pollen-mine.toml");
vi.mock("@/lib/ipc", () => ({ exportThemeFile: (n: string, c: string) => exportThemeFile(n, c), setTitlebarColor: vi.fn(async () => {}) }));
vi.mock("@/components/Toast", () => ({ toast: { success: vi.fn(), error: vi.fn() }, errMsg: (e: unknown) => String(e) }));

import { AppearanceSection } from "./AppearanceSection";
import { customThemes, reloadCustomThemes } from "@/lib/customThemes";

function setup() {
  const onPaletteChange = vi.fn();
  render(<AppearanceSection palette="pollen" onPaletteChange={onPaletteChange} appearanceMode="light" onAppearanceModeChange={vi.fn()} />);
  return { onPaletteChange };
}

const file = (text: string, name: string) => new File([text], name, { type: "text/plain" });

describe("custom themes in Appearance", () => {
  beforeEach(() => {
    localStorage.clear();
    reloadCustomThemes();
    exportThemeFile.mockClear();
  });

  it("imports a file, lists it, uses it and shows contrast warnings", async () => {
    const { onPaletteChange } = setup();
    const input = screen.getByTestId("theme-file") as HTMLInputElement;
    await userEvent.upload(input, file(`extends = "harbor"\n[light]\ncanvas = "#ffffff"\nink = "#cccccc"\n`, "Fog.toml"));
    await screen.findByText("Imported Fog");
    expect(screen.getByRole("status").textContent).toMatch(/ink on canvas/);
    expect(screen.getByText("Fog")).toBeTruthy();
    expect(onPaletteChange).toHaveBeenCalledWith("harbor");
    expect(customThemes().active?.name).toBe("Fog");
    // The built-in buttons no longer show as picked.
    expect(screen.getByRole("button", { name: /pollen/i }).getAttribute("aria-pressed")).toBe("false");
  });

  it("refuses a bad file with the reason and keeps nothing", async () => {
    setup();
    await userEvent.upload(screen.getByTestId("theme-file") as HTMLInputElement, file(`[light]\nink = "red; } body { background: url(x) }"`, "Evil.toml"));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/line 2: light\.ink/);
    expect(customThemes().themes).toEqual([]);
  });

  it("refuses a huge file without reading it", async () => {
    setup();
    const big = file("#".repeat(70 * 1024), "big.toml");
    const text = vi.spyOn(big, "text");
    await userEvent.upload(screen.getByTestId("theme-file") as HTMLInputElement, big);
    expect((await screen.findByRole("alert")).textContent).toMatch(/larger than 64 KB/);
    expect(text).not.toHaveBeenCalled();
  });

  it("exports the current palette as a full theme file", async () => {
    setup();
    await userEvent.click(screen.getByRole("button", { name: "Export current theme" }));
    await waitFor(() => expect(exportThemeFile).toHaveBeenCalled());
    const [name, contents] = exportThemeFile.mock.calls[0];
    expect(name).toBe("pollen-mine.toml");
    expect(contents).toMatch(/^format = "ligus\.theme\.v1"$/m);
    expect(contents).toMatch(/^\[dark\]$/m);
  });
});
