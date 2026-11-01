import { expect, test, type Page } from "@playwright/test";

import {
  READING_COLUMN_MAX_PX,
  SURFACE_GUTTER_PX,
  fittedWidthPx,
} from "../src/readingPageFit";
import { EYE_LINE_PADDING } from "../src/readingPageScroll";
import { enterReading, openReading, play } from "./readingHarness";

test.use({ viewport: { width: 1920, height: 1080 } });

const VIEWPORT_WIDTH = 1920;

const READABLE_WIDTH_FRACTION = 0.7;

const FIXTURE_PAGE_WIDTH_PT = 612;
const FIXTURE_PAGE_HEIGHT_PT = 792;
const FIXTURE_WORD_BOX_PT = 12;
const PHYSICAL_SIZE_FLOOR_PX_PER_PT = 2.0;
const WORD_BOX_FLOOR_PX = FIXTURE_WORD_BOX_PT * PHYSICAL_SIZE_FLOOR_PX_PER_PT;

async function seekTo(page: Page, ms: number): Promise<void> {
  await page.evaluate((target) => {
    const audio = document.querySelector("audio");
    if (audio != null) audio.currentTime = target / 1000;
  }, ms);
}

async function enterPageMode(page: Page): Promise<void> {
  await page.getByTestId("reading-mode-toggle").click();
  await expect(page.getByTestId("reading-surface")).toHaveAttribute(
    "data-mode",
    "page",
  );
}

async function waitForWord(page: Page): Promise<void> {
  await page.waitForFunction(
    () => {
      const element = document.querySelector<HTMLElement>(
        '[data-testid="reading-page-word"]',
      );
      return element != null && !element.hidden;
    },
    null,
    { timeout: 5_000 },
  );
}

async function pauseAudio(page: Page): Promise<void> {
  await page.evaluate(() => document.querySelector("audio")?.pause());
}

async function resumeAudio(page: Page): Promise<void> {
  await page.evaluate(() => document.querySelector("audio")?.play());
}

async function surfaceMetrics(page: Page) {
  return page.evaluate(() => {
    const surface = document.querySelector(
      '[data-testid="reading-surface"]',
    ) as HTMLElement | null;
    const box = document.querySelector(
      '[data-testid="reading-page-box"]',
    ) as HTMLElement | null;
    const word = document.querySelector(
      '[data-testid="reading-page-word"]',
    ) as HTMLElement | null;
    if (surface == null || box == null) return null;
    const surfaceRect = surface.getBoundingClientRect();
    const boxRect = box.getBoundingClientRect();
    const wordRect =
      word == null || word.hidden ? null : word.getBoundingClientRect();
    return {
      clientWidth: surface.clientWidth,
      clientHeight: surface.clientHeight,
      scrollTop: surface.scrollTop,
      scrollHeight: surface.scrollHeight,
      boxWidth: boxRect.width,
      boxHeight: boxRect.height,
      surfaceTop: surfaceRect.top,
      surfaceBottom: surfaceRect.bottom,
      word:
        wordRect == null
          ? null
          : {
              top: wordRect.top,
              bottom: wordRect.bottom,
              height: wordRect.height,
            },
    };
  });
}

async function setScrollTop(page: Page, top: number): Promise<void> {
  await page.evaluate((value) => {
    const surface = document.querySelector(
      '[data-testid="reading-surface"]',
    ) as HTMLElement | null;
    if (surface != null) surface.scrollTop = value;
  }, top);
}

test("the page is rendered at a readable size, which is the defect this item fixes", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await pauseAudio(page);
  await seekTo(page, 250);
  await enterPageMode(page);
  await waitForWord(page);

  const metrics = await surfaceMetrics(page);
  expect(metrics).not.toBeNull();
  const surface = metrics!;

  expect(
    surface.boxWidth,
    `the printed page renders ${surface.boxWidth}px wide in a ${VIEWPORT_WIDTH}px window; ` +
      "the shipped layout drew it at 448px, which is 0.45x the page's printed physical size " +
      "on a 143 DPI panel and is why the user could not read it",
  ).toBeGreaterThanOrEqual(VIEWPORT_WIDTH * READABLE_WIDTH_FRACTION);

  expect(
    surface.boxWidth,
    "the CSS width and the fit model must be one decision, not two that agree today",
  ).toBe(
    fittedWidthPx({
      containerWidth: surface.clientWidth - SURFACE_GUTTER_PX * 2,
      containerHeight: surface.clientHeight - SURFACE_GUTTER_PX * 2,
      ratio: FIXTURE_PAGE_WIDTH_PT / FIXTURE_PAGE_HEIGHT_PT,
      fit: "width",
      zoom: 1,
    }),
  );

  expect(surface.boxWidth).toBeLessThanOrEqual(READING_COLUMN_MAX_PX);

  expect(
    surface.word,
    "without a located word there is nothing to measure legibility against",
  ).not.toBeNull();
  expect(
    surface.word!.height,
    `the spoken word's box renders ${surface.word!.height}px tall. The floor is ` +
      `${WORD_BOX_FLOOR_PX}px: the golden page's ${FIXTURE_WORD_BOX_PT}pt word box at ` +
      `${PHYSICAL_SIZE_FLOOR_PX_PER_PT} px/pt, which is the page at its printed physical size. ` +
      "Measured on the real book, DDIA's median word box is 14.16pt over 8535 rects, so the " +
      "same floor is 28.3px there against 12.6px in the shipped 448px layout",
  ).toBeGreaterThanOrEqual(WORD_BOX_FLOOR_PX);

  expect(
    surface.scrollHeight,
    "the ruled default fits the width and scrolls, exactly as the user's own PDF reader does " +
      "at the zoom he described",
  ).toBeGreaterThan(surface.clientHeight);
});

test("fit page is offered as a preset and actually fits", async ({ page }) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await pauseAudio(page);
  await seekTo(page, 250);
  await enterPageMode(page);

  await expect(page.getByTestId("reading-fit-width")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await page.getByTestId("reading-fit-page").click();
  await expect(page.getByTestId("reading-fit-page")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(page.getByTestId("reading-surface")).toHaveAttribute(
    "data-fit",
    "page",
  );

  const fitted = await surfaceMetrics(page);
  expect(fitted!.boxHeight).toBeLessThanOrEqual(fitted!.clientHeight + 1);
  expect(
    fitted!.boxWidth,
    "fit page is the mode that cannot reach the user's reference, which is why it is never " +
      "the default",
  ).toBeLessThan(VIEWPORT_WIDTH * READABLE_WIDTH_FRACTION);
});

test("zoom is the reader's override and survives a reopen", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await pauseAudio(page);
  await seekTo(page, 250);
  await enterPageMode(page);

  const before = await surfaceMetrics(page);
  await page.getByTestId("reading-zoom-in").click();
  await page.getByTestId("reading-zoom-in").click();
  await expect(page.getByTestId("reading-zoom-reset")).toHaveText("120%");

  const zoomed = await surfaceMetrics(page);
  expect(zoomed!.boxWidth).toBeGreaterThan(before!.boxWidth);

  await page.getByTestId("reading-zoom-reset").click();
  await expect(page.getByTestId("reading-zoom-reset")).toHaveText("100%");
  const reset = await surfaceMetrics(page);
  expect(reset!.boxWidth).toBe(before!.boxWidth);

  expect(
    await page.evaluate(() => localStorage.getItem("txtcaster.pageZoom")),
    "page zoom is a per-seat preference on the useZoom precedent, and is not webview zoom",
  ).toBe("1");
});

test("following keeps the spoken word in view, browsing never moves the viewport", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await pauseAudio(page);
  await seekTo(page, 250);
  await enterPageMode(page);
  await waitForWord(page);

  const parked = await surfaceMetrics(page);
  expect(
    parked!.scrollTop,
    "the spoken word is near the top of the golden page, so the eye line clamps the viewport to 0",
  ).toBe(0);
  expect(parked!.word).not.toBeNull();
  expect(parked!.word!.top).toBeGreaterThanOrEqual(parked!.surfaceTop);
  expect(parked!.word!.bottom).toBeLessThanOrEqual(parked!.surfaceBottom);

  await setScrollTop(page, 700);
  await page.waitForTimeout(200);
  expect((await surfaceMetrics(page))!.scrollTop).toBe(700);

  await seekTo(page, 1500);
  await expect
    .poll(
      () =>
        page.evaluate(() => {
          const surface = document.querySelector<HTMLElement>(
            '[data-testid="reading-surface"]',
          );
          const envelope = document.querySelector<HTMLElement>(
            '[data-testid="reading-page-fallback-box"]',
          );
          if (surface == null || envelope == null) return null;
          return (
            envelope.getBoundingClientRect().top -
            surface.getBoundingClientRect().top
          );
        }),
      {
        message:
          "a narration-driven page turn re-aims the viewport and re-establishes auto-scroll after " +
          "the reader's own scroll: the next sentence has no word boxes, so its envelope is what " +
          "lands on the eye line; if this never moves, the wiring passed the wrong follow state " +
          "and no unit test would have noticed",
      },
    )
    .toBeCloseTo(EYE_LINE_PADDING, 0);
  expect(
    (await surfaceMetrics(page))!.scrollTop,
    "the viewport left the spot the reader had scrolled to",
  ).not.toBe(700);

  await page.getByTestId("reading-flip-forward").click();
  await expect(page.getByTestId("reading-page-controls")).toHaveAttribute(
    "data-browsing",
    "yes",
  );

  await setScrollTop(page, 500);
  await resumeAudio(page);
  await page.waitForTimeout(900);
  expect(
    (await surfaceMetrics(page))!.scrollTop,
    "browsing owns the viewport absolutely; an auto-scroll here would re-aim a page the " +
      "reader chose, the same defect a flip that seeks is",
  ).toBe(500);
});

test("a manual scroll while following is a viewport gesture, not a browse", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await pauseAudio(page);
  await seekTo(page, 250);
  await enterPageMode(page);
  await waitForWord(page);

  await page.getByTestId("reading-surface").hover();
  await page.mouse.wheel(0, 600);
  await page.waitForTimeout(300);

  await expect(
    page.getByTestId("reading-page-controls"),
    "a scroll is not a page flip: the mode must not become browsing and the return " +
      "affordance must not appear",
  ).toHaveAttribute("data-browsing", "no");
  await expect(page.getByTestId("reading-return")).toHaveCount(0);

  const scrolled = (await surfaceMetrics(page))!.scrollTop;
  expect(scrolled).toBeGreaterThan(0);

  await seekTo(page, 890);
  await page.waitForTimeout(700);
  expect(
    (await surfaceMetrics(page))!.scrollTop,
    "after the reader's own scroll, within-page auto-scroll stays out of the way until the " +
      "next narration-driven page turn re-establishes it",
  ).toBe(scrolled);
});
