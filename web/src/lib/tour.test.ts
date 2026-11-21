import { describe, expect, it } from "vitest";
import { CANVAS_TAB_HINT, TOUR_CANVAS_TABS, TOUR_DIFF } from "./tour";

// The Diff tab shows the workspace's uncommitted git changes (DiffView →
// get_workspace_diffs), not agents' proposals; the tour must say so.
describe("tour copy for the Diff tab", () => {
  it("describes uncommitted changes, not proposals", () => {
    expect(TOUR_DIFF.body).toMatch(/not yet committed/);
    expect(TOUR_DIFF.body).not.toMatch(/every proposed file change/i);
    expect(TOUR_DIFF.body).toMatch(/Review pane/);
    expect(TOUR_CANVAS_TABS.body).toMatch(/uncommitted changes/);
    expect(TOUR_CANVAS_TABS.body).not.toMatch(/proposed changes/);
  });
  it("labels the Diff tab the same way", () => {
    expect(CANVAS_TAB_HINT.diff).toMatch(/Uncommitted changes/);
  });
});
