import { describe, expect, it } from "vitest";

import type {
  ReadingFigure,
  ReadingPage,
  ReadingSegment,
} from "./readingModel";
import {
  anchorPage,
  anchorTarget,
  INITIAL_TEACHER_ANCHOR,
  isRegionKind,
  spotlightEnabledFrom,
  spotlightPlan,
  teacherAnchorFrom,
  teacherAnchorReducer,
  type TeacherAnchor,
  type TeacherAnchorState,
} from "./teacherAnchorModel";

const anchor = (
  id: string,
  gen: number,
  kind: TeacherAnchor["kind"] = "figure",
): TeacherAnchor => ({ kind, id, gen });

const arrived = (
  state: TeacherAnchorState,
  next: TeacherAnchor,
): TeacherAnchorState =>
  teacherAnchorReducer(state, { type: "anchor", anchor: next });

describe("teacherAnchorFrom parses the Tauri payload defensively", () => {
  it("round-trips a valid payload", () => {
    expect(teacherAnchorFrom({ kind: "equation", id: "eq-4", gen: 2 })).toEqual(
      { kind: "equation", id: "eq-4", gen: 2 },
    );
  });

  it("refuses out-of-vocabulary kinds — §11.1 anchors are section/figure/equation/table only", () => {
    expect(teacherAnchorFrom({ kind: "page", id: "p-4", gen: 0 })).toBeNull();
    expect(teacherAnchorFrom({ kind: "word", id: "w-1", gen: 0 })).toBeNull();
  });

  it("refuses a missing or empty id", () => {
    expect(teacherAnchorFrom({ kind: "figure", gen: 0 })).toBeNull();
    expect(teacherAnchorFrom({ kind: "figure", id: "", gen: 0 })).toBeNull();
    expect(teacherAnchorFrom({ kind: "figure", id: 7, gen: 0 })).toBeNull();
  });

  it("refuses a generation that cannot order anything", () => {
    expect(teacherAnchorFrom({ kind: "figure", id: "f", gen: -1 })).toBeNull();
    expect(
      teacherAnchorFrom({ kind: "figure", id: "f", gen: Number.NaN }),
    ).toBeNull();
    expect(
      teacherAnchorFrom({
        kind: "figure",
        id: "f",
        gen: Number.POSITIVE_INFINITY,
      }),
    ).toBeNull();
    expect(teacherAnchorFrom({ kind: "figure", id: "f" })).toBeNull();
  });

  it("refuses non-object payloads", () => {
    expect(teacherAnchorFrom(null)).toBeNull();
    expect(teacherAnchorFrom(undefined)).toBeNull();
    expect(teacherAnchorFrom("figure")).toBeNull();
  });
});

describe("monotonic-generation drop (§11.1 staleness across the async boundary)", () => {
  it("a lower generation arriving after a higher one never moves the page", () => {
    const synced = arrived(INITIAL_TEACHER_ANCHOR, anchor("fig-2", 3));
    const stale = arrived(synced, anchor("fig-9", 1));
    expect(stale).toBe(synced);
  });

  it("drops the stale arrival even while following is paused", () => {
    const synced = arrived(INITIAL_TEACHER_ANCHOR, anchor("fig-2", 3));
    const paused = teacherAnchorReducer(synced, { type: "pause" });
    expect(arrived(paused, anchor("fig-9", 1))).toBe(paused);
  });

  it("an equal-generation re-anchor is live, not stale — one response may anchor twice", () => {
    const first = arrived(INITIAL_TEACHER_ANCHOR, anchor("fig-2", 3));
    const second = arrived(first, anchor("tbl-1", 3, "table"));
    expect(second.synced).toEqual(anchor("tbl-1", 3, "table"));
    expect(second.syncNonce).toBe(first.syncNonce + 1);
  });
});

describe("change-only sync (§11.1: scroll on anchor change only)", () => {
  it("the first anchor of a session syncs", () => {
    const state = arrived(INITIAL_TEACHER_ANCHOR, anchor("fig-2", 0));
    expect(state.synced).toEqual(anchor("fig-2", 0));
    expect(state.syncNonce).toBe(1);
  });

  it("the same target repeated re-arms nothing, even under a newer generation", () => {
    const first = arrived(INITIAL_TEACHER_ANCHOR, anchor("fig-2", 0));
    const repeated = arrived(first, anchor("fig-2", 2));
    expect(repeated.syncNonce).toBe(first.syncNonce);
    expect(repeated.latest?.gen).toBe(2);
  });

  it("the same id under a different kind is a change — kind+id is the target identity", () => {
    const first = arrived(INITIAL_TEACHER_ANCHOR, anchor("x-1", 0, "figure"));
    const other = arrived(first, anchor("x-1", 0, "table"));
    expect(other.syncNonce).toBe(first.syncNonce + 1);
  });
});

describe("scroll pause and resume (the log-viewer pattern; sync never fights the scroll)", () => {
  const followedThenPaused = () => {
    const synced = arrived(INITIAL_TEACHER_ANCHOR, anchor("fig-2", 0));
    return teacherAnchorReducer(synced, { type: "pause" });
  };

  it("pausing freezes `synced` while `latest` keeps drifting, and never bumps the nonce", () => {
    const paused = followedThenPaused();
    const drifted = arrived(paused, anchor("eq-5", 1, "equation"));
    expect(drifted.paused).toBe(true);
    expect(drifted.synced).toEqual(anchor("fig-2", 0));
    expect(drifted.latest).toEqual(anchor("eq-5", 1, "equation"));
    expect(drifted.syncNonce).toBe(paused.syncNonce);
  });

  it("resume jumps to the latest anchor and re-arms the sync", () => {
    const paused = followedThenPaused();
    const drifted = arrived(paused, anchor("eq-5", 1, "equation"));
    const resumed = teacherAnchorReducer(drifted, { type: "resume" });
    expect(resumed.paused).toBe(false);
    expect(resumed.synced).toEqual(anchor("eq-5", 1, "equation"));
    expect(resumed.syncNonce).toBe(drifted.syncNonce + 1);
  });

  it("resume with nothing drifted still re-syncs to the frozen target", () => {
    const paused = followedThenPaused();
    const resumed = teacherAnchorReducer(paused, { type: "resume" });
    expect(resumed.synced).toEqual(anchor("fig-2", 0));
    expect(resumed.paused).toBe(false);
  });

  it("pause without an active anchor is a no-op — there is nothing to stop following", () => {
    expect(
      teacherAnchorReducer(INITIAL_TEACHER_ANCHOR, { type: "pause" }),
    ).toBe(INITIAL_TEACHER_ANCHOR);
  });

  it("a second pause and a resume while following are identities", () => {
    const paused = followedThenPaused();
    expect(teacherAnchorReducer(paused, { type: "pause" })).toBe(paused);
    const following = arrived(INITIAL_TEACHER_ANCHOR, anchor("fig-2", 0));
    expect(teacherAnchorReducer(following, { type: "resume" })).toBe(following);
  });
});

describe("release (a page turn the learner asked for)", () => {
  it("lets go of the synced anchor so the requested page shows, and a new anchor syncs again", () => {
    const synced = arrived(INITIAL_TEACHER_ANCHOR, anchor("fig-2", 4));
    const released = teacherAnchorReducer(synced, { type: "release" });
    expect(released.synced).toBeNull();
    expect(released.paused).toBe(false);
    const next = arrived(released, anchor("fig-3", 5));
    expect(next.synced).toEqual(anchor("fig-3", 5));
  });

  it("is an identity when nothing is synced or paused", () => {
    expect(
      teacherAnchorReducer(INITIAL_TEACHER_ANCHOR, { type: "release" }),
    ).toBe(INITIAL_TEACHER_ANCHOR);
  });
});

describe("sessionEnd", () => {
  it("clears anchors, pause and target — the next session starts clean", () => {
    const paused = teacherAnchorReducer(
      arrived(INITIAL_TEACHER_ANCHOR, anchor("fig-2", 4)),
      { type: "pause" },
    );
    expect(teacherAnchorReducer(paused, { type: "sessionEnd" })).toEqual(
      INITIAL_TEACHER_ANCHOR,
    );
  });

  it("is an identity on the initial state", () => {
    expect(
      teacherAnchorReducer(INITIAL_TEACHER_ANCHOR, { type: "sessionEnd" }),
    ).toBe(INITIAL_TEACHER_ANCHOR);
  });
});

const segment = (overrides: Partial<ReadingSegment>): ReadingSegment => ({
  segmentId: "seg-0",
  idx: 0,
  pageNo: 1,
  kind: "paragraph",
  figureId: null,
  text: "",
  startMs: 0,
  endMs: 0,
  provenance: "fresh",
  words: [],
  anchor: "block",
  rects: [],
  ...overrides,
});

const figure = (overrides: Partial<ReadingFigure>): ReadingFigure => ({
  figureId: "fig-1",
  pageNo: 4,
  bbox: [10, 20, 110, 220],
  cropHash: null,
  ...overrides,
});

describe("anchorTarget resolves ids against the book the view already holds", () => {
  it("figure, equation and table all resolve against the layout regions (figure_region serves all three kinds)", () => {
    const figures = [figure({ figureId: "any-region" })];
    for (const kind of ["figure", "equation", "table"] as const) {
      expect(isRegionKind(kind)).toBe(true);
      const target = anchorTarget(anchor("any-region", 0, kind), figures, []);
      expect(target).toEqual({
        pageNo: 4,
        rects: [{ x0: 10, y0: 20, x1: 110, y1: 220 }],
        shape: "region",
      });
    }
  });

  it("normalizes a reversed bbox — corner order is the layout stage's business, not ours", () => {
    const figures = [figure({ bbox: [110, 220, 10, 20] })];
    const target = anchorTarget(anchor("fig-1", 0), figures, []);
    expect(target?.rects).toEqual([{ x0: 10, y0: 20, x1: 110, y1: 220 }]);
  });

  it("a region without a bbox scrolls but draws nothing", () => {
    const figures = [figure({ bbox: null })];
    const target = anchorTarget(anchor("fig-1", 0), figures, []);
    expect(target).toEqual({ pageNo: 4, rects: [], shape: "region" });
  });

  it("a section resolves against heading segments only — a body segment with the same id is not an anchor", () => {
    const rects = [{ x0: 1, y0: 2, x1: 3, y1: 4 }];
    const segments = [
      segment({ segmentId: "sec-9", kind: "paragraph", pageNo: 2 }),
      segment({ segmentId: "sec-9", kind: "heading", pageNo: 7, rects }),
    ];
    expect(anchorTarget(anchor("sec-9", 0, "section"), [], segments)).toEqual({
      pageNo: 7,
      rects,
      shape: "block",
    });
    const bodyOnly = [segment({ segmentId: "sec-9", kind: "paragraph" })];
    expect(
      anchorTarget(anchor("sec-9", 0, "section"), [], bodyOnly),
    ).toBeNull();
  });

  it("an unresolvable id returns null and the page stays put (§5 existence check, UI half)", () => {
    expect(anchorTarget(anchor("ghost", 0), [figure({})], [])).toBeNull();
    expect(anchorTarget(anchor("ghost", 0, "section"), [], [])).toBeNull();
    expect(anchorPage(anchorTarget(anchor("ghost", 0), [], []))).toBeNull();
  });

  it("no active anchor contributes nothing to the shown page", () => {
    expect(anchorTarget(null, [figure({})], [])).toBeNull();
    expect(anchorPage(null)).toBeNull();
  });
});

const page = (overrides: Partial<ReadingPage>): ReadingPage => ({
  pageNo: 4,
  widthPt: 500,
  heightPt: 1000,
  imagePath: null,
  ...overrides,
});

const regionTarget = {
  pageNo: 4,
  rects: [{ x0: 50, y0: 100, x1: 150, y1: 300 }],
  shape: "region" as const,
};

describe("spotlightPlan", () => {
  it("projects the anchor onto the shown page", () => {
    const plan = spotlightPlan(regionTarget, 4, page({}), true);
    expect(plan).toEqual({
      shape: "region",
      box: { leftPct: 10, topPct: 10, widthPct: 20, heightPct: 20 },
    });
  });

  it("keeps the block shape for section anchors", () => {
    const plan = spotlightPlan(
      { ...regionTarget, shape: "block" },
      4,
      page({}),
      true,
    );
    expect(plan?.shape).toBe("block");
  });

  it("the settings toggle removes the spotlight entirely (§11.1 one user setting)", () => {
    expect(spotlightPlan(regionTarget, 4, page({}), false)).toBeNull();
    expect(
      spotlightPlan({ ...regionTarget, shape: "block" }, 4, page({}), false),
    ).toBeNull();
  });

  it("draws nothing on the wrong page — browsing away must not carry the spotlight along", () => {
    expect(spotlightPlan(regionTarget, 5, page({}), true)).toBeNull();
    expect(
      spotlightPlan(regionTarget, 4, page({ pageNo: 5 }), true),
    ).toBeNull();
    expect(spotlightPlan(regionTarget, null, page({}), true)).toBeNull();
    expect(spotlightPlan(regionTarget, 4, null, true)).toBeNull();
  });

  it("scroll-only anchors (no geometry) draw nothing", () => {
    expect(
      spotlightPlan({ ...regionTarget, rects: [] }, 4, page({}), true),
    ).toBeNull();
    expect(spotlightPlan(null, 4, page({}), true)).toBeNull();
  });

  it("a page without dimensions cannot be projected onto", () => {
    expect(
      spotlightPlan(regionTarget, 4, page({ widthPt: null }), true),
    ).toBeNull();
  });
});

describe("the spotlight setting", () => {
  it("defaults on: only a stored 'off' disables it", () => {
    expect(spotlightEnabledFrom(null)).toBe(true);
    expect(spotlightEnabledFrom("on")).toBe(true);
    expect(spotlightEnabledFrom("off")).toBe(false);
    expect(spotlightEnabledFrom("garbage")).toBe(true);
  });
});
