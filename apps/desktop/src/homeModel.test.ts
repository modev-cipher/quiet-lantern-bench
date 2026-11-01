import { describe, expect, it } from "vitest";

import type { AudioShelfItem } from "./audioShelfModel";
import type { ReadingIndex } from "./readingModel";
import {
  JUMP_COUNT,
  MAX_RECENT,
  formatPosition,
  homeSelection,
  parseRecent,
  positionLine,
  withRecent,
} from "./homeModel";

function item(overrides: Partial<AudioShelfItem>): AudioShelfItem {
  return {
    id: "export-1",
    bookId: "book-1",
    title: "Fixture Book",
    path: "/exports/fixture.mp3",
    mode: "narrate",
    format: "mp3",
    bytes: 1000,
    stale: false,
    isScanned: false,
    jobId: "job-1",
    createdAt: 100,
    ...overrides,
  };
}

describe("parseRecent", () => {
  it("returns empty on garbage, non-arrays, and null", () => {
    expect(parseRecent(null)).toEqual([]);
    expect(parseRecent("not json")).toEqual([]);
    expect(parseRecent('{"a":1}')).toEqual([]);
  });

  it("keeps only unique non-empty strings, capped", () => {
    const raw = JSON.stringify(["a", "b", "a", "", 3, "c"]);
    expect(parseRecent(raw)).toEqual(["a", "b", "c"]);
    const many = JSON.stringify(
      Array.from({ length: MAX_RECENT + 10 }, (_v, i) => `id-${i}`),
    );
    expect(parseRecent(many)).toHaveLength(MAX_RECENT);
  });
});

describe("withRecent", () => {
  it("moves the id to the front without duplicating it", () => {
    expect(withRecent(["a", "b"], "b")).toEqual(["b", "a"]);
  });

  it("returns the same array when the id is already first", () => {
    const ids = ["a", "b"];
    expect(withRecent(ids, "a")).toBe(ids);
  });

  it("caps the list", () => {
    const ids = Array.from({ length: MAX_RECENT }, (_v, i) => `id-${i}`);
    expect(withRecent(ids, "fresh")).toHaveLength(MAX_RECENT);
  });
});

describe("homeSelection", () => {
  it("puts the most recently listened playable item in the hero", () => {
    const shelf = [
      item({ id: "a", createdAt: 300 }),
      item({ id: "b", bookId: "book-2", createdAt: 200 }),
    ];
    const picked = homeSelection(shelf, ["b"], { b: 0.4 });
    expect(picked.hero?.id).toBe("b");
    expect(picked.heroListened).toBe(0.4);
  });

  it("never puts a stale or unplayable export in the hero", () => {
    const shelf = [
      item({ id: "a", stale: true, createdAt: 300 }),
      item({ id: "b", bookId: "book-2", createdAt: 200 }),
    ];
    expect(homeSelection(shelf, ["a"], {}).hero?.id).toBe("b");
  });

  it("falls back to newest exports when nothing was listened", () => {
    const shelf = [
      item({ id: "old", createdAt: 100 }),
      item({ id: "new", bookId: "book-2", createdAt: 900 }),
    ];
    expect(homeSelection(shelf, [], {}).hero?.id).toBe("new");
  });

  it("fills jump with at most four items from distinct books", () => {
    const shelf = [
      item({ id: "hero", bookId: "book-h", createdAt: 900 }),
      item({ id: "h2", bookId: "book-h", createdAt: 850 }),
      ...Array.from({ length: 6 }, (_v, i) =>
        item({ id: `j${i}`, bookId: `book-${i}`, createdAt: 800 - i }),
      ),
    ];
    const picked = homeSelection(shelf, ["hero"], {});
    expect(picked.jump).toHaveLength(JUMP_COUNT);
    expect(picked.jump.map((entry) => entry.bookId)).not.toContain("book-h");
    const books = picked.jump.map((entry) => entry.bookId);
    expect(new Set(books).size).toBe(books.length);
  });

  it("prefers a read-along-capable export for the hero over a newer audio-only one", () => {
    const shelf = [
      item({ id: "wav", format: "wav", createdAt: 900 }),
      item({ id: "mp3", bookId: "book-2", createdAt: 100 }),
    ];
    const picked = homeSelection(shelf, ["wav"], {});
    expect(picked.hero?.id).toBe("mp3");
    expect(picked.heroReadAlong).toBe(true);
  });

  it("admits an audio-only hero honestly when nothing read-along exists", () => {
    const picked = homeSelection([item({ format: "wav" })], [], {});
    expect(picked.hero?.id).toBe("export-1");
    expect(picked.heroReadAlong).toBe(false);
  });

  it("returns an empty selection for an empty shelf", () => {
    expect(homeSelection([], ["ghost"], {})).toEqual({
      hero: null,
      heroListened: null,
      heroReadAlong: false,
      jump: [],
    });
  });
});

function index(): ReadingIndex {
  return {
    jobId: "job-1",
    bookId: "book-1",
    origin: "measured",
    totalMs: 600_000,
    chapters: [
      {
        chapterId: "c1",
        idx: 0,
        title: "The Alphabet",
        pageStart: 1,
        pageEnd: 10,
        startMs: 0,
        endMs: 200_000,
        segmentCount: 10,
      },
      {
        chapterId: "c2",
        idx: 1,
        title: "Verb Forms",
        pageStart: 11,
        pageEnd: 20,
        startMs: 200_000,
        endMs: 600_000,
        segmentCount: 10,
      },
    ],
  };
}

describe("positionLine", () => {
  it("names the chapter and position the fraction lands in", () => {
    expect(positionLine(index(), 0.5)).toBe("Chapter 2 · Verb Forms · 5:00");
  });

  it("starts at the first chapter with no position for unstarted audio", () => {
    expect(positionLine(index(), null)).toBe("Chapter 1 · The Alphabet");
    expect(positionLine(index(), 0)).toBe("Chapter 1 · The Alphabet");
  });

  it("returns null when the index cannot orient anyone", () => {
    expect(positionLine({ ...index(), chapters: [] }, 0.5)).toBeNull();
    expect(positionLine({ ...index(), totalMs: 0 }, 0.5)).toBeNull();
  });
});

describe("formatPosition", () => {
  it("formats minutes and hours", () => {
    expect(formatPosition(0)).toBe("0:00");
    expect(formatPosition(761_000)).toBe("12:41");
    expect(formatPosition(3_723_000)).toBe("1:02:03");
  });
});
