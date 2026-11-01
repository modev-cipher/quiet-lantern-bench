import { describe, expect, it } from "vitest";

import {
  FOLLOWING,
  followReducer,
  seekNonceAdvanced,
  shownPage,
  travelDirectionAfter,
  type FollowAction,
  type FollowContext,
  type FollowState,
} from "./followModel";

const context: FollowContext = { narrationPage: 3, lastPage: 8 };

const browsing = (pageNo: number): FollowState => ({ mode: "browsing", pageNo });

describe("flipping", () => {
  it("a flip while following starts browsing from the narration page", () => {
    const next = followReducer(FOLLOWING, { type: "flip", delta: 1 }, context);
    expect(next).toEqual(browsing(4));
  });

  it("a flip while browsing moves relative to the browsed page, not the narration page", () => {
    const next = followReducer(browsing(6), { type: "flip", delta: -1 }, context);
    expect(next).toEqual(browsing(5));
  });

  it("a flip below page 1 clamps to page 1", () => {
    const next = followReducer(browsing(1), { type: "flip", delta: -1 }, context);
    expect(next).toEqual(browsing(1));
  });

  it("a flip past the last page clamps to the last page", () => {
    const next = followReducer(browsing(8), { type: "flip", delta: 1 }, context);
    expect(next).toEqual(browsing(8));
  });

  it("an unknown last page clamps below but never above", () => {
    const open: FollowContext = { narrationPage: 3, lastPage: 0 };
    expect(followReducer(browsing(9), { type: "flip", delta: 1 }, open)).toEqual(
      browsing(10),
    );
    expect(followReducer(browsing(1), { type: "flip", delta: -2 }, open)).toEqual(
      browsing(1),
    );
  });

  it("a non-finite flip target stays put instead of browsing to infinity", () => {
    const open: FollowContext = { narrationPage: 3, lastPage: 0 };
    expect(
      followReducer(FOLLOWING, { type: "flip", delta: Number.POSITIVE_INFINITY }, open),
    ).toBe(FOLLOWING);
    const state = browsing(4);
    expect(
      followReducer(state, { type: "flip", delta: Number.NaN }, open),
    ).toBe(state);
  });

  it("a fractional flip delta is refused, never a fractional page", () => {
    expect(followReducer(FOLLOWING, { type: "flip", delta: 0.5 }, context)).toBe(
      FOLLOWING,
    );
    const state = browsing(4);
    expect(
      followReducer(state, { type: "flip", delta: -1.5 }, context),
    ).toBe(state);
  });

  it("a flip with no narration page yet stays following", () => {
    const blank: FollowContext = { narrationPage: null, lastPage: 8 };
    const next = followReducer(FOLLOWING, { type: "flip", delta: 1 }, blank);
    expect(next).toBe(FOLLOWING);
  });

  it("a backward flip while following browses the previous page", () => {
    const next = followReducer(FOLLOWING, { type: "flip", delta: -1 }, context);
    expect(next).toEqual(browsing(2));
  });
});

describe("returning", () => {
  it("returnToNarration ends browsing", () => {
    const next = followReducer(browsing(6), { type: "returnToNarration" }, context);
    expect(next).toBe(FOLLOWING);
  });

  it("returnToNarration while following is an identity", () => {
    const next = followReducer(FOLLOWING, { type: "returnToNarration" }, context);
    expect(next).toBe(FOLLOWING);
  });
});

describe("deliberate re-aiming is the only exit from browsing", () => {
  it("a seek ends browsing because the user re-aimed the narration", () => {
    const next = followReducer(browsing(6), { type: "seeked" }, context);
    expect(next).toBe(FOLLOWING);
  });

  it("a seek while following is an identity", () => {
    const next = followReducer(FOLLOWING, { type: "seeked" }, context);
    expect(next).toBe(FOLLOWING);
  });

  it("a chapter change holds the browsed page rather than silently re-aiming it", () => {
    const state = browsing(6);
    const next = followReducer(state, { type: "chapterChanged" }, context);
    expect(next).toBe(state);
  });

  it("a reading-mode change stays inside browsing on the same page", () => {
    const state = browsing(6);
    const next = followReducer(state, { type: "readingModeChanged" }, context);
    expect(next).toBe(state);
  });

  it("a narration tick never moves a browsed page", () => {
    const state = browsing(6);
    const next = followReducer(
      state,
      { type: "narrationPage", pageNo: 6 },
      context,
    );
    expect(next).toBe(state);
  });

  it("a narration tick never transitions the following state", () => {
    const next = followReducer(
      FOLLOWING,
      { type: "narrationPage", pageNo: 5 },
      context,
    );
    expect(next).toBe(FOLLOWING);
  });
});

describe("the travel direction follows accepted flips only", () => {
  it("an accepted flip sets the direction of travel", () => {
    expect(travelDirectionAfter({ type: "flip", delta: 1 }, true, -1)).toBe(1);
    expect(travelDirectionAfter({ type: "flip", delta: -1 }, true, 1)).toBe(-1);
  });

  it("a refused or clamped flip leaves the direction alone — mutating before the reducer decides would flip prefetch for the next unrelated render", () => {
    expect(travelDirectionAfter({ type: "flip", delta: -1 }, false, 1)).toBe(1);
    expect(travelDirectionAfter({ type: "flip", delta: 1 }, false, -1)).toBe(-1);
  });

  it("non-flip actions never steer the prefetch window", () => {
    expect(travelDirectionAfter({ type: "seeked" }, true, -1)).toBe(-1);
    expect(travelDirectionAfter({ type: "chapterChanged" }, false, 1)).toBe(1);
  });
});

describe("only a nonce that advanced is a seek", () => {
  it("a bump is a real seek", () => {
    expect(seekNonceAdvanced(0, 1)).toBe(true);
    expect(seekNonceAdvanced(4, 5)).toBe(true);
  });

  it("a reset to 0 is a load or a stop, never a seek — logging seek=yes there would fabricate a seek that never happened", () => {
    expect(seekNonceAdvanced(5, 0)).toBe(false);
  });

  it("the subscriber adopts the reset value, so the first real seek after a reset still counts", () => {
    expect(seekNonceAdvanced(5, 0)).toBe(false);
    expect(seekNonceAdvanced(0, 1)).toBe(true);
  });
});

describe("shownPage is the single place the two page sources meet", () => {
  it("following shows the narration page", () => {
    expect(shownPage(FOLLOWING, 5)).toBe(5);
    expect(shownPage(FOLLOWING, null)).toBeNull();
  });

  it("browsing shows the browsed page whatever the narration does", () => {
    expect(shownPage(browsing(2), 5)).toBe(2);
    expect(shownPage(browsing(2), null)).toBe(2);
  });
});

describe("the structural no-seek invariant", () => {
  const everyAction: FollowAction[] = [
    { type: "flip", delta: 1 },
    { type: "flip", delta: -1 },
    { type: "returnToNarration" },
    { type: "seeked" },
    { type: "chapterChanged" },
    { type: "readingModeChanged" },
    { type: "narrationPage", pageNo: 2 },
  ];
  const everyState: FollowState[] = [FOLLOWING, browsing(4)];

  it("no action from any state returns anything beyond the follow state itself", () => {
    for (const state of everyState) {
      for (const action of everyAction) {
        const next = followReducer(state, action, context);
        expect(
          Object.keys(next).sort(),
          `${state.mode} + ${action.type} must not smuggle a seek request out of the reducer`,
        ).toEqual(next.mode === "browsing" ? ["mode", "pageNo"] : ["mode"]);
      }
    }
  });

  it("no-op transitions return the same reference so subscribers can bail", () => {
    const holds: FollowAction[] = [
      { type: "chapterChanged" },
      { type: "readingModeChanged" },
      { type: "narrationPage", pageNo: 2 },
    ];
    const state = browsing(4);
    for (const action of holds) {
      expect(followReducer(state, action, context)).toBe(state);
    }
    for (const action of everyAction) {
      if (action.type === "flip") continue;
      expect(followReducer(FOLLOWING, action, context)).toBe(FOLLOWING);
    }
  });
});
