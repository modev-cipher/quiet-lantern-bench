import { describe, expect, it } from "vitest";

import golden from "../e2e/fixtures/reading-timeline.golden.json";
import {
  advanceCursor,
  chapterAt,
  chapterToLoadAt,
  heldWordAt,
  OUTSIDE_CURSOR,
  resolveCursor,
  segmentAt,
  underlineFill,
  wordAt,
  type ReadingChapter,
  type ReadingCursor,
  type ReadingIndex,
  type ReadingSegment,
} from "./readingModel";

const index = golden.index as ReadingIndex;
const chapters = golden.chapters as ReadingChapter[];

function segment(overrides: Partial<ReadingSegment> = {}): ReadingSegment {
  return {
    segmentId: "seg-test",
    idx: 0,
    pageNo: 1,
    kind: "text",
    figureId: null,
    anchor: "word",
    rects: [],
    text: "One two.",
    startMs: 1000,
    endMs: 2000,
    provenance: "fresh",
    words: [
      { w: "One", startMs: 1000, endMs: 1400, rect: null },
      { w: "two", startMs: 1600, endMs: 1900, rect: null },
    ],
    ...overrides,
  };
}

describe("wordAt", () => {
  it("contains its start and excludes its end", () => {
    const s = segment();

    expect(wordAt(s, 1000)).toBe(0);
    expect(wordAt(s, 1399)).toBe(0);
    expect(wordAt(s, 1400)).toBeNull();
    expect(wordAt(s, 1600)).toBe(1);
    expect(wordAt(s, 1899)).toBe(1);
    expect(wordAt(s, 1900)).toBeNull();
  });

  it("returns null between words instead of snapping to a neighbour", () => {
    const s = segment();

    expect(wordAt(s, 1500)).toBeNull();
    expect(wordAt(s, 999)).toBeNull();
    expect(wordAt(s, 5000)).toBeNull();
  });

  it("never returns a zero-length span, which owns no instant", () => {
    const s = segment({
      words: [
        { w: "a", startMs: 1000, endMs: 1000, rect: null },
        { w: "b", startMs: 1000, endMs: 1200, rect: null },
      ],
    });

    expect(wordAt(s, 1000)).toBe(1);
  });

  it("has no word to return on a wordless segment", () => {
    expect(wordAt(segment({ words: [] }), 1500)).toBeNull();
  });
});

describe("heldWordAt", () => {
  it("holds the last word through the silence that follows it", () => {
    const s = segment();

    expect(heldWordAt(s, 1500)).toBe(0);
    expect(heldWordAt(s, 1950)).toBe(1);
  });

  it("holds nothing before the first word begins", () => {
    expect(heldWordAt(segment(), 999)).toBeNull();
  });

  it("releases at the segment end rather than bleeding into the next", () => {
    const s = segment();

    expect(heldWordAt(s, 1999)).toBe(1);
    expect(heldWordAt(s, 2000)).toBeNull();
    expect(heldWordAt(s, 2500)).toBeNull();
  });

  it("is a display rule, so it disagrees with the truth on purpose", () => {
    const s = segment();

    expect(wordAt(s, 1500)).toBeNull();
    expect(heldWordAt(s, 1500)).toBe(0);
  });
});

describe("underlineFill", () => {
  it("runs from zero to one across the segment", () => {
    const s = segment();

    expect(underlineFill(s, 1000)).toBe(0);
    expect(underlineFill(s, 1500)).toBeCloseTo(0.5, 10);
    expect(underlineFill(s, 2000)).toBe(1);
  });

  it("clamps outside the segment instead of extrapolating", () => {
    const s = segment();

    expect(underlineFill(s, 0)).toBe(0);
    expect(underlineFill(s, 9999)).toBe(1);
  });

  it("returns zero rather than NaN on a zero-length segment", () => {
    const s = segment({ startMs: 1000, endMs: 1000 });

    expect(underlineFill(s, 1000)).toBe(0);
    expect(Number.isNaN(underlineFill(s, 1000))).toBe(false);
  });

  it("advances on a wordless segment, which is its whole purpose there", () => {
    const wordless = segment({ words: [], provenance: "stale" });

    expect(wordAt(wordless, 1500)).toBeNull();
    expect(underlineFill(wordless, 1500)).toBeCloseTo(0.5, 10);
  });
});

describe("chapter and segment location", () => {
  it("locates a chapter by containment and reports a gap as a gap", () => {
    expect(chapterAt(index, 0)).toBe(0);
    expect(chapterAt(index, 2199)).toBe(0);
    expect(chapterAt(index, 2200)).toBeNull();
    expect(chapterAt(index, 3452)).toBe(1);
    expect(chapterAt(index, 99999)).toBeNull();
  });

  it("still names a chapter to load while the audio sits in a gap", () => {
    expect(chapterToLoadAt(index, 2200)).toBe(0);
    expect(chapterToLoadAt(index, 3451)).toBe(0);
    expect(chapterToLoadAt(index, 3452)).toBe(1);
    expect(chapterToLoadAt(index, 0)).toBe(0);
  });

  it("locates a segment by the same containment rule", () => {
    const first = chapters[0];

    expect(segmentAt(first, 0)).toBe(0);
    expect(segmentAt(first, 900)).toBeNull();
    expect(segmentAt(first, 1300)).toBe(1);
  });
});

const PROBE_COUNT = 10_000;

function probes(chapter: ReadingChapter): number[] {
  const span = chapter.endMs - chapter.startMs;
  const out: number[] = [];
  for (let i = 0; i < PROBE_COUNT; i += 1) {
    out.push(chapter.startMs + Math.floor((span * i) / PROBE_COUNT));
  }
  return out;
}

function linearScan(chapter: ReadingChapter, t: number): ReadingCursor {
  let position: number | null = null;
  for (let i = 0; i < chapter.segments.length; i += 1) {
    if (chapter.segments[i].startMs <= t) position = i;
  }
  if (position == null) return OUTSIDE_CURSOR;
  const found = chapter.segments[position];
  if (t >= found.endMs) {
    return {
      segmentIdx: position,
      inSegment: false,
      wordIdx: null,
      heldWordIdx: null,
      fill: 1,
    };
  }
  let wordIdx: number | null = null;
  let heldWordIdx: number | null = null;
  for (let i = 0; i < found.words.length; i += 1) {
    const word = found.words[i];
    if (word.startMs <= t) heldWordIdx = i;
    if (t >= word.startMs && t < word.endMs) wordIdx = i;
  }
  return {
    segmentIdx: position,
    inSegment: true,
    wordIdx,
    heldWordIdx,
    fill: underlineFill(found, t),
  };
}

function isInAGap(chapter: ReadingChapter, t: number): boolean {
  const position = segmentAt(chapter, t);
  if (position == null) return true;
  const found = chapter.segments[position];
  return !found.words.some((word) => t >= word.startMs && t < word.endMs);
}

describe("resolveCursor against a linear scan", () => {
  it.each(chapters.map((chapter, i) => [i, chapter] as const))(
    "agrees at every one of 10000 probes across chapter %i",
    (_position, chapter) => {
      for (const t of probes(chapter)) {
        expect(resolveCursor(chapter, t), `t=${t}`).toEqual(
          linearScan(chapter, t),
        );
      }
    },
  );

  it.each(chapters.map((chapter, i) => [i, chapter] as const))(
    "returns no word exactly where chapter %i has no word",
    (_position, chapter) => {
      const nulls: number[] = [];
      const gaps: number[] = [];
      for (const t of probes(chapter)) {
        if (resolveCursor(chapter, t).wordIdx == null) nulls.push(t);
        if (isInAGap(chapter, t)) gaps.push(t);
      }

      expect(nulls).toEqual(gaps);
      expect(
        gaps.length,
        "a fixture with no gaps cannot tell containment from snapping",
      ).toBeGreaterThan(0);
      expect(
        nulls.length,
        "a fixture where every probe is a gap would pass this vacuously",
      ).toBeLessThan(PROBE_COUNT);
    },
  );
});

describe("advanceCursor never diverges from a cold resolve", () => {
  it.each(chapters.map((chapter, i) => [i, chapter] as const))(
    "matches the cold cursor at every step through chapter %i",
    (_position, chapter) => {
      let previous = resolveCursor(chapter, chapter.startMs);
      let advanced = 0;
      for (const t of probes(chapter)) {
        const cold = resolveCursor(chapter, t);
        const incremental = advanceCursor(previous, chapter, t);
        expect(incremental, `a forward tick to t=${t} must not need a resync`).not.toBeNull();
        expect(incremental, `t=${t}`).toEqual(cold);
        previous = cold;
        advanced += 1;
      }

      expect(advanced).toBe(PROBE_COUNT);
    },
  );

  it("asks for a resync rather than guessing when time moves backwards", () => {
    const chapter = chapters[1];
    const previous = resolveCursor(chapter, chapter.endMs - 1);

    expect(advanceCursor(previous, chapter, chapter.startMs)).toBeNull();
  });

  it("asks for a resync when a jump outruns the incremental window", () => {
    const chapter: ReadingChapter = {
      ...chapters[0],
      segments: Array.from({ length: 12 }, (_unused, i) =>
        segment({
          segmentId: `seg-${i}`,
          idx: i,
          startMs: i * 1000,
          endMs: i * 1000 + 900,
          words: [{ w: "w", startMs: i * 1000, endMs: i * 1000 + 900, rect: null }],
        }),
      ),
    };
    const previous = resolveCursor(chapter, 0);

    expect(advanceCursor(previous, chapter, 500)).not.toBeNull();
    expect(advanceCursor(previous, chapter, 10_500)).toBeNull();
  });

  it("cannot advance from outside the chapter", () => {
    expect(advanceCursor(OUTSIDE_CURSOR, chapters[0], 100)).toBeNull();
  });

  it("is not a function of playback rate, which media time already carries", () => {
    const chapter = chapters[0];

    expect(resolveCursor(chapter, 450)).toEqual(resolveCursor(chapter, 450));
    expect(Object.keys(resolveCursor(chapter, 450)).sort()).toEqual([
      "fill",
      "heldWordIdx",
      "inSegment",
      "segmentIdx",
      "wordIdx",
    ]);
  });
});
