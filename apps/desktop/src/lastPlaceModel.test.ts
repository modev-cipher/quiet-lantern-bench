import { describe, expect, it } from "vitest";

import type { AudioShelfItem } from "./audioShelfModel";
import {
  parseLastPlace,
  placeOf,
  resolveLastPlace,
  serialiseLastPlace,
  type LastPlace,
} from "./lastPlaceModel";
import type { BookSummary } from "./libraryModel";

function book(overrides: Partial<BookSummary> = {}): BookSummary {
  return {
    id: "book-1",
    title: "A Book",
    pageCount: 12,
    isScanned: false,
    ...overrides,
  } as BookSummary;
}

function cast(overrides: Partial<AudioShelfItem> = {}): AudioShelfItem {
  return {
    id: "export-1",
    bookId: "book-1",
    jobId: "job-1",
    title: "A Book",
    format: "mp3",
    mode: "narrate",
    stale: false,
    ...overrides,
  } as AudioShelfItem;
}

describe("parseLastPlace", () => {
  it("round-trips both kinds of place", () => {
    const places: LastPlace[] = [
      { kind: "cast", bookId: "book-1", exportId: "export-1" },
      { kind: "pages", bookId: "book-1" },
    ];
    for (const place of places) {
      expect(parseLastPlace(serialiseLastPlace(place))).toEqual(place);
    }
  });

  it("treats anything it cannot prove is a place as no place", () => {
    for (const raw of [
      null,
      "",
      "not json",
      "null",
      "[]",
      '{"kind":"cast","bookId":"book-1"}',
      '{"kind":"cast","bookId":7,"exportId":"e"}',
      '{"kind":"pages"}',
      '{"kind":"shelf","bookId":"book-1"}',
      '{"kind":"pages","bookId":""}',
    ]) {
      expect(parseLastPlace(raw), String(raw)).toBeNull();
    }
  });
});

describe("placeOf", () => {
  it("remembers a cast by its export and a page reader by its book", () => {
    expect(placeOf(cast())).toEqual({
      kind: "cast",
      bookId: "book-1",
      exportId: "export-1",
    });
    expect(
      placeOf({
        kind: "bookSurface",
        bookId: "book-1",
        title: "A Book",
        isScanned: false,
        pageCount: 12,
      }),
    ).toEqual({ kind: "pages", bookId: "book-1" });
  });
});

describe("resolveLastPlace", () => {
  it("reopens the cast that was open when it can still be followed", () => {
    const resolved = resolveLastPlace(
      { kind: "cast", bookId: "book-1", exportId: "export-1" },
      [book()],
      [cast()],
    );
    expect(resolved).toEqual({ kind: "cast", item: cast() });
  });

  it("falls back to the book's pages when the cast is gone, stale or not followable", () => {
    const place: LastPlace = {
      kind: "cast",
      bookId: "book-1",
      exportId: "export-1",
    };
    for (const shelf of [
      [],
      [cast({ stale: true })],
      [cast({ jobId: null })],
      [cast({ format: "wav" })],
    ]) {
      const resolved = resolveLastPlace(place, [book()], shelf);
      expect(resolved?.kind).toBe("pages");
    }
  });

  it("reopens a page reader on its book", () => {
    const resolved = resolveLastPlace(
      { kind: "pages", bookId: "book-1" },
      [book({ isScanned: true })],
      [],
    );
    expect(resolved).toEqual({
      kind: "pages",
      surface: {
        kind: "bookSurface",
        bookId: "book-1",
        title: "A Book",
        isScanned: true,
        pageCount: 12,
      },
    });
  });

  it("gives up when the book was deleted or has nothing to render", () => {
    const pages: LastPlace = { kind: "pages", bookId: "book-1" };
    const castPlace: LastPlace = {
      kind: "cast",
      bookId: "book-1",
      exportId: "export-1",
    };
    expect(resolveLastPlace(pages, [], [])).toBeNull();
    expect(resolveLastPlace(pages, [book({ pageCount: 0 })], [])).toBeNull();
    expect(resolveLastPlace(pages, [book({ pageCount: null })], [])).toBeNull();
    expect(resolveLastPlace(castPlace, [], [cast()])).toBeNull();
    expect(
      resolveLastPlace(castPlace, [book({ pageCount: 0 })], []),
    ).toBeNull();
  });

  it("never reopens another book's cast under a reused export id", () => {
    const resolved = resolveLastPlace(
      { kind: "cast", bookId: "book-1", exportId: "export-1" },
      [book(), book({ id: "book-2" })],
      [cast({ bookId: "book-2" })],
    );
    expect(resolved?.kind).toBe("pages");
  });

  it("has nothing to resolve without a stored place", () => {
    expect(resolveLastPlace(null, [book()], [cast()])).toBeNull();
  });
});
