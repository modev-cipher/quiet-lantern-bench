import { describe, expect, it } from "vitest";

import {
  audioFileName,
  buildAudioShelf,
  durationParts,
  formatAudioDate,
  formatAudioSize,
  canPlay,
  isPlayable,
  outputFormatOf,
  type ExportSummary,
} from "./audioShelfModel";
import type { BookSummary } from "./libraryModel";

function exported(overrides: Partial<ExportSummary> = {}): ExportSummary {
  return {
    id: "export-1",
    bookId: "book-1",
    jobId: "job-1",
    path: "/exports/book-1.mp3",
    bytes: 5 * 1024 * 1024,
    format: "mp3",
    mode: "narrator",
    stale: false,
    createdAt: 1000,
    ...overrides,
  };
}

function book(overrides: Partial<BookSummary> = {}): BookSummary {
  return {
    id: "book-1",
    title: "The Pragmatic Programmer",
    author: "Hunt",
    pageCount: 300,
    chapterCount: 10,
    language: "en",
    isScanned: false,
    coverPath: null,
    createdAt: 0,
    hasPseudoChapters: false,
    layoutAnalysed: false,
    blocksStale: false,
    reflowed: false,
    ...overrides,
  };
}

describe("durationParts", () => {
  it("splits a cast length into whole hours and rounded minutes", () => {
    expect(durationParts(2 * 3_600_000 + 14 * 60_000 + 29_000)).toEqual({
      h: 2,
      m: 14,
    });
    expect(durationParts(47 * 60_000 + 31_000)).toEqual({ h: 0, m: 48 });
  });

  it("never shows a zero-minute cast and carries 59.5 minutes into the hour", () => {
    expect(durationParts(12_000)).toEqual({ h: 0, m: 1 });
    expect(durationParts(59 * 60_000 + 40_000)).toEqual({ h: 1, m: 0 });
  });

  it("has nothing to say about an unknown or empty length", () => {
    expect(durationParts(null)).toBeNull();
    expect(durationParts(undefined)).toBeNull();
    expect(durationParts(0)).toBeNull();
    expect(durationParts(Number.NaN)).toBeNull();
  });
});

describe("buildAudioShelf", () => {
  it("carries the probed length and seek exactness through to the shelf", () => {
    const [item] = buildAudioShelf(
      [exported({ durationMs: 90_000, seekExact: false })],
      [book()],
    );
    expect(item!.durationMs).toBe(90_000);
    expect(item!.seekExact).toBe(false);
    const [unknown] = buildAudioShelf([exported()], [book()]);
    expect(unknown!.durationMs).toBeNull();
    expect(unknown!.seekExact).toBeNull();
  });

  it("joins the book title", () => {
    const items = buildAudioShelf([exported()], [book()]);
    expect(items).toHaveLength(1);
    expect(items[0]!.title).toBe("The Pragmatic Programmer");
    expect(items[0]!.path).toBe("/exports/book-1.mp3");
    expect(items[0]!.format).toBe("mp3");
    expect(items[0]!.mode).toBe("narrator");
  });

  it("names an adopted orphan after its file when no book owns it", () => {
    const items = buildAudioShelf(
      [
        exported({
          bookId: null,
          path: "/exports/Some Book - ch 1 - teacher.mp3",
        }),
      ],
      [],
    );
    expect(items[0]!.title).toBe("Some Book - ch 1 - teacher.mp3");
  });

  it("keeps an export whose job was deleted", () => {
    const items = buildAudioShelf([exported({ jobId: null })], [book()]);
    expect(items).toHaveLength(1);
    expect(items[0]!.title).toBe("The Pragmatic Programmer");
  });

  it("sorts newest first", () => {
    const items = buildAudioShelf(
      [
        exported({ id: "old", createdAt: 100, path: "/x/old.mp3" }),
        exported({ id: "new", createdAt: 900, path: "/x/new.mp3" }),
      ],
      [book()],
    );
    expect(items.map((item) => item.id)).toEqual(["new", "old"]);
  });

  it("is empty when nothing has been exported", () => {
    expect(buildAudioShelf([], [book()])).toEqual([]);
  });
});

describe("formatAudioSize", () => {
  it("renders megabytes and kilobytes", () => {
    expect(formatAudioSize(5 * 1024 * 1024)).toBe("5.0 MB");
    expect(formatAudioSize(12 * 1024 * 1024)).toBe("12 MB");
    expect(formatAudioSize(2048)).toBe("2 KB");
  });

  it("renders nothing for an unknown size", () => {
    expect(formatAudioSize(0)).toBe("");
  });
});

describe("outputFormatOf", () => {
  it("reads the extension from the path", () => {
    expect(outputFormatOf("/exports/x.MP3", "wav")).toBe("mp3");
    expect(outputFormatOf("/exports/clip.wav", "mp3")).toBe("wav");
  });

  it("falls back when there is no extension", () => {
    expect(outputFormatOf("/exports/noext", "mp3")).toBe("mp3");
  });
});

describe("isPlayable", () => {
  it("accepts audio formats and rejects video and unknown formats", () => {
    expect(isPlayable("mp3")).toBe(true);
    expect(isPlayable("wav")).toBe(true);
    expect(isPlayable("mp4")).toBe(false);
    expect(isPlayable("bin")).toBe(false);
  });
});

describe("canPlay", () => {
  it("refuses an export whose audio no longer matches its chapters", () => {
    const [fresh] = buildAudioShelf([exported()], []);
    expect(canPlay(fresh!)).toBe(true);
    const [stale] = buildAudioShelf([exported({ stale: true })], []);
    expect(canPlay(stale!)).toBe(false);
    expect(stale!.stale).toBe(true);
    expect(stale!.jobId).toBe("job-1");
  });

  it("still refuses a format the player cannot open, stale or not", () => {
    const [video] = buildAudioShelf(
      [exported({ path: "/exports/a.mp4", format: "mp4" })],
      [],
    );
    expect(canPlay(video!)).toBe(false);
  });
});

describe("audioFileName", () => {
  it("takes the final path segment for posix and windows paths", () => {
    expect(audioFileName("/exports/book-1.mp3")).toBe("book-1.mp3");
    expect(audioFileName("C:\\exports\\book-1.mp3")).toBe("book-1.mp3");
  });
});

describe("formatAudioDate", () => {
  it("formats a real timestamp for both en and ar", () => {
    const timestamp = Date.UTC(2026, 6, 13);
    expect(formatAudioDate(timestamp, "en")).toContain("2026");
    expect(formatAudioDate(timestamp, "ar")).toContain("2026");
  });

  it("returns empty for invalid timestamps", () => {
    expect(formatAudioDate(0)).toBe("");
    expect(formatAudioDate(Number.NaN)).toBe("");
  });
});
