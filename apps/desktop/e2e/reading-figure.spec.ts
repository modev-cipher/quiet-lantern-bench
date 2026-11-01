import { expect, test } from "@playwright/test";

import golden from "./fixtures/reading-timeline.golden.json" with { type: "json" };
import { enterReading, openReading, play } from "./readingHarness";

async function seekTo(page: import("@playwright/test").Page, ms: number) {
  await page.evaluate((target) => {
    const audio = document.querySelector("audio");
    if (audio != null) audio.currentTime = target / 1000;
  }, ms);
}

test("framing shows the figure's own page, not the anchoring segment's", async ({
  page,
}) => {
  const cue = golden.chapters[1].segments[0];
  const figure = golden.figures.find(
    (entry) => entry.figureId === cue.figureId,
  );
  expect(
    cue.pageNo,
    "the fixture must not frame a figure on its own segment's page",
  ).not.toBe(figure?.pageNo);

  await openReading(page);
  await play(page);
  await enterReading(page);
  await seekTo(page, cue.startMs + 200);

  const facsimile = page.getByTestId("reading-facsimile");
  await expect(facsimile).toHaveAttribute("data-figure", "figure-7", {
    timeout: 5_000,
  });
  await expect(facsimile).toHaveAttribute("data-page", String(figure?.pageNo));
  await expect(facsimile).toHaveAttribute("data-zoomed", "yes");
  await expect(page.getByTestId("reading-figure-frame")).toBeVisible();

  const transform = await page
    .getByTestId("reading-facsimile-image")
    .evaluate((element) => getComputedStyle(element).transform);
  expect(transform, "a framed figure is zoomed by a transform").not.toBe(
    "none",
  );
});

test("a plain text segment that anchors a figure is framed the same way", async ({
  page,
}) => {
  const text = golden.chapters[1].segments[1];
  const figure = golden.figures.find(
    (entry) => entry.figureId === text.figureId,
  );
  expect(text.kind, "this is the teacher-mode case the ruling is about").toBe(
    "text",
  );
  expect(figure?.pageNo).not.toBe(text.pageNo);

  await openReading(page);
  await play(page);
  await enterReading(page);
  await seekTo(page, text.startMs + 200);

  const facsimile = page.getByTestId("reading-facsimile");
  await expect(facsimile).toHaveAttribute("data-figure", "figure-9", {
    timeout: 5_000,
  });
  await expect(facsimile).toHaveAttribute("data-page", String(figure?.pageNo));
});

test("a segment with no figure frames nothing", async ({ page }) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await seekTo(page, 200);

  const facsimile = page.getByTestId("reading-facsimile");
  await expect(facsimile).toHaveAttribute("data-figure", "", {
    timeout: 5_000,
  });
  await expect(facsimile).toHaveAttribute("data-zoomed", "no");
  await expect(page.getByTestId("reading-figure-frame")).toHaveCount(0);
});
