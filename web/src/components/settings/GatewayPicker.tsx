import type { WorkspaceHostDto } from "@/bindings/WorkspaceHostDto";

/// One model gateway as hosts offer it: its name, its wire, and the hosts
/// (labels) that offer it. Names come from each host's operator config; no
/// URL or credential ever reaches this UI.
export interface GatewayOffer {
  name: string;
  api: string;
  hosts: string[];
}

/// The union of the gateways the workspace's hosts offer, by name.
export function gatewayOffers(hosts: WorkspaceHostDto[]): GatewayOffer[] {
  const by = new Map<string, GatewayOffer>();
  for (const h of hosts) {
    for (const g of h.modelGateways ?? []) {
      const o = by.get(g.name) ?? { name: g.name, api: g.api, hosts: [] };
      const label = h.label || h.id;
      if (!o.hosts.includes(label)) o.hosts.push(label);
      by.set(g.name, o);
    }
  }
  return [...by.values()].sort((a, b) => a.name.localeCompare(b.name));
}

/// The models hosts report for gateway `name` (from their signed beats),
/// deduplicated in first-seen order. Empty when no host reports any: the
/// model field stays free text.
export function gatewayModels(hosts: WorkspaceHostDto[], name: string): string[] {
  const out: string[] = [];
  for (const h of hosts) {
    for (const o of h.offers ?? []) {
      if (o.kind !== "gateway" || o.name !== name) continue;
      for (const m of o.models ?? []) if (!out.includes(m)) out.push(m);
    }
  }
  return out;
}

/// "litellm (openai), claude-gw (anthropic)" for a host row.
export function hostGatewaysLabel(h: WorkspaceHostDto): string {
  return (h.modelGateways ?? []).map((g) => `${g.name} (${g.api})`).join(", ");
}

/// Picks a model gateway by name from what hosts offer. Renders nothing when
/// no host offers one (the plain endpoint path is unchanged).
export function GatewayPicker({
  hosts,
  value,
  onChange,
  style,
}: {
  hosts: WorkspaceHostDto[];
  value: string;
  onChange: (name: string) => void;
  style?: React.CSSProperties;
}) {
  const offers = gatewayOffers(hosts);
  if (offers.length === 0 && !value) return null;
  const picked = offers.find((o) => o.name === value);
  return (
    <div className="space-y-1">
      <label className="block text-xs text-ink-soft" htmlFor="runtime-gateway">
        Model gateway
      </label>
      <select
        id="runtime-gateway"
        aria-label="Model gateway"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="w-full rounded-xl border px-3 py-2 text-sm"
        style={style}
      >
        <option value="">None (use the endpoint)</option>
        {offers.map((o) => (
          <option key={o.name} value={o.name}>
            {o.name} ({o.api}) · {o.hosts.join(", ")}
          </option>
        ))}
        {value && !picked && <option value={value}>{value} (no host offers it now)</option>}
      </select>
      {value && (
        <p className="text-xs text-ink-soft" data-testid="gateway-note">
          Runs only on hosts that offer this gateway: {picked ? picked.hosts.join(", ") : "none right now"}; credentials stay
          on those hosts. Add an agent picks one of them for you.
        </p>
      )}
    </div>
  );
}
