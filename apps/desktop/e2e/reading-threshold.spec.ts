import type { Page } from "@playwright/test";
import { expect, test } from "@playwright/test";

import { enterReading, openReading, play } from "./readingHarness";

const CLONE = '[data-testid="reading-threshold"]';

declare global {
  interface Window {
    __thresholdSeen?: number;
  }
}

async function watchForClone(page: Page): Promise<void> {
  await page.evaluate((selector) => {
    window.__thresholdSeen = 0;
    const observer = new MutationObserver(() => {
      const found = document.querySelectorAll(selector).length;
      if (found > 0) window.__thresholdSeen = (window.__thresholdSeen ?? 0) + 1;
    });
    observer.observe(document.body, { childList: true, subtree: true });
  }, CLONE);
}

async function timesSeen(page: Page): Promise<number> {
  return page.evaluate(() => window.__thresholdSeen ?? 0);
}

test("the cover you touched grows into the reading surface, then gets out of the way", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await watchForClone(page);
  await enterReading(page);

  expect(
    await timesSeen(page),
    "the threshold is the shared element that ties the cover you pressed to the page you are now reading",
  ).toBeGreaterThan(0);

  const clone = page.locator(CLONE);
  if ((await clone.count()) > 0) {
    await expect(
      clone,
      "a decorative clone must never intercept a tap meant for the printed word",
    ).toHaveCSS("pointer-events", "none");
    await expect(clone).toHaveAttribute("aria-hidden", "true");
  }

  await expect(
    clone,
    "the threshold is a gesture, not a layer; it has to leave",
  ).toHaveCount(0, { timeout: 3000 });
});

test.describe("a reader who asked for stillness gets stillness", () => {
  test("no clone is ever mounted under prefers-reduced-motion", async ({
    page,
  }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    await openReading(page);
    await play(page);
    await watchForClone(page);
    await enterReading(page);
    await page.waitForTimeout(600);

    expect(
      await timesSeen(page),
      "reduced motion is a request, not a preference to soften; a clone that appears and then tidies itself away still moved",
    ).toBe(0);

    await expect(page.getByTestId("reading-surface")).toBeVisible();
  });
});
