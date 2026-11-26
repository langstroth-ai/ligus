import type { WorkspaceHostDto } from "@/bindings/WorkspaceHostDto";

/// One choice in Add an agent's "Runs on" picker. `id` "" is this device.
export interface AgentHostOption {
  id: string;
  label: string;
  /// Offers the runtime's gateway (always true without a gateway).
  offersGateway: boolean;
}

function hostLabel(h: WorkspaceHostDto): string {
  const name = h.label || `${h.id.slice(0, 8)}…`;
  const whose = h.kind === "device" ? (h.mine ? " · your device" : h.ownerName ? ` · ${h.ownerName}'s device` : "") : "";
  return `${name}${whose}${h.online ? "" : " (offline)"}`;
}

/// Where a new agent can run: this device, every always-on box (serve and
/// worker hosts both register as workers), then your other devices (an lt or
/// a second desktop). Other people's devices are left out: an agent there
/// runs only while they're at their computer.
export function agentHostOptions(
  hosts: WorkspaceHostDto[],
  thisDeviceId: string | null,
  gateway?: string | null,
): AgentHostOption[] {
  const offers = (h: WorkspaceHostDto) => !gateway || (h.modelGateways ?? []).some((g) => g.name === gateway);
  const boxes = hosts.filter((h) => h.kind === "worker");
  const mine = hosts.filter((h) => h.kind === "device" && h.mine && h.id !== thisDeviceId);
  return [
    { id: "", label: "This device", offersGateway: !gateway },
    ...[...boxes, ...mine].map((h) => ({ id: h.id, label: hostLabel(h), offersGateway: offers(h) })),
  ];
}

/// The default "Runs on": this device, except for a gateway runtime, which
/// runs only where the gateway is: the one host offering it (an online one
/// first), or this device when none or several do (the person picks).
export function defaultAgentHost(hosts: WorkspaceHostDto[], gateway?: string | null): string {
  if (!gateway) return "";
  const offering = hosts.filter((h) => (h.modelGateways ?? []).some((g) => g.name === gateway));
  if (offering.length === 1) return offering[0].id;
  const online = offering.filter((h) => h.online);
  return online.length === 1 ? online[0].id : "";
}
