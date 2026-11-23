import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

vi.mock("@/lib/ipc", () => ({
  growthSuggestions: vi.fn(async () => [
    { id: "always-on-host", kind: "host", title: "Add an always-on host", reason: "2 requests are waiting." },
    { id: "second-agent", kind: "agent", title: "Add another agent", reason: "One agent wrote 90%." },
  ]),
}));

import { GrowthSuggestions } from "./GrowthSuggestions";

const wrap = (ui: React.ReactNode) =>
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>{ui}</QueryClientProvider>);

describe("GrowthSuggestions", () => {
  it("is a quiet tip, not a banner", async () => {
    wrap(<GrowthSuggestions onOpen={vi.fn()} />);
    const tip = await screen.findByRole("note", { name: "Tip" });
    expect(tip).toHaveTextContent("Add an always-on host.");
    // Banners announce themselves as status/alert; a tip doesn't compete.
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("shows one suggestion at a time and remembers dismissals", async () => {
    const onOpen = vi.fn();
    const { unmount } = wrap(<GrowthSuggestions onOpen={onOpen} />);
    await waitFor(() => expect(screen.getByText("Add an always-on host.")).toBeInTheDocument());
    await userEvent.click(screen.getByText("Set up"));
    expect(onOpen).toHaveBeenCalledWith("host");
    await userEvent.click(screen.getByLabelText("Dismiss suggestion"));
    expect(screen.getByText("Add another agent.")).toBeInTheDocument();
    unmount();
    wrap(<GrowthSuggestions onOpen={onOpen} />);
    await waitFor(() => expect(screen.getByText("Add another agent.")).toBeInTheDocument());
    expect(screen.queryByText("Add an always-on host.")).toBeNull();
  });
});
