import { describe, it, expect } from "vitest";
import type { WorkspaceHostDto } from "@/bindings/WorkspaceHostDto";
import { agentHostOptions, defaultAgentHost } from "./agentHost";

const host = (
  id: string,
  label: string,
  kind: "device" | "worker",
  opts: Partial<WorkspaceHostDto> = {},
): WorkspaceHostDto => ({
  id,
  kind,
  label,
  online: true,
  lastSeen: "",
  modelGateways: [],
  ownerName: "",
  offers: [],
  readiness: "",
  build: "",
  mine: false,
  ...opts,
});

const hosts = [
  host("me", "Desk", "device", { mine: true }),
  host("air", "lt on Air", "device", { mine: true, online: false }),
  host("ana", "Ana's laptop", "device", { ownerName: "Ana" }),
  host("box", "serve-1", "worker", { modelGateways: [{ name: "litellm", api: "openai" }] }),
  host("w2", "build-1", "worker"),
];

describe("agentHostOptions", () => {
  it("lists this device, every box, then your other devices, never someone else's", () => {
    const o = agentHostOptions(hosts, "me");
    expect(o.map((x) => x.id)).toEqual(["", "box", "w2", "air"]);
    expect(o[0].label).toBe("This device");
    expect(o.find((x) => x.id === "air")?.label).toBe("lt on Air · your device (offline)");
  });
  it("marks which hosts offer the runtime's gateway", () => {
    const o = agentHostOptions(hosts, "me", "litellm");
    expect(o.filter((x) => x.offersGateway).map((x) => x.id)).toEqual(["box"]);
  });
});

describe("defaultAgentHost", () => {
  it("is this device for a runtime without a gateway", () => {
    expect(defaultAgentHost(hosts, null)).toBe("");
  });
  it("is the only host offering the gateway", () => {
    expect(defaultAgentHost(hosts, "litellm")).toBe("box");
  });
  it("leaves the choice to the person when several hosts offer it", () => {
    const two = [...hosts, host("w3", "gpu", "worker", { modelGateways: [{ name: "litellm", api: "openai" }] })];
    expect(defaultAgentHost(two, "litellm")).toBe("");
  });
  it("prefers the one online host when several offer it", () => {
    const two = [...hosts, host("w3", "gpu", "worker", { online: false, modelGateways: [{ name: "litellm", api: "openai" }] })];
    expect(defaultAgentHost(two, "litellm")).toBe("box");
  });
});
