import { describe, it, expect, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RunOthersAgentsRow } from "./RunOthersAgentsRow";
import * as ipc from "@/lib/ipc";

let listed = false;
vi.mock("@/lib/ipc", () => ({
  getRunOthersAgents: vi.fn(async () => listed),
  setRunOthersAgents: vi.fn(async (v: boolean) => {
    listed = v;
  }),
}));

describe("RunOthersAgentsRow", () => {
  it("is off by default and turns on", async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={qc}>
        <RunOthersAgentsRow />
      </QueryClientProvider>,
    );
    const sw = await screen.findByRole("switch", { name: "Run other people's agents on this computer" });
    expect(sw.getAttribute("aria-checked")).toBe("false");
    await userEvent.click(sw);
    expect(ipc.setRunOthersAgents).toHaveBeenCalledWith(true);
    await waitFor(() => expect(screen.getByRole("switch").getAttribute("aria-checked")).toBe("true"));
    // One row, one control: nothing else to read.
    expect(screen.getAllByRole("switch")).toHaveLength(1);
  });
});
