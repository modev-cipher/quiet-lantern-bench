import { afterEach, describe, expect, it } from "vitest";

import golden from "../e2e/fixtures/reading-timeline.golden.json";
import {
  anchoredSegmentCount,
  loadReadingMode,
  MODE_STORAGE_KEY,
  NO_OVERLAY,
  overlayPlan,
  pageModeAdvisory,
  pageModeReason,
  projectRect,
  readingModeFrom,
  segmentFallbackRects,
  storeReadingMode,
  tapAtFraction,
  unionOf,
  wordAtPagePoint,
} from "./readingPageModel";
import type {
  ReadingChapter,
  ReadingPage,
  ReadingSegment,
} from "./readingModel";

const chapters = golden.chapters as ReadingChapter[];
const pages = golden.pages as ReadingPage[];

const anchored = chapters[0].segments[0];
const blockRung = chapters[0].segments[1];
const cue = chapters[1].segments[0];
const samePageFramed = chapters[1].segments[2];

function pageOne(): ReadingPage {
  return pages.find((entry) => entry.pageNo === 1) as ReadingPage;
}

function pageThree(): ReadingPage {
  return pages.find((entry) => entry.pageNo === 3) as ReadingPage;
}

function fakeStorage(initial: string | null): Storage {
  let value = initial;
  return {
    get length() {
      return value == null ? 0 : 1;
    },
    clear() {
      value = null;
    },
    getItem(key: string) {
      return key === MODE_STORAGE_KEY ? value : null;
    },
    key() {
      return MODE_STORAGE_KEY;
    },
    removeItem() {
      value = null;
    },
    setItem(key: string, next: string) {
      if (key === MODE_STORAGE_KEY) value = next;
    },
  };
}

function withStorage(storage: Storage | undefined): void {
  Object.defineProperty(globalThis, "localStorage", {
    value: storage,
    configurable: true,
    writable: true,
  });
}

afterEach(() => {
  withStorage(undefined);
});

describe("the golden fixture this model is measured against", () => {
  it("carries a real word rect and a real per-token gap", () => {
    expect(
      anchored.anchor,
      "R-BF: without a seeded geometry page every assertion below would pass against a world where anchoring never succeeds",
    ).toBe("word");
    expect(anchored.words.map((word) => word.rect != null)).toEqual([
      true,
      true,
      false,
      true,
    ]);
    expect(blockRung.anchor).toBe("block");
    expect(cue.anchor).toBe("page");
  });
});

describe("reading mode selection", () => {
  it("defaults to script for every value that is not exactly the page literal", () => {
    for (const stored of [null, "", "Page", "script", "PAGE", "1", "true"]) {
      expect(
        readingModeFrom(stored),
        `${JSON.stringify(stored)} must not enable page mode`,
      ).toBe("script");
    }
    expect(readingModeFrom("page")).toBe("page");
  });

  it("reads the stored preference, and reads script when nothing is stored", () => {
    withStorage(fakeStorage(null));
    expect(
      loadReadingMode(),
      "page mode ships opt-in for one release, so an absent preference is script",
    ).toBe("script");
    withStorage(fakeStorage("page"));
    expect(loadReadingMode()).toBe("page");
  });

  it("round-trips through storage", () => {
    const storage = fakeStorage(null);
    withStorage(storage);
    storeReadingMode("page");
    expect(storage.getItem(MODE_STORAGE_KEY)).toBe("page");
    expect(loadReadingMode()).toBe("page");
  });

  it("falls back to script when storage itself throws", () => {
    withStorage({
      ...fakeStorage(null),
      getItem() {
        throw new Error("blocked");
      },
    } as unknown as Storage);
    expect(loadReadingMode()).toBe("script");
    expect(() => storeReadingMode("page")).not.toThrow();
  });
});

describe("why the surface looks the way it does", () => {
  it("names the missing text layer ahead of the job mode", () => {
    expect(pageModeReason("teacher", true)).toBe("text-layer-missing");
    expect(pageModeReason("narrator", true)).toBe("text-layer-missing");
    expect(pageModeReason("teacher", false)).toBe("teacher");
    expect(pageModeReason("Teacher", false)).toBe("teacher");
    expect(pageModeReason("narrator", false)).toBe("user-preference");
  });

  it("advises on the two reasons that limit anchoring and stays silent otherwise", () => {
    expect(pageModeAdvisory("narrator", false, chapters[0])).toBeNull();
    expect(pageModeAdvisory("teacher", false, chapters[0])).toContain(
      "lecture",
    );
    expect(pageModeAdvisory("narrator", true, chapters[1])).toContain(
      "no text layer",
    );
  });

  it("withdraws the no-text-layer claim once the chapter proves it highlights", () => {
    expect(chapters[0].anchorGeometry).toBe("ensured");
    expect(anchoredSegmentCount(chapters[0].segments)).toBeGreaterThan(0);
    expect(pageModeAdvisory("narrator", true, chapters[0])).toBeNull();
  });

  it("keeps the teacher claim on a scanned book whose geometry works, since that limit is mode-inherent", () => {
    expect(pageModeAdvisory("teacher", true, chapters[0])).toContain(
      "lecture",
    );
  });

  it("stands by the no-text-layer claim when the chapter genuinely cannot anchor", () => {
    expect(chapters[1].anchorGeometry).toBe("unavailable");
    expect(pageModeAdvisory("narrator", true, chapters[1])).toContain(
      "no text layer",
    );
    const ensuredButUnanchored: ReadingChapter = {
      ...chapters[0],
      segments: chapters[0].segments.map((segment) => ({
        ...segment,
        rects: [],
        words: segment.words.map((word) => ({ ...word, rect: null })),
      })),
    };
    expect(pageModeAdvisory("narrator", true, ensuredButUnanchored)).toContain(
      "no text layer",
    );
  });

  it("claims nothing about the text layer before the chapter arrives, but mode-inherent limits speak at once", () => {
    expect(pageModeAdvisory("narrator", true, null)).toBeNull();
    expect(pageModeAdvisory("teacher", false, null)).toContain("lecture");
    expect(pageModeAdvisory("teacher", true, null)).toContain("lecture");
  });
});

describe("projecting a page rect onto the rendered image", () => {
  it("uses page points as a fraction of the page, so no pixel measurement is involved", () => {
    const box = projectRect({ x0: 61.2, y0: 79.2, x1: 122.4, y1: 158.4 }, pageOne());

    expect(box).toEqual({
      leftPct: 10,
      topPct: 10,
      widthPct: 10,
      heightPct: 10,
    });
  });

  it("refuses to project when the page never declared its own size", () => {
    for (const overrides of [
      { widthPt: null },
      { heightPt: null },
      { widthPt: 0 },
      { heightPt: -1 },
    ]) {
      expect(
        projectRect(
          { x0: 72, y0: 100, x1: 98, y1: 112 },
          { ...pageOne(), ...overrides },
        ),
        `${JSON.stringify(overrides)} makes the container fall back to a guessed 3/4 ratio, and an overlay drawn on a guessed ratio is a confidently wrong position`,
      ).toBeNull();
    }
  });

  it("keeps a rect whose edges land exactly on the page bounds", () => {
    const page = pageOne();
    const box = projectRect(
      { x0: 0, y0: 0, x1: page.widthPt as number, y1: page.heightPt as number },
      page,
    );

    expect(
      box,
      "a glyph box that reaches the trim edge is an ordinary rect, and the containment test is written with a > so the exact edge must stay inside it",
    ).toEqual({ leftPct: 0, topPct: 0, widthPct: 100, heightPct: 100 });
  });

  it("treats sub-tolerance overhang as float noise and anything larger as off the page", () => {
    const page = pageOne();
    const width = page.widthPt as number;
    const rect = (x1: number) => ({ x0: 0, y0: 100, x1, y1: 112 });

    expect(
      projectRect(rect(width + width * 0.0001), page),
      "the 1.0001 bound exists so a rect reconstructed from 2-decimal points is not rejected by its last bit",
    ).not.toBeNull();
    expect(
      projectRect(rect(width + 1), page),
      "a whole point past the trim edge is a real disagreement with the page, not rounding",
    ).toBeNull();
  });

  it("refuses a rect that does not lie on the page or has no area", () => {
    expect(projectRect({ x0: -1, y0: 100, x1: 98, y1: 112 }, pageOne())).toBeNull();
    expect(projectRect({ x0: 72, y0: 100, x1: 700, y1: 112 }, pageOne())).toBeNull();
    expect(projectRect({ x0: 72, y0: 100, x1: 72, y1: 112 }, pageOne())).toBeNull();
    expect(projectRect({ x0: 72, y0: 112, x1: 98, y1: 112 }, pageOne())).toBeNull();
  });
});

describe("the segment-level fallback", () => {
  it("prefers the segment's own rects when the ladder produced them", () => {
    expect(segmentFallbackRects(blockRung, pageOne())).toEqual(blockRung.rects);
  });

  it("unions the located word rects when the rung produced no segment rect", () => {
    expect(anchored.rects, "a word-rung segment carries none").toEqual([]);
    expect(segmentFallbackRects(anchored, pageOne())).toEqual([
      { x0: 72, y0: 100, x1: 240, y1: 112 },
    ]);
  });

  it("has nothing to offer a segment the ladder never located", () => {
    expect(segmentFallbackRects(cue, pageOne())).toEqual([]);
  });

  it("unions rects that span lines rather than picking one", () => {
    expect(
      unionOf([
        { x0: 72, y0: 100, x1: 98, y1: 112 },
        { x0: 210, y0: 130, x1: 292, y1: 142 },
      ]),
    ).toEqual({ x0: 72, y0: 100, x1: 292, y1: 142 });
    expect(unionOf([])).toBeNull();
  });
});

describe("the overlay plan", () => {
  it("draws the word rect while the spoken word is located", () => {
    const plan = overlayPlan(anchored, 1, 1, pageOne());

    expect(plan.kind).toBe("word");
    expect(plan.boxes).toHaveLength(1);
    expect(plan.boxes[0].leftPct).toBeCloseTo((102 / 612) * 100, 6);
    expect(plan.boxes[0].topPct).toBeCloseTo((100 / 792) * 100, 6);
  });

  it("unconditionally returns NO_OVERLAY when in Teacher mode to prevent accidental word collision flashes", () => {
    expect(overlayPlan(anchored, 1, 1, pageOne(), true)).toEqual(NO_OVERLAY);
    const pageTwo = pages.find((entry) => entry.pageNo === 2) as ReadingPage;
    expect(overlayPlan(blockRung, null, 2, pageTwo, true)).toEqual(NO_OVERLAY);
  });

  it("holds the previous located word rect on a token the walk could not locate", () => {
    const plan = overlayPlan(anchored, 2, 1, pageOne());

    expect(
      plan.kind,
      "'sentence' is spoken but sits in another block; holding the previous word avoids flashing the whole segment",
    ).toBe("word");
    expect(plan.boxes).toHaveLength(1);
    expect(plan.boxes[0].leftPct).toBeCloseTo((102 / 612) * 100, 6);
  });

  it("returns none during silence between words or before first word for word-anchored segments, while block-anchored falls back to segment", () => {
    expect(overlayPlan(anchored, null, 1, pageOne())).toEqual(NO_OVERLAY);
    const pageTwo = pages.find((entry) => entry.pageNo === 2) as ReadingPage;
    const block = overlayPlan(blockRung, null, 2, pageTwo);
    expect(block.kind).toBe("segment");
    expect(block.boxes).toHaveLength(1);
  });

  it("cleanly returns NO_OVERLAY on an unlocated initial token rather than falling through to segment envelope", () => {
    const initialUnlocated: ReadingSegment = {
      ...anchored,
      words: anchored.words.map((word, position) =>
        position === 0 ? { ...word, rect: null } : word,
      ),
    };
    expect(overlayPlan(initialUnlocated, 0, 1, pageOne())).toEqual(NO_OVERLAY);
  });

  it("returns NO_OVERLAY if the backward-held word cannot project onto the shown page", () => {
    const crossPageSegment: ReadingSegment = {
      ...anchored,
      pageNo: 2,
      words: [
        {
          w: "previous",
          startMs: 0,
          endMs: 500,
          rect: { x0: 100, y0: 10000, x1: 200, y1: 10020 },
        },
        { w: "current", startMs: 500, endMs: 1000, rect: null },
      ],
    };
    const pageTwo = pages.find((entry) => entry.pageNo === 2) as ReadingPage;
    expect(overlayPlan(crossPageSegment, 1, 2, pageTwo)).toEqual(NO_OVERLAY);
  });

  it("draws nothing at all for a segment the ladder left on the page rung", () => {
    expect(cue.pageNo).toBe(3);
    expect(
      overlayPlan(cue, 0, 3, pageThree()),
      "the shown page is passed as the cue's own page so the identity gate is satisfied and the page rung is the only thing left refusing the overlay",
    ).toEqual({ kind: "none", boxes: [] });
  });

  it("refuses to draw a segment's rects on a page that is not the segment's page", () => {
    expect(
      cue.pageNo,
      "the golden carries the collision on the cue: a segment on page 3 whose figure frames page 4",
    ).toBe(3);
    expect(
      samePageFramed.pageNo,
      "and the ordinary layout on the next text segment: its figure sits on its own page",
    ).toBe(4);
    expect(
      overlayPlan(anchored, 1, 3, pageThree()),
      "page 1 rects drawn on page 3 would be a confidently wrong highlight, which is the one failure the ladder exists to prevent",
    ).toEqual({ kind: "none", boxes: [] });
  });

  it("refuses a page row that is not the page it was told it is showing", () => {
    expect(
      overlayPlan(anchored, 1, 1, pageThree()),
      "the caller resolves the row from the number, so these agree today; this function owns the refusal and must not depend on a caller getting that right",
    ).toEqual({ kind: "none", boxes: [] });
  });

  it("draws nothing without a segment or without the page row", () => {
    expect(overlayPlan(null, 0, 1, pageOne()).kind).toBe("none");
    expect(overlayPlan(anchored, 0, 1, null).kind).toBe("none");
    expect(overlayPlan(anchored, 0, null, pageOne()).kind).toBe("none");
  });

  it("holds the previous located word if a located word's rect will not project onto the page", () => {
    for (const [name, bad] of [
      ["degenerate", { x0: 72, y0: 100, x1: 72, y1: 112 }],
      ["past the media box", { x0: 72, y0: 100, x1: 9000, y1: 112 }],
      ["negative", { x0: -4, y0: 100, x1: 98, y1: 112 }],
    ] as const) {
      const broken: ReadingSegment = {
        ...anchored,
        words: anchored.words.map((word, position) =>
          position === 1 ? { ...word, rect: bad } : word,
        ),
      };
      const plan = overlayPlan(broken, 1, 1, pageOne());

      expect(
        plan.kind,
        `a ${name} glyph box holds the preceding located word without expanding to a multi-line block`,
      ).toBe("word");
      expect(plan.boxes).toHaveLength(1);
      expect(plan.boxes[0].leftPct).toBeCloseTo((72 / 612) * 100, 6);
    }
  });

  it("drops a rect the page cannot hold rather than clamping it into view", () => {
    const offPage: ReadingSegment = {
      ...anchored,
      rects: [{ x0: 72, y0: 100, x1: 9000, y1: 112 }],
      words: [],
    };

    expect(overlayPlan(offPage, null, 1, pageOne())).toEqual({
      kind: "none",
      boxes: [],
    });
  });
});

describe("the anchored-segment count the diagnostic reports", () => {
  it("counts a segment with any rect at all, from either source", () => {
    expect(anchoredSegmentCount(chapters[0].segments)).toBe(2);
    expect(
      anchoredSegmentCount(chapters[1].segments),
      "the cue anchors nothing; both text segments reach the block rung on page 4",
    ).toBe(2);
    expect(anchoredSegmentCount([])).toBe(0);
  });
});

describe("wordAtPagePoint, the inverse of projectRect", () => {
  const segments = chapters[0].segments;

  it("returns the word whose printed rect contains the tap", () => {
    expect(wordAtPagePoint(segments, 1, 85, 106)).toEqual({
      segmentIdx: 0,
      wordIdx: 0,
      startMs: 0,
    });
    expect(wordAtPagePoint(segments, 1, 118, 106)).toEqual({
      segmentIdx: 0,
      wordIdx: 1,
      startMs: 200,
    });
    expect(wordAtPagePoint(segments, 1, 214, 106)).toEqual({
      segmentIdx: 0,
      wordIdx: 3,
      startMs: 890,
    });
  });

  it("a tap where the unlocated word is printed hits nothing rather than the nearest neighbour", () => {
    expect(
      wordAtPagePoint(segments, 1, 160, 106),
      "the golden's per-token gap sits between 134pt and 188pt; snapping it to a neighbour would seek somewhere the user did not aim",
    ).toBeNull();
  });

  it("only the tapped page's words are eligible", () => {
    expect(wordAtPagePoint(segments, 2, 85, 106)).toBeNull();
    expect(wordAtPagePoint(chapters[1].segments, 3, 85, 106)).toBeNull();
  });

  it("the rect's own edges are inside it", () => {
    expect(wordAtPagePoint(segments, 1, 72, 100)?.wordIdx).toBe(0);
    expect(wordAtPagePoint(segments, 1, 98, 112)?.wordIdx).toBe(0);
    expect(wordAtPagePoint(segments, 1, 71.9, 106)).toBeNull();
  });

  it("overlapping rects resolve first-in-document-order, shared edges inclusive — the earlier word wins", () => {
    const overlapping: ReadingSegment = {
      ...anchored,
      words: [
        {
          w: "earlier",
          startMs: 0,
          endMs: 100,
          rect: { x0: 100, y0: 100, x1: 140, y1: 112 },
        },
        {
          w: "later",
          startMs: 200,
          endMs: 300,
          rect: { x0: 120, y0: 100, x1: 180, y1: 112 },
        },
      ],
    };
    expect(
      wordAtPagePoint([overlapping], 1, 130, 106)?.wordIdx,
      "inside both rects, the earlier word in document order wins",
    ).toBe(0);
    expect(
      wordAtPagePoint([overlapping], 1, 140, 106)?.wordIdx,
      "a shared edge is inside both rects, so the earlier word still wins",
    ).toBe(0);
    expect(wordAtPagePoint([overlapping], 1, 141, 106)?.wordIdx).toBe(1);
    const shadowing: ReadingSegment = {
      ...anchored,
      words: [
        {
          w: "second-segment",
          startMs: 400,
          endMs: 500,
          rect: { x0: 100, y0: 100, x1: 140, y1: 112 },
        },
      ],
    };
    expect(
      wordAtPagePoint([overlapping, shadowing], 1, 130, 106),
      "across segments the same rule holds: the earlier segment's word wins",
    ).toEqual({ segmentIdx: 0, wordIdx: 0, startMs: 0 });
  });

  it("a degenerate rect can contain nothing", () => {
    const degenerate: ReadingSegment = {
      ...anchored,
      words: [
        {
          w: "flat",
          startMs: 0,
          endMs: 100,
          rect: { x0: 100, y0: 100, x1: 100, y1: 112 },
        },
      ],
    };

    expect(wordAtPagePoint([degenerate], 1, 100, 106)).toBeNull();
  });

  it("misses outside every rect", () => {
    expect(wordAtPagePoint(segments, 1, 300, 400)).toBeNull();
    expect(wordAtPagePoint([], 1, 85, 106)).toBeNull();
  });
});

describe("the by-construction equivalence the tap target relies on", () => {
  it("a segment carries a located word rect exactly when its rung is word", () => {
    for (const chapter of chapters) {
      for (const segment of chapter.segments) {
        const located = segment.words.some((word) => word.rect != null);
        expect(
          located,
          `${segment.segmentId}: tap-to-seek is offered on the word rung only, and containment lookup finds words only where rects exist — these must be the same set`,
        ).toBe(segment.anchor === "word");
      }
    }
  });
});

describe("tapAtFraction maps a click fraction into page points", () => {
  const segments = chapters[0].segments;

  it("lands a fraction on the word printed there", () => {
    const outcome = tapAtFraction(segments, 1, pageOne(), 85 / 612, 106 / 792);

    expect(outcome?.xPt).toBeCloseTo(85, 6);
    expect(outcome?.yPt).toBeCloseTo(106, 6);
    expect(outcome?.hit).toEqual({ segmentIdx: 0, wordIdx: 0, startMs: 0 });
  });

  it("reports a dead tap instead of inventing a hit", () => {
    const outcome = tapAtFraction(segments, 1, pageOne(), 160 / 612, 106 / 792);

    expect(outcome).not.toBeNull();
    expect(outcome?.hit).toBeNull();
  });

  it("a surface that is not the shown page yields an unmapped outcome, never a silent null", () => {
    const unmapped = { xPt: null, yPt: null, hit: null };
    expect(tapAtFraction(segments, 3, pageOne(), 0.5, 0.5)).toEqual({
      pageNo: 3,
      ...unmapped,
    });
    expect(tapAtFraction(segments, null, pageOne(), 0.5, 0.5)).toEqual({
      pageNo: null,
      ...unmapped,
    });
    expect(
      tapAtFraction(segments, 1, null, 0.5, 0.5),
      "a tap on a loading placeholder must still surface as a debug line, or the dead-taps-observable claim is false exactly where the user is confused",
    ).toEqual({ pageNo: 1, ...unmapped });
  });

  it("a page without geometry yields an unmapped outcome rather than fabricated points", () => {
    const flat: ReadingPage = {
      pageNo: 1,
      widthPt: null,
      heightPt: null,
      imagePath: null,
    };

    expect(tapAtFraction(segments, 1, flat, 0.5, 0.5)).toEqual({
      pageNo: 1,
      xPt: null,
      yPt: null,
      hit: null,
    });
  });

  it("a non-finite fraction yields an unmapped outcome", () => {
    expect(tapAtFraction(segments, 1, pageOne(), Number.NaN, 0.5)).toEqual({
      pageNo: 1,
      xPt: null,
      yPt: null,
      hit: null,
    });
  });
});
