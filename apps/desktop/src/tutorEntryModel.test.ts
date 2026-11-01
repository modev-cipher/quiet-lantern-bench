// Talk entry routing (owner decision 2026-10-09, superseding F8 §5.3): a
// book-voice entry opens the book's own pages whenever they render — never
// the last cast. A readable cast is the fallback for books without page
// renders; the Study card only when nothing renders.

import { describe, expect, it } from "vitest";

import type { AudioShelfItem } from "./audioShelfModel";
import type { BookSummary } from "./libraryModel";
import { tutorEntryRoute } from "./tutorEntryModel";

function book(overrides: Partial<BookSummary> = {}): BookSummary {
  return {
    id: "b1",
    title: "The Book",
    author: null,
    pageCount: 40,
    chapterCount: 3,
    language: null,
    isScanned: false,
    coverPath: null,
    createdAt: 1,
    hasPseudoChapters: false,
    layoutAnalysed: true,
    reflowed: false,
    blocksStale: false,
    ...overrides,
  };
}

function shelfItem(overrides: Partial<AudioShelfItem> = {}): AudioShelfItem {
  return {
    id: "e1",
    bookId: "b1",
    title: "The Book",
    path: "/x/e1.mp3",
    mode: "narrate",
    format: "mp3",
    bytes: 1,
    stale: false,
    isScanned: false,
    jobId: "j1",
    createdAt: 10,
    ...overrides,
  };
}

describe("tutorEntryRoute (P9 routing)", () => {
  it("a book with pages opens its pages even when a readable cast exists", () => {
    const route = tutorEntryRoute(book(), [shelfItem()]);
    expect(route.kind).toBe("bookSurface");
  });

  it("a book WITHOUT page renders but with a readable cast routes to the reader", () => {
    const route = tutorEntryRoute(book({ pageCount: null }), [shelfItem()]);
    expect(route.kind).toBe("reader");
  });

  it("picks the NEWEST followable cast", () => {
    const route = tutorEntryRoute(book({ pageCount: null }), [
      shelfItem({ id: "old", createdAt: 1 }),
      shelfItem({ id: "new", createdAt: 99 }),
    ]);
    expect(route).toMatchObject({ kind: "reader", item: { id: "new" } });
  });

  it("a stale or non-followable cast does not count as readable", () => {
    const route = tutorEntryRoute(book({ pageCount: null }), [
      shelfItem({ stale: true }),
      shelfItem({ id: "e2", format: "wav" }),
      shelfItem({ id: "e3", jobId: null }),
      shelfItem({ id: "other-book", bookId: "b2" }),
    ]);
    expect(route.kind).toBe("study");
  });

  it("a book WITHOUT a cast but with rendered pages routes to BookSurface", () => {
    const route = tutorEntryRoute(book(), []);
    expect(route).toEqual({
      kind: "bookSurface",
      surface: {
        kind: "bookSurface",
        bookId: "b1",
        title: "The Book",
        isScanned: false,
        pageCount: 40,
      },
    });
  });

  it("a book with nothing renderable falls back to the Study card", () => {
    expect(tutorEntryRoute(book({ pageCount: null }), []).kind).toBe("study");
    expect(tutorEntryRoute(book({ pageCount: 0 }), []).kind).toBe("study");
  });

  it("an unresolved book falls back to the Study card", () => {
    expect(tutorEntryRoute(null, [shelfItem()]).kind).toBe("study");
  });
});
