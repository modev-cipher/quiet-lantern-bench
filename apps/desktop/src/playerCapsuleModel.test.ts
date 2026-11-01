import { describe, expect, it, vi } from "vitest";

import {
  CAPSULE_RATES,
  PLAYER_SCOPE_KEY,
  RESTART_GRACE_S,
  chapterAt,
  chapterLengths,
  chapterSpan,
  chapterTicks,
  loadPlayerScope,
  nextChapterTarget,
  prevChapterTarget,
  remainingLabel,
  storePlayerScope,
} from "./playerCapsuleModel";

const CHAPTERS = [
  { chapterId: "a", startMs: 0 },
  { chapterId: "b", startMs: 60_000 },
  { chapterId: "c", startMs: 150_000 },
];

describe("chapterTicks", () => {
  it("places a tick at every chapter start after the first, as a fraction of the cast", () => {
    expect(chapterTicks(CHAPTERS, 300)).toEqual([
      { chapterId: "b", fraction: 0.2 },
      { chapterId: "c", fraction: 0.5 },
    ]);
  });

  it("draws nothing while the duration is unknown, and drops starts past the end", () => {
    expect(chapterTicks(CHAPTERS, Number.NaN)).toEqual([]);
    expect(chapterTicks(CHAPTERS, 0)).toEqual([]);
    expect(chapterTicks(CHAPTERS, 100)).toEqual([
      { chapterId: "b", fraction: 0.6 },
    ]);
  });
});

describe("chapterAt", () => {
  it("finds the chapter whose span holds the time", () => {
    expect(chapterAt(CHAPTERS, 0)?.chapterId).toBe("a");
    expect(chapterAt(CHAPTERS, 59.9)?.chapterId).toBe("a");
    expect(chapterAt(CHAPTERS, 60)?.chapterId).toBe("b");
    expect(chapterAt(CHAPTERS, 999)?.chapterId).toBe("c");
    expect(chapterAt([], 10)).toBeNull();
  });
});

describe("prevChapterTarget", () => {
  it("restarts the current chapter once past the grace window", () => {
    expect(prevChapterTarget(CHAPTERS, 60 + RESTART_GRACE_S + 1)).toBe(60_000);
  });

  it("steps back a chapter inside the grace window", () => {
    expect(prevChapterTarget(CHAPTERS, 61)).toBe(0);
  });

  it("restarts the first chapter rather than doing nothing", () => {
    expect(prevChapterTarget(CHAPTERS, 1)).toBe(0);
    expect(prevChapterTarget([], 1)).toBeNull();
  });
});

describe("nextChapterTarget", () => {
  it("jumps to the next chapter start and has nowhere to go from the last", () => {
    expect(nextChapterTarget(CHAPTERS, 10)).toBe(60_000);
    expect(nextChapterTarget(CHAPTERS, 160)).toBeNull();
  });
});

describe("remainingLabel", () => {
  it("counts down to the end and is blank while the duration is unknown", () => {
    expect(remainingLabel(30, 95)).toBe("−1:05");
    expect(remainingLabel(30, Number.NaN)).toBe("");
    expect(remainingLabel(200, 95)).toBe("−0:00");
  });
});

describe("CAPSULE_RATES", () => {
  it("offers the approved speeds in ascending order", () => {
    expect(CAPSULE_RATES).toEqual([0.75, 1, 1.25, 1.5, 1.75, 2]);
  });
});

describe("chapterSpan", () => {
  const chapters = [
    { chapterId: "a", startMs: 0 },
    { chapterId: "b", startMs: 60_000 },
    { chapterId: "c", startMs: 150_000 },
  ];

  it("spans from the chapter start to the next chapter start", () => {
    expect(chapterSpan(chapters, 300, 90)).toEqual({ startS: 60, endS: 150 });
  });

  it("ends the last chapter at the book's end", () => {
    expect(chapterSpan(chapters, 300, 200)).toEqual({ startS: 150, endS: 300 });
  });

  it("is null without chapters or a known duration", () => {
    expect(chapterSpan([], 300, 10)).toBeNull();
    expect(chapterSpan(chapters, Number.NaN, 10)).toBeNull();
    expect(chapterSpan(chapters, 0, 10)).toBeNull();
  });
});

describe("chapterLengths", () => {
  it("measures each chapter, the last one to the book's end", () => {
    expect(
      chapterLengths(
        [
          { chapterId: "a", startMs: 0 },
          { chapterId: "b", startMs: 60_000 },
        ],
        100,
      ),
    ).toEqual([60, 40]);
  });

  it("is empty while the duration is unknown", () => {
    expect(chapterLengths([{ chapterId: "a", startMs: 0 }], Infinity)).toEqual(
      [],
    );
  });
});

describe("player scope", () => {
  it("defaults to the chapter and remembers the choice", () => {
    const store = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => store.get(key) ?? null,
      setItem: (key: string, value: string) => store.set(key, value),
    });
    expect(loadPlayerScope()).toBe("chapter");
    storePlayerScope("book");
    expect(loadPlayerScope()).toBe("book");
    localStorage.setItem(PLAYER_SCOPE_KEY, "garbage");
    expect(loadPlayerScope()).toBe("chapter");
    vi.unstubAllGlobals();
  });
});
