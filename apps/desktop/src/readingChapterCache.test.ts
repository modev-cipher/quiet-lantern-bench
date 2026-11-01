import { describe, expect, it } from "vitest";

import golden from "../e2e/fixtures/reading-timeline.golden.json";
import { chapterKey, createChapterCache } from "./readingChapterCache";
import type { ReadingChapter } from "./readingModel";

const chapters = golden.chapters as ReadingChapter[];

describe("the chapter cache", () => {
  it("serves a chapter it already fetched", () => {
    const cache = createChapterCache();
    cache.set("job-1", "chap-0", chapters[0]);

    expect(cache.get("job-1", "chap-0")).toBe(chapters[0]);
  });

  it("never serves a cached chapter across a job change", () => {
    const cache = createChapterCache();
    cache.set("job-1", "chap-0", chapters[0]);

    expect(
      cache.get("job-2", "chap-0"),
      "chapter ids are book-scoped, so the same id under another job is another payload",
    ).toBeUndefined();
  });

  it("cannot be confused by a job id that contains the separator", () => {
    expect(chapterKey("a\u0000b", "c")).not.toBe(chapterKey("a", "b\u0000c"));
  });

  it("forgets everything on flush, which is what overlay close calls", () => {
    const cache = createChapterCache();
    cache.set("job-1", "chap-0", chapters[0]);
    cache.set("job-1", "chap-1", chapters[1]);

    expect(cache.size()).toBe(2);

    cache.flush();

    expect(cache.size()).toBe(0);
    expect(cache.get("job-1", "chap-0")).toBeUndefined();
  });

  it("keeps one entry per chapter rather than growing on a refetch", () => {
    const cache = createChapterCache();
    cache.set("job-1", "chap-0", chapters[0]);
    cache.set("job-1", "chap-0", chapters[0]);

    expect(cache.size()).toBe(1);
  });

  it("holds the payload identity the cursor functions were given", () => {
    const cache = createChapterCache();
    cache.set("job-1", "chap-0", chapters[0]);
    const first = cache.get("job-1", "chap-0");
    const second = cache.get("job-1", "chap-0");

    expect(
      first,
      "the cursor guarantees hold over a non-mutating payload, so identity must be stable",
    ).toBe(second);
  });
});
