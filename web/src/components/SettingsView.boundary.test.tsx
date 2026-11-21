import { describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";

// Repositories throws on render (as it did under the mock backend with no repos);
// Appearance renders normally, to show the rest of Settings keeps working.
vi.mock("@/components/settings/RepositoriesSection", () => ({
  RepositoriesSection: () => {
    throw new Error("repos payload missing");
  },
}));
vi.mock("@/components/settings/AppearanceSection", () => ({
  AppearanceSection: () => <div>appearance body</div>,
}));

import { SettingsView } from "./SettingsView";

describe("SettingsView section error boundary", () => {
  it("contains a crashing section to its own card, with retry, and keeps the nav working", () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    render(
      <SettingsView
        palette="pollen"
        onPaletteChange={() => {}}
        appearanceMode="light"
        onAppearanceModeChange={() => {}}
        initialTab="repositories"
        onClose={() => {}}
      />,
    );
    expect(screen.getByRole("alert")).toHaveTextContent("Repositories hit a problem.");
    expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
    expect(screen.queryByText("Something went wrong")).not.toBeInTheDocument();
    // The rest of Settings is still usable: switch to another section.
    fireEvent.click(screen.getByRole("button", { name: "Appearance" }));
    expect(screen.getByText("appearance body")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    spy.mockRestore();
  });
});
