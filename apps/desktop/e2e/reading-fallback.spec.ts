import { expect, test } from "@playwright/test";

import golden from "./fixtures/reading-timeline.golden.json" with { type: "json" };
import { enterReading, openReading, play } from "./readingHarness";

const stale = golden.chapters[0].segments[1];
const missing = golden.chapters[1].segments[1];

async function seekTo(page: import("@playwright/test").Page, ms: number) {
  await page.evaluate((target) => {
    const audio = document.querySelector("audio");
    if (audio != null) audio.currentTime = target / 1000;
  }, ms);
}

async function fillValue(
  page: import("@playwright/test").Page,
): Promise<number> {
  return page.evaluate(() => {
    const underline = document.querySelector<HTMLElement>(
      '[data-testid="reading-underline"]',
    );
    return Number(underline?.style.getPropertyValue("--reading-fill") ?? "-1");
  });
}

test("a stale sentence fades, says why, and offers the repair surface", async ({
  page,
}) => {
  expect(stale.provenance).toBe("stale");
  await openReading(page);
  await play(page);
  await enterReading(page);
  await seekTo(page, stale.startMs + 100);

  const text = page.getByTestId("reading-fallback-text");
  await expect(text).toHaveAttribute("data-provenance", "stale", {
    timeout: 5_000,
  });
  await expect(text).toContainText(stale.text);
  await expect(page.getByTestId("reading-fallback-notice")).toBeVisible();
  await expect(page.getByTestId("reading-fallback-repair")).toBeVisible();
  await expect(page.locator("[data-word]")).toHaveCount(0);
});

test("a missing sentence fades with different words and no repair", async ({
  page,
}) => {
  expect(missing.provenance).toBe("missing");
  await openReading(page);
  await play(page);
  await enterReading(page);
  await seekTo(page, missing.startMs + 100);

  const text = page.getByTestId("reading-fallback-text");
  await expect(text).toHaveAttribute("data-provenance", "missing", {
    timeout: 5_000,
  });
  const notice = await page
    .getByTestId("reading-fallback-notice")
    .textContent();

  await expect(
    page.getByTestId("reading-fallback-repair"),
    "there is nothing to re-record for a segment that was never aligned",
  ).toHaveCount(0);

  await seekTo(page, stale.startMs + 100);
  await expect(page.getByTestId("reading-fallback-repair")).toBeVisible({
    timeout: 5_000,
  });
  const staleNotice = await page
    .getByTestId("reading-fallback-notice")
    .textContent();

  expect(
    staleNotice,
    "stale and missing are different states and must not share one sentence",
  ).not.toBe(notice);
});

test("the underline advances on a wordless sentence too", async ({ page }) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await seekTo(page, stale.startMs + 50);

  await expect(page.getByTestId("reading-fallback-text")).toBeVisible({
    timeout: 5_000,
  });
  await expect(page.getByTestId("reading-underline")).toBeVisible();

  const first = await fillValue(page);
  await page.waitForFunction(
    (previous) => {
      const underline = document.querySelector<HTMLElement>(
        '[data-testid="reading-underline"]',
      );
      const value = Number(
        underline?.style.getPropertyValue("--reading-fill") ?? "-1",
      );
      return value > previous;
    },
    first,
    { timeout: 5_000 },
  );
  const second = await fillValue(page);

  expect(first).toBeGreaterThanOrEqual(0);
  expect(
    second,
    "the fill is the only progress cue a wordless sentence has",
  ).toBeGreaterThan(first);
});

test("the repair affordance leaves the overlay for the review surface", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await seekTo(page, stale.startMs + 100);
  await page.getByTestId("reading-fallback-repair").click();

  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await expect(page.getByTestId("job-row-job-1")).toBeVisible({
    timeout: 5_000,
  });
});
