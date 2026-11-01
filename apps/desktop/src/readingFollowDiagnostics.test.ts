import { describe, expect, it } from "vitest";

import { FOLLOWING, type FollowState } from "./followModel";
import {
  describeFollowState,
  followTransitionLine,
  tapLine,
} from "./readingFollowDiagnostics";

const browsing = (pageNo: number): FollowState => ({ mode: "browsing", pageNo });

describe("the follow state line", () => {
  it("spells a flip transition with seek=no, the invariant made observable", () => {
    expect(
      followTransitionLine({
        trigger: "flip",
        from: FOLLOWING,
        to: browsing(4),
      }),
    ).toBe("state=browsing trigger=flip from=following to=browsing:4 seek=no");
  });

  it("spells the browsed page on both ends of a flip inside browsing", () => {
    expect(
      followTransitionLine({
        trigger: "flip",
        from: browsing(4),
        to: browsing(5),
      }),
    ).toBe("state=browsing trigger=flip from=browsing:4 to=browsing:5 seek=no");
  });

  it("marks seek=yes only on the seeked trigger, the one that carried a seek", () => {
    expect(
      followTransitionLine({
        trigger: "seeked",
        from: browsing(4),
        to: FOLLOWING,
      }),
    ).toBe("state=following trigger=seeked from=browsing:4 to=following seek=yes");
    expect(
      followTransitionLine({
        trigger: "returnToNarration",
        from: browsing(4),
        to: FOLLOWING,
      }),
    ).toBe(
      "state=following trigger=returnToNarration from=browsing:4 to=following seek=no",
    );
  });

  it("emits nothing when the state did not change, so the log stays quiet per tick", () => {
    expect(
      followTransitionLine({
        trigger: "narrationPage",
        from: FOLLOWING,
        to: FOLLOWING,
      }),
    ).toBeNull();
    expect(
      followTransitionLine({
        trigger: "flip",
        from: browsing(8),
        to: browsing(8),
      }),
    ).toBeNull();
  });
});

describe("describeFollowState", () => {
  it("names the page while browsing and nothing while following", () => {
    expect(describeFollowState(FOLLOWING)).toBe("following");
    expect(describeFollowState(browsing(3))).toBe("browsing:3");
  });
});

describe("the tap line", () => {
  it("spells a dead tap exactly the way the log review greps for it", () => {
    expect(
      tapLine({ pageNo: 1, xPt: 160.04, yPt: 106, hit: null }),
    ).toBe("160,106 page=1 hit=none");
  });

  it("spells a hit with the millisecond the seek was issued for", () => {
    expect(
      tapLine({
        pageNo: 1,
        xPt: 85.55,
        yPt: 106.2,
        hit: { segmentIdx: 0, wordIdx: 3, startMs: 890 },
      }),
    ).toBe("85.6,106.2 page=1 hit=word seekMs=890");
  });

  it("spells a tap the geometry could not map, so no tap on the layer is ever silent", () => {
    expect(tapLine({ pageNo: 7, xPt: null, yPt: null, hit: null })).toBe(
      "unmapped page=7 hit=none",
    );
    expect(tapLine({ pageNo: null, xPt: null, yPt: null, hit: null })).toBe(
      "unmapped page=none hit=none",
    );
  });
});
