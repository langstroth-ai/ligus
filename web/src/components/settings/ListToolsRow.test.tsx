import { describe, it, expect, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ListToolsRow } from "./ListToolsRow";
import * as ipc from "@/lib/ipc";

let listed = true;
vi.mock("@/lib/ipc", () => ({
  getListTools: vi.fn(async () => listed),
  setListTools: vi.fn(async (v: boolean) => {
    listed = v;
  }),
}));

describe("ListToolsRow", () => {
  it("turns listing this computer's tools off and on", async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={qc}>
        <ListToolsRow />
      </QueryClientProvider>,
    );
    const sw = await screen.findByRole("switch", { name: "List this computer's tools" });
    expect(sw.getAttribute("aria-checked")).toBe("true");
    await userEvent.click(sw);
    expect(ipc.setListTools).toHaveBeenCalledWith(false);
    await waitFor(() => expect(screen.getByRole("switch").getAttribute("aria-checked")).toBe("false"));
    // One row, one control: nothing else to read.
    expect(screen.getAllByRole("switch")).toHaveLength(1);
  });
});
