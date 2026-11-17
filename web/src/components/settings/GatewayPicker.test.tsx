import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import type { WorkspaceHostDto } from "@/bindings/WorkspaceHostDto";
import { GatewayPicker, gatewayOffers, hostGatewaysLabel } from "./GatewayPicker";

const host = (id: string, label: string, gws: [string, string][]): WorkspaceHostDto => ({
  id,
  kind: "worker",
  label,
  online: true,
  lastSeen: "",
  modelGateways: gws.map(([name, api]) => ({ name, api })),
});

const hosts = [
  host("a", "serve-1", [["litellm", "openai"]]),
  host("b", "build-1", [["litellm", "openai"], ["claude-gw", "anthropic"]]),
  host("c", "laptop", []),
];

describe("GatewayPicker", () => {
  it("lists the union of offered gateways with the hosts offering each", () => {
    expect(gatewayOffers(hosts)).toEqual([
      { name: "claude-gw", api: "anthropic", hosts: ["build-1"] },
      { name: "litellm", api: "openai", hosts: ["serve-1", "build-1"] },
    ]);
    expect(hostGatewaysLabel(hosts[1])).toBe("litellm (openai), claude-gw (anthropic)");
    expect(hostGatewaysLabel(hosts[2])).toBe("");
  });

  it("renders nothing when no host offers a gateway", () => {
    const { container } = render(<GatewayPicker hosts={[hosts[2]]} value="" onChange={() => {}} />);
    expect(container.innerHTML).toBe("");
  });

  it("selecting a gateway sets it by name and explains where it runs", async () => {
    const onChange = vi.fn();
    function Harness() {
      const [v, setV] = useState("");
      return (
        <GatewayPicker
          hosts={hosts}
          value={v}
          onChange={(n) => {
            onChange(n);
            setV(n);
          }}
        />
      );
    }
    render(<Harness />);
    const select = screen.getByLabelText("Model gateway");
    expect(screen.getByRole("option", { name: "None (use the endpoint)" })).toBeTruthy();
    expect(screen.getByRole("option", { name: /litellm \(openai\) · serve-1, build-1/ })).toBeTruthy();
    expect(screen.queryByTestId("gateway-note")).toBeNull();
    await userEvent.selectOptions(select, "litellm");
    expect(onChange).toHaveBeenCalledWith("litellm");
    expect(screen.getByTestId("gateway-note").textContent).toContain("serve-1, build-1");
    expect(screen.getByTestId("gateway-note").textContent).toContain("credentials stay on those hosts");
    // No URL is ever shown.
    expect(document.body.textContent).not.toMatch(/https?:\/\//);
  });
});
