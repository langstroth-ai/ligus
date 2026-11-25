// The one action an empty pane offers: open the matching setup flow. Panes
// pass it as their EmptyHint's `action` (`setupAction` in lib/setup.ts is
// the same as data, for panes that render their own control).

import { Button } from "@/components/ui";
import { setupAction } from "@/lib/setup";
import type { SetupFlowId } from "@/lib/ipc";

export function SetupAction({ flow }: { flow: SetupFlowId }) {
  const a = setupAction(flow);
  return (
    <Button variant="quiet" onClick={a.onClick}>
      {a.label}
    </Button>
  );
}
