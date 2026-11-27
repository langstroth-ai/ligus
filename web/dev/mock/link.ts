// Dev-only: "I already use Ligus" (the desktop linking itself) with demo
// data, for previews and screenshots. The flow moves on by itself a moment
// after each step, the way the other device would answer. Knob (URL query):
//   &link=nomailbox   (the relay can't carry requests: the paste path)

import { emit } from "./event";

type Args = Record<string, unknown> | undefined;

const params = new URLSearchParams(window.location.search);
const noMailbox = params.get("link") === "nomailbox";

const base = {
  phase: "idle",
  request: null as string | null,
  fingerprint: null as string | null,
  label: "Ana's MacBook Air",
  code: null as string | null,
  codeSpelled: null as string | null,
  claimedName: null as string | null,
  claimedFingerprint: null as string | null,
  personName: null as string | null,
  workspaceName: null as string | null,
  workspaceId: null as string | null,
  approverFingerprint: null as string | null,
  notice: null as string | null,
  error: null as string | null,
  canRetry: false,
};
let flow = { ...base };

function set(next: Partial<typeof base>) {
  flow = { ...flow, ...next };
  return flow;
}

function later(next: Partial<typeof base>) {
  setTimeout(() => void emit("link://changed", set(next)), 1500);
}

const REQUEST = "liguslink1:eyJ2IjoxLCJkZW1vIjp0cnVlfQ";

export function linkMock(cmd: string, args: Args): { handled: boolean; value?: unknown } {
  switch (cmd) {
    case "link_request_status":
      return { handled: true, value: flow };
    case "link_request_start":
      if (noMailbox) {
        return {
          handled: true,
          value: set({ ...base, phase: "noMailbox", error: "Your relay can't pass requests between devices. Use a request code instead." }),
        };
      }
      later({ phase: "code", code: "K7Q4M9X2", claimedName: "Ana", claimedFingerprint: "7F3A-91C0-77DE-0B14-5E62" });
      return { handled: true, value: set({ ...base, phase: "waiting", request: REQUEST, fingerprint: "2C4E-6A8B-0D1F-3E5A-7C9B" }) };
    case "link_request_codes_match":
      if (!args?.matches) return { handled: true, value: set({ phase: "failed", error: "The codes don't match, so nothing was granted.", canRetry: true }) };
      later({ phase: "confirmPerson", personName: "Ana", workspaceName: "Apiary" });
      return { handled: true, value: set({ phase: "approving" }) };
    case "link_request_link_as":
      if (!args?.yes) return { handled: true, value: set({ phase: "failed", error: "Not linked. Nothing was saved on this computer.", canRetry: true }) };
      later({ phase: "linked", workspaceId: "ws-1" });
      return { handled: true, value: set({ phase: "linking" }) };
    case "link_request_cancel":
      return { handled: true, value: set({ ...base }) };
    case "link_request_paste":
      return { handled: true, value: set({ ...base, phase: "paste", request: REQUEST, fingerprint: "2C4E-6A8B-0D1F-3E5A-7C9B" }) };
    case "link_request_open_grant":
      return {
        handled: true,
        value: set({ phase: "confirmApprover", personName: "Ana", workspaceName: "Apiary", approverFingerprint: "7F3A-91C0-77DE-0B14-5E62" }),
      };
    case "link_request_redeem":
      return { handled: true, value: set({ phase: "linked", personName: "Ana", workspaceId: "ws-1" }) };
    default:
      return { handled: false };
  }
}
