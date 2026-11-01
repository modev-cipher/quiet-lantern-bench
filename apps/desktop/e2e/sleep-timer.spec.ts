import { expect, test, type Page } from "@playwright/test";

import { audioState, enterReading, openReading, play } from "./readingHarness";

async function openCapsule(page: Page): Promise<void> {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openReading(page);
  await play(page);
  await enterReading(page);
  await expect(page.getByTestId("player-capsule")).toBeVisible();
  await expect
    .poll(async () => (await audioState(page)).duration)
    .toBeGreaterThan(8);
}

async function volume(page: Page): Promise<number> {
  return page.evaluate(() => document.querySelector("audio")?.volume ?? -1);
}

test("a minutes timer counts down while listening, fades the last seconds, then pauses and turns itself off", async ({
  page,
}) => {
  await page.clock.install();
  await openCapsule(page);
  await page.getByTestId("player-sleep").click();
  await expect(page.getByTestId("player-sleep-menu")).toBeVisible();
  await page.getByTestId("player-sleep-15").click();
  await expect(page.getByTestId("player-sleep")).toHaveAttribute(
    "data-state",
    "minutes",
  );
  await expect(page.getByTestId("player-sleep-remaining")).toHaveText(
    /^1[45]:\d\d$/,
  );

  await page.clock.fastForward("14:55");
  await expect(page.getByTestId("player-sleep-remaining")).toHaveText(
    /^0:0[0-5]$/,
  );
  await expect.poll(() => volume(page)).toBeLessThan(0.75);
  expect((await audioState(page)).paused).toBe(false);

  await page.clock.fastForward("00:08");
  await expect.poll(async () => (await audioState(page)).paused).toBe(true);
  await expect(page.getByTestId("player-sleep")).toHaveAttribute(
    "data-state",
    "off",
  );
  await expect.poll(() => volume(page)).toBe(1);
});

test("end of chapter pauses where the next chapter begins", async ({
  page,
}) => {
  await openCapsule(page);
  await page.getByTestId("player-prev-chapter").click();
  await expect
    .poll(async () => (await audioState(page)).currentTime)
    .toBeLessThan(1);
  await page.getByTestId("player-sleep").click();
  await page.getByTestId("player-sleep-chapter").click();
  await expect(page.getByTestId("player-sleep")).toHaveAttribute(
    "data-state",
    "chapterEnd",
  );
  await expect
    .poll(async () => (await audioState(page)).paused, { timeout: 10_000 })
    .toBe(true);
  const at = (await audioState(page)).currentTime;
  expect(at).toBeGreaterThan(3.2);
  expect(at).toBeLessThan(4.6);
  await expect(page.getByTestId("player-sleep")).toHaveAttribute(
    "data-state",
    "off",
  );
});

test("turning the timer off restores full volume and Escape closes only its menu", async ({
  page,
}) => {
  await openCapsule(page);
  await page.getByTestId("player-sleep").click();
  await page.getByTestId("player-sleep-chapter").click();
  await page.getByTestId("player-sleep").click();
  await page.getByTestId("player-sleep-off").click();
  await expect(page.getByTestId("player-sleep")).toHaveAttribute(
    "data-state",
    "off",
  );
  await expect.poll(() => volume(page)).toBe(1);
  await page.getByTestId("player-sleep").click();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("player-sleep-menu")).toHaveCount(0);
  await expect(page.getByTestId("reading-view")).toBeVisible();
});
