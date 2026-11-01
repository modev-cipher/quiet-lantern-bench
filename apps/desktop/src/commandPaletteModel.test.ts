import { describe, expect, it } from "vitest";

import {
  PALETTE_GROUP_LIMIT,
  matchScore,
  moveSelection,
  normalizeForSearch,
  rankPalette,
  type PaletteEntry,
} from "./commandPaletteModel";

const book = (id: string, label: string, detail?: string): PaletteEntry => ({
  group: "books",
  id: `book:${id}`,
  label,
  detail,
  target: { kind: "book", bookId: id, hasPages: true, playable: null },
});
const go = (id: string, label: string): PaletteEntry => ({
  group: "go",
  id: `go:${id}`,
  label,
  target: { kind: "view", view: "library" },
});

describe("normalizeForSearch", () => {
  it("folds case and Latin diacritics", () => {
    expect(normalizeForSearch("Brontë ÉLAN")).toBe("bronte elan");
  });

  it("drops Arabic harakat and tatweel and unifies alef, ta marbuta and alef maqsura", () => {
    expect(normalizeForSearch("الإِعْدَادَات")).toBe("الاعدادات");
    expect(normalizeForSearch("مـكـتـبـة")).toBe("مكتبه");
    expect(normalizeForSearch("أحمد آمن إلى")).toBe("احمد امن الي");
  });
});

describe("matchScore", () => {
  it("ranks a prefix above a word start above a substring above a scattered match", () => {
    const prefix = matchScore("jane", "Jane Eyre");
    const word = matchScore("eyre", "Jane Eyre");
    const inside = matchScore("yre", "Jane Eyre");
    const scattered = matchScore("jey", "Jane Eyre");
    expect(prefix).toBeGreaterThan(word);
    expect(word).toBeGreaterThan(inside);
    expect(inside).toBeGreaterThan(scattered);
    expect(scattered).toBeGreaterThan(0);
  });

  it("refuses letters that are not all present in order", () => {
    expect(matchScore("zq", "Jane Eyre")).toBe(0);
    expect(matchScore("eyj", "Jane Eyre")).toBe(0);
  });

  it("matches the detail line, but below the label", () => {
    const viaDetail = matchScore("bronte", "Jane Eyre", "Charlotte Brontë");
    expect(viaDetail).toBeGreaterThan(0);
    expect(viaDetail).toBeLessThan(matchScore("jane", "Jane Eyre"));
  });

  it("matches Arabic without its diacritics", () => {
    expect(matchScore("الاعدادات", "الإعدادات")).toBeGreaterThan(0);
  });
});

describe("rankPalette", () => {
  const entries: PaletteEntry[] = [
    go("shelf", "Shelf"),
    book("a", "Middlemarch", "George Eliot"),
    book("b", "Jane Eyre", "Charlotte Brontë"),
    {
      group: "chapters",
      id: "ch:3",
      label: "Chapter 3: The red-room",
      target: { kind: "chapter", startMs: 120_000 },
    },
    {
      group: "actions",
      id: "act:tutor",
      label: "Talk to the tutor",
      target: { kind: "action", action: "tutor" },
    },
  ];

  it("keeps the fixed group order whatever order entries arrive in", () => {
    expect(rankPalette(entries, "").map((g) => g.group)).toEqual([
      "books",
      "chapters",
      "go",
      "actions",
    ]);
  });

  it("filters to matches and orders them best-first inside a group", () => {
    const ranked = rankPalette(
      [book("x", "Eyre notes"), book("b", "Jane Eyre"), book("z", "Ivanhoe")],
      "eyre",
    );
    expect(ranked).toHaveLength(1);
    expect(ranked[0].entries.map((e) => e.id)).toEqual(["book:x", "book:b"]);
  });

  it("drops groups with no match", () => {
    expect(rankPalette(entries, "red").map((g) => g.group)).toEqual([
      "chapters",
    ]);
  });

  it("caps each group so one long library cannot bury the rest", () => {
    const many = Array.from({ length: 12 }, (_, i) =>
      book(String(i), `Book ${i}`),
    );
    const ranked = rankPalette([...many, go("shelf", "Shelf")], "");
    expect(ranked[0].entries).toHaveLength(PALETTE_GROUP_LIMIT);
    expect(ranked[1].group).toBe("go");
  });

  it("returns nothing when nothing matches", () => {
    expect(rankPalette(entries, "qqq")).toEqual([]);
  });
});

describe("moveSelection", () => {
  it("wraps at both ends and stays put with nothing to select", () => {
    expect(moveSelection(0, 1, 3)).toBe(1);
    expect(moveSelection(2, 1, 3)).toBe(0);
    expect(moveSelection(0, -1, 3)).toBe(2);
    expect(moveSelection(0, 1, 0)).toBe(0);
  });
});
