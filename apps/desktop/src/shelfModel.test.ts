import { describe, expect, it } from "vitest";

import {
  CLOTH_TONES,
  SHELF_BOOK_HEIGHT,
  bookDirection,
  clothTone,
  partitionShelf,
  ribbonLength,
  shelfStatus,
  spineWidth,
  type ShelfStatus,
} from "./shelfModel";

describe("spineWidth", () => {
  it("follows the printers' rule: pages over 444 PPI plus the boards, at the shelf's scale", () => {
    expect(spineWidth(264)).toBe(17);
    expect(spineWidth(432)).toBe(25);
  });

  it("clamps very thin and very thick books to a buildable spine", () => {
    expect(spineWidth(12)).toBe(10);
    expect(spineWidth(4000)).toBe(44);
  });

  it("falls back to an ordinary book when the page count is unknown or broken", () => {
    expect(spineWidth(null)).toBe(spineWidth(240));
    expect(spineWidth(Number.NaN)).toBe(spineWidth(240));
    expect(spineWidth(-5)).toBe(spineWidth(240));
  });

  it("scales with the drawn height", () => {
    expect(spineWidth(432, SHELF_BOOK_HEIGHT * 2)).toBe(49);
  });
});

const base = {
  improving: false,
  casting: false,
  playable: false,
  stale: false,
  listened: null,
  durationMs: null,
};

describe("shelfStatus", () => {
  it("says what a book needs, in one priority order", () => {
    expect(shelfStatus({ ...base, improving: true, casting: true })).toEqual({
      kind: "improving",
    });
    expect(shelfStatus({ ...base, casting: true, playable: true })).toEqual({
      kind: "casting",
    });
    expect(shelfStatus({ ...base, playable: true })).toEqual({
      kind: "audio",
    });
    expect(shelfStatus({ ...base, stale: true })).toEqual({ kind: "stale" });
    expect(shelfStatus(base)).toEqual({ kind: "ready" });
  });

  it("turns listening into progress and time left, and a finished book into finished", () => {
    expect(
      shelfStatus({
        ...base,
        playable: true,
        listened: 0.25,
        durationMs: 4 * 3_600_000,
      }),
    ).toEqual({ kind: "listen", progress: 0.25, leftMs: 3 * 3_600_000 });
    expect(shelfStatus({ ...base, playable: true, listened: 0.4 })).toEqual({
      kind: "listen",
      progress: 0.4,
      leftMs: null,
    });
    expect(shelfStatus({ ...base, playable: true, listened: 0.99 })).toEqual({
      kind: "finished",
    });
  });

  it("ignores progress that is not a real fraction", () => {
    expect(
      shelfStatus({ ...base, playable: true, listened: Number.NaN }),
    ).toEqual({ kind: "audio" });
    expect(shelfStatus({ ...base, playable: true, listened: 0 })).toEqual({
      kind: "audio",
    });
  });
});

describe("partitionShelf", () => {
  it("puts books being listened to or cast on the In progress shelf, keeping order", () => {
    const statuses: Record<string, ShelfStatus> = {
      a: { kind: "ready" },
      b: { kind: "listen", progress: 0.3, leftMs: null },
      c: { kind: "casting" },
      d: { kind: "finished" },
    };
    const { inProgress, onShelf } = partitionShelf(
      ["a", "b", "c", "d"],
      (id) => statuses[id]!,
    );
    expect(inProgress).toEqual(["b", "c"]);
    expect(onShelf).toEqual(["a", "d"]);
  });
});

describe("ribbonLength", () => {
  it("is absent until listening starts, then grows with progress", () => {
    expect(ribbonLength(null)).toBe(0);
    expect(ribbonLength(0)).toBe(0);
    expect(ribbonLength(0.25)).toBeLessThan(ribbonLength(0.75));
    expect(ribbonLength(1)).toBeLessThanOrEqual(SHELF_BOOK_HEIGHT);
  });
});

describe("clothTone", () => {
  it("gives a book the same cloth every time, from the shared palette", () => {
    expect(clothTone("book-a")).toBe(clothTone("book-a"));
    for (const seed of ["", "a", "book-b", "كتاب", "x".repeat(500)]) {
      const tone = clothTone(seed);
      expect(Number.isInteger(tone)).toBe(true);
      expect(tone).toBeGreaterThanOrEqual(0);
      expect(tone).toBeLessThan(CLOTH_TONES);
    }
  });

  it("spreads a typical library across several cloths", () => {
    const seen = new Set(
      Array.from({ length: 40 }, (_, at) => clothTone(`book-${at}`)),
    );
    expect(seen.size).toBeGreaterThanOrEqual(6);
  });
});

describe("bookDirection", () => {
  it("binds a book by its own script, not the app's language", () => {
    expect(bookDirection("كتاب القراءة الهادئة")).toBe("rtl");
    expect(bookDirection("ספר")).toBe("rtl");
    expect(bookDirection("Meditations")).toBe("ltr");
  });

  it("decides by the first letter that carries a direction", () => {
    expect(bookDirection("1984 — رواية")).toBe("rtl");
    expect(bookDirection("«Clean Code» بالعربية")).toBe("ltr");
    expect(bookDirection("")).toBe("ltr");
  });
});
