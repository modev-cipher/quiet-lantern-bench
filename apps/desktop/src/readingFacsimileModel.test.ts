import { describe, expect, it } from "vitest";

import golden from "../e2e/fixtures/reading-timeline.golden.json";
import {
  aspectRatio,
  type FigurePageHold,
  figureForSegment,
  figureViewport,
  framedFigureId,
  filterWantedPages,
  normalisedRect,
  pagesToFetch,
  pageToShow,
  updateFigureHold,
  MAX_FIGURE_PAGE_HOLD,
  MAX_FIGURE_ZOOM,
} from "./readingFacsimileModel";
import type {
  ReadingChapter,
  ReadingFigure,
  ReadingPage,
  ReadingSegment,
} from "./readingModel";

const pages = golden.pages as ReadingPage[];
const figures = golden.figures as ReadingFigure[];
const chapters = golden.chapters as ReadingChapter[];

function page(overrides: Partial<ReadingPage> = {}): ReadingPage {
  return {
    pageNo: 4,
    widthPt: 612,
    heightPt: 792,
    imagePath: "<cache>/pa/page-hash-4",
    ...overrides,
  };
}

function figure(overrides: Partial<ReadingFigure> = {}): ReadingFigure {
  return {
    figureId: "figure-7",
    pageNo: 4,
    bbox: [72, 120.5, 468, 396.25],
    cropHash: "crop-hash-7",
    ...overrides,
  };
}

function segment(overrides: Partial<ReadingSegment> = {}): ReadingSegment {
  return {
    segmentId: "seg-test",
    idx: 0,
    pageNo: 3,
    kind: "text",
    anchor: "word",
    rects: [],
    figureId: null,
    text: "A line.",
    startMs: 0,
    endMs: 900,
    provenance: "fresh",
    words: [],
    ...overrides,
  };
}

function visibleWindow(scale: number, originPct: number) {
  const half = 0.5 / scale;
  const origin = originPct / 100;
  return { from: origin - half, to: origin + half };
}

describe("framedFigureId", () => {
  it("frames any segment that carries a figure, whatever its kind", () => {
    expect(
      framedFigureId(segment({ figureId: "figure-9", kind: "text" })),
    ).toBe("figure-9");
    expect(
      framedFigureId(segment({ figureId: "figure-7", kind: "figure_cue" })),
    ).toBe("figure-7");
  });

  it("frames nothing when the segment anchors nothing, cue or not", () => {
    expect(
      framedFigureId(segment({ figureId: null, kind: "figure_cue" })),
    ).toBeNull();
    expect(framedFigureId(segment({ figureId: null }))).toBeNull();
    expect(framedFigureId(null)).toBeNull();
  });

  it("frames every one of the golden's anchoring segments", () => {
    const anchoring = chapters
      .flatMap((chapter) => chapter.segments)
      .filter((entry) => framedFigureId(entry) != null);

    expect(anchoring.map((entry) => entry.segmentId)).toEqual([
      "seg-0002",
      "seg-0003",
      "seg-0004",
    ]);
    expect(
      anchoring.map((entry) => entry.kind),
      "a cue and two plain text segments, so kind independence is exercised",
    ).toEqual(["figure_cue", "text", "text"]);
  });
});

describe("pageToShow", () => {
  it("shows the figure's own page, not the segment's", () => {
    const cue = chapters[1].segments[0];

    expect(cue.pageNo).toBe(3);
    expect(pageToShow(cue, figures)).toBe(4);
  });

  it("shows the figure's page for a text segment too", () => {
    const text = chapters[1].segments[1];

    expect(text.pageNo).toBe(3);
    expect(pageToShow(text, figures)).toBe(2);
  });

  it("leaves a figure that already sits on its segment's page where it is", () => {
    const samePage = chapters[1].segments[2];

    expect(
      samePage.pageNo,
      "the ordinary book layout, and the only shape where the page-identity gate stays quiet while the zoom still unmounts the overlay",
    ).toBe(4);
    expect(pageToShow(samePage, figures)).toBe(4);
  });

  it("falls back to the segment's page when it anchors no figure", () => {
    expect(pageToShow(segment({ pageNo: 7 }), figures)).toBe(7);
  });

  it("falls back to the segment's page when the figure was not served", () => {
    expect(
      pageToShow(segment({ figureId: "figure-404", pageNo: 3 }), figures),
    ).toBe(3);
  });
});

describe("pagesToFetch", () => {
  it("asks for the page and the one after it", () => {
    expect(pagesToFetch(2, 4, 1)).toEqual([2, 3]);
  });

  it("stops at the last page rather than asking for one past the end", () => {
    expect(pagesToFetch(4, 4, 1)).toEqual([4]);
    expect(pagesToFetch(9, 4, 1)).toEqual([4]);
  });

  it("does not collapse every page to zero when the last page is unknown", () => {
    expect(
      pagesToFetch(3, 0, 1),
      "a book whose chapter rows carry no page numbers yields lastPage 0; clamping to it would ask for page 0 forever and never show a page at all",
    ).toEqual([3, 4]);
    expect(pagesToFetch(1, 0, 1)).toEqual([1, 2]);
  });

  it("asks for nothing when there is no page to show", () => {
    expect(pagesToFetch(0, 4, 1)).toEqual([]);
    expect(pagesToFetch(Number.NaN, 4, 1)).toEqual([]);
  });

  it("warms the page behind when travel is backward", () => {
    expect(pagesToFetch(3, 4, -1)).toEqual([3, 2]);
  });

  it("stops at page 1 rather than asking for one before the start", () => {
    expect(pagesToFetch(1, 4, -1)).toEqual([1]);
  });

  it("warms backward under an unknown last page too", () => {
    expect(pagesToFetch(3, 0, -1)).toEqual([3, 2]);
  });
});

describe("aspectRatio", () => {
  it("comes from the page row so the placeholder cannot jump", () => {
    expect(aspectRatio(page())).toBeCloseTo(612 / 792, 10);
  });

  it("is unknown rather than guessed when the row has no geometry", () => {
    expect(aspectRatio(page({ widthPt: null }))).toBeNull();
    expect(aspectRatio(page({ heightPt: null }))).toBeNull();
    expect(aspectRatio(page({ widthPt: 0, heightPt: 0 }))).toBeNull();
    expect(aspectRatio(null)).toBeNull();
  });

  it("is known for every page the golden serves, cached or evicted", () => {
    for (const entry of pages) {
      expect(aspectRatio(entry), `page ${entry.pageNo}`).not.toBeNull();
    }
  });
});

describe("figureViewport", () => {
  it("keeps the whole figure inside the viewport", () => {
    const viewport = figureViewport(page(), figure());
    const rect = normalisedRect(page(), figure());

    expect(viewport).not.toBeNull();
    expect(rect).not.toBeNull();
    const horizontal = visibleWindow(
      (viewport as { scale: number }).scale,
      (viewport as { originXPct: number }).originXPct,
    );
    const vertical = visibleWindow(
      (viewport as { scale: number }).scale,
      (viewport as { originYPct: number }).originYPct,
    );
    const box = rect as {
      left: number;
      top: number;
      width: number;
      height: number;
    };

    expect(horizontal.from).toBeLessThanOrEqual(box.left);
    expect(horizontal.to).toBeGreaterThanOrEqual(box.left + box.width);
    expect(vertical.from).toBeLessThanOrEqual(box.top);
    expect(vertical.to).toBeGreaterThanOrEqual(box.top + box.height);
  });

  it("never pans past the edge of the page", () => {
    const corner = figure({ bbox: [0, 0, 60, 60] });
    const viewport = figureViewport(page(), corner);
    const scale = (viewport as { scale: number }).scale;
    const horizontal = visibleWindow(
      scale,
      (viewport as { originXPct: number }).originXPct,
    );
    const vertical = visibleWindow(
      scale,
      (viewport as { originYPct: number }).originYPct,
    );

    expect(horizontal.from).toBeGreaterThanOrEqual(-1e-9);
    expect(horizontal.to).toBeLessThanOrEqual(1 + 1e-9);
    expect(vertical.from).toBeGreaterThanOrEqual(-1e-9);
    expect(vertical.to).toBeLessThanOrEqual(1 + 1e-9);
  });

  it("never zooms out, and never past the declared ceiling", () => {
    const whole = figureViewport(page(), figure({ bbox: [0, 0, 612, 792] }));
    const tiny = figureViewport(page(), figure({ bbox: [300, 400, 302, 402] }));

    expect((whole as { scale: number }).scale).toBe(1);
    expect((tiny as { scale: number }).scale).toBe(MAX_FIGURE_ZOOM);
  });

  it("falls back to the whole page when the page has no point size", () => {
    expect(
      figureViewport(page({ widthPt: null, heightPt: null }), figure()),
      "D8 says a page without point sizes shows the crop with no pan",
    ).toBeNull();
  });

  it("falls back to the whole page when the figure has no bbox", () => {
    expect(figureViewport(page(), figure({ bbox: null }))).toBeNull();
  });

  it("refuses a bbox that does not fit its page rather than panning off it", () => {
    expect(
      figureViewport(page(), figure({ bbox: [72, 120, 900, 400] })),
    ).toBeNull();
    expect(
      figureViewport(page(), figure({ bbox: [-10, 120, 400, 400] })),
    ).toBeNull();
  });

  it("refuses a degenerate bbox instead of dividing by zero", () => {
    expect(
      figureViewport(page(), figure({ bbox: [100, 100, 100, 100] })),
    ).toBeNull();
    expect(
      Number.isNaN(
        figureViewport(page(), figure({ bbox: [100, 100, 100, 400] }))?.scale ??
          0,
      ),
    ).toBe(false);
  });

  it("frames the golden's figure on the page the golden serves it on", () => {
    const figure7 = figures[0];
    const pageFour = pages.find((entry) => entry.pageNo === figure7.pageNo);

    expect(pageFour).toBeDefined();
    expect(figureViewport(pageFour as ReadingPage, figure7)).not.toBeNull();
  });
});

describe("figureForSegment", () => {
  it("resolves the figure a segment anchors", () => {
    expect(figureForSegment(chapters[1].segments[0], figures)?.figureId).toBe(
      "figure-7",
    );
  });

  it("resolves nothing for a segment that anchors nothing", () => {
    expect(figureForSegment(segment(), figures)).toBeNull();
  });
});

describe("filterWantedPages", () => {
  it("wants a page when not yet in pages and not yet asked", () => {
    const pagesMap = new Map<number, ReadingPage>();
    const asked = new Set<number>();
    expect(filterWantedPages([1, 2], pagesMap, asked)).toEqual([1, 2]);
  });

  it("does not want a page that has already been asked, even if missing from pages", () => {
    const pagesMap = new Map<number, ReadingPage>();
    const asked = new Set<number>([1]);
    expect(filterWantedPages([1, 2], pagesMap, asked)).toEqual([2]);
  });

  it("does not want a page that already has an imagePath", () => {
    const pagesMap = new Map<number, ReadingPage>([
      [1, page({ pageNo: 1, imagePath: "/path/1.png" })],
    ]);
    const asked = new Set<number>();
    expect(filterWantedPages([1, 2], pagesMap, asked)).toEqual([2]);
  });

  it("wants a page that is known in pages but has no imagePath yet, if not asked", () => {
    const pagesMap = new Map<number, ReadingPage>([
      [1, page({ pageNo: 1, imagePath: null })],
    ]);
    const asked = new Set<number>();
    expect(filterWantedPages([1], pagesMap, asked)).toEqual([1]);
  });
});

describe("updateFigureHold (Teacher Mode Page Hysteresis & Seek Safety)", () => {
  const figFigures: ReadingFigure[] = [figure({ figureId: "fig-1", pageNo: 3 })];

  it("sets hold when an active figure is encountered during playback", () => {
    const figSeg = segment({ idx: 10, pageNo: 1, figureId: "fig-1" });
    const result = updateFigureHold(null, figSeg, figFigures, true, true);
    expect(result.pageNo).toBe(3);
    expect(result.hold).toEqual({
      pageNo: 3,
      remainingSegments: MAX_FIGURE_PAGE_HOLD,
    });
  });

  it("holds figure page across sequential explanation segments that lack figure IDs", () => {
    let currentHold: FigurePageHold | null = { pageNo: 3, remainingSegments: 4 };

    // Segment 11: explanation without figure ID, legacy pageNo is 1
    const seg11 = segment({ idx: 11, pageNo: 1, figureId: null });
    const res11 = updateFigureHold(currentHold, seg11, figFigures, true, true);
    expect(res11.pageNo).toBe(3); // does NOT snap back to 1
    expect(res11.hold?.remainingSegments).toBe(3);
    currentHold = res11.hold!;

    // Segment 12
    const seg12 = segment({ idx: 12, pageNo: 1, figureId: null });
    const res12 = updateFigureHold(currentHold, seg12, figFigures, true, true);
    expect(res12.pageNo).toBe(3);
    expect(res12.hold?.remainingSegments).toBe(2);
    currentHold = res12.hold!;

    // Segment 13
    const seg13 = segment({ idx: 13, pageNo: 1, figureId: null });
    const res13 = updateFigureHold(currentHold, seg13, figFigures, true, true);
    expect(res13.pageNo).toBe(3);
    expect(res13.hold?.remainingSegments).toBe(1);
    currentHold = res13.hold!;

    // Segment 14: final hold segment
    const seg14 = segment({ idx: 14, pageNo: 1, figureId: null });
    const res14 = updateFigureHold(currentHold, seg14, figFigures, true, true);
    expect(res14.pageNo).toBe(3);
    expect(res14.hold).toBeNull();
    currentHold = res14.hold;

    // Segment 15: hold has expired, yields cleanly back to natural page
    const seg15 = segment({ idx: 15, pageNo: 1, figureId: null });
    const res15 = updateFigureHold(currentHold, seg15, figFigures, true, true);
    expect(res15.pageNo).toBe(1);
    expect(res15.hold).toBeNull();
  });

  it("yields immediately when natural page advances past hold page", () => {
    const currentHold = { pageNo: 3, remainingSegments: 3 };
    const advancedSeg = segment({ idx: 12, pageNo: 5, figureId: null });
    const res = updateFigureHold(currentHold, advancedSeg, figFigures, true, true);
    expect(res.pageNo).toBe(5);
    expect(res.hold).toBeNull();
  });

  it("SEEK SAFETY: immediately resets hold and navigates backward when non-contiguous seek occurs", () => {
    const currentHold = { pageNo: 3, remainingSegments: 3 };
    // User scrubs backward to segment 2 on page 1
    const scrubbedSeg = segment({ idx: 2, pageNo: 1, figureId: null });
    const res = updateFigureHold(currentHold, scrubbedSeg, figFigures, false, false);
    expect(res.pageNo).toBe(1); // Navigates backward cleanly
    expect(res.hold).toBeNull(); // Hold is wiped
  });
});


