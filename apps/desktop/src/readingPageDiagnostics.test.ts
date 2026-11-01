import { describe, expect, it } from "vitest";

import golden from "../e2e/fixtures/reading-timeline.golden.json";
import {
  emptyRungs,
  formatPageMode,
  pageModeSample,
  shouldEmitPageMode,
} from "./readingPageDiagnostics";
import type { PageModeSample } from "./readingPageDiagnostics";
import type { ReadingChapter } from "./readingModel";

const chapters = golden.chapters as ReadingChapter[];

describe("the page mode sample", () => {
  it("counts the rungs the payload actually delivered, not the rungs it hoped for", () => {
    const sample = pageModeSample("page", "user-preference", chapters[0]);

    expect(sample.rungs).toEqual({ word: 1, line: 0, block: 1, page: 0 });
    expect(sample.segments).toBe(2);
    expect(sample.anchored).toBe(2);
    expect(sample.anchorGeometry).toBe("ensured");
  });

  it("reports the degraded chapter as degraded", () => {
    const sample = pageModeSample("page", "user-preference", chapters[1]);

    expect(sample.rungs).toEqual({ word: 0, line: 0, block: 2, page: 2 });
    expect(sample.anchored).toBe(2);
    expect(sample.anchorGeometry).toBe("unavailable");
  });

  it("survives having no chapter yet", () => {
    const sample = pageModeSample("script", "teacher", null);

    expect(sample.chapterId).toBeNull();
    expect(sample.rungs).toEqual(emptyRungs());
    expect(sample.segments).toBe(0);
  });
});

describe("the emission rule", () => {
  const base = pageModeSample("script", "user-preference", chapters[0]);

  it("emits the first sample", () => {
    expect(shouldEmitPageMode(null, base)).toBe(true);
  });

  it("stays quiet while nothing about the surface changed", () => {
    expect(
      shouldEmitPageMode(base, pageModeSample("script", "user-preference", chapters[0])),
      "this line is sampled on every cursor tick, and a per-tick logger is the defect this stage is fixing elsewhere",
    ).toBe(false);
  });

  it("emits on a mode switch, a chapter turn, a reason change and a geometry change", () => {
    expect(shouldEmitPageMode(base, { ...base, mode: "page" })).toBe(true);
    expect(shouldEmitPageMode(base, { ...base, chapterId: "chap-1" })).toBe(true);
    expect(shouldEmitPageMode(base, { ...base, reason: "teacher" })).toBe(true);
    expect(
      shouldEmitPageMode(base, { ...base, anchorGeometry: "unavailable" }),
      "a chapter that recovers from a degraded first open must be visible in the log, because that is the one case the warm-reopen invariant does not cover",
    ).toBe(true);
  });
});

describe("the formatted line", () => {
  it("names every field the LOG REVIEW greps for", () => {
    const line = formatPageMode(pageModeSample("page", "teacher", chapters[0]));

    expect(line).toBe(
      "mode=page reason=teacher chapter=chap-0 anchorGeometry=ensured segments=2 word=1 line=0 block=1 page=0 anchored=2",
    );
  });

  it("shortens a real chapter id and says so when there is none", () => {
    const sample: PageModeSample = {
      ...pageModeSample("page", "user-preference", chapters[0]),
      chapterId: "0123456789abcdef",
    };

    expect(formatPageMode(sample)).toContain("chapter=01234567");
    expect(
      formatPageMode(pageModeSample("script", "user-preference", null)),
    ).toContain("chapter=n/a anchorGeometry=n/a");
  });
});
