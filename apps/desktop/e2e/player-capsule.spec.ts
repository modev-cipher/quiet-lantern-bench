import { expect, test, type Page } from "@playwright/test";

import { e2eLocale } from "./i18nText";
import { audioState, enterReading, openReading, play } from "./readingHarness";

const rtl = e2eLocale === "ar";

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

test("the capsule names the chapter, counts down, and marks chapter starts on the scrubber", async ({
  page,
}) => {
  await openCapsule(page);
  const capsule = page.getByTestId("player-capsule");
  await expect(capsule.getByTestId("player-chapter")).toHaveAttribute(
    "data-chapter-idx",
    "0",
  );
  await expect(capsule.getByTestId("player-time")).toHaveAttribute(
    "data-scope",
    "chapter",
  );
  await expect(capsule.getByTestId("player-chapter-tick")).toHaveCount(0);
  await capsule.getByTestId("player-scope").click();
  await expect(capsule.getByTestId("player-time")).toHaveAttribute(
    "data-scope",
    "book",
  );
  await expect(capsule.getByTestId("player-chapter-tick")).toHaveCount(1);
});

test("the scrubber covers the current chapter, and the whole-book choice is remembered", async ({
  page,
}) => {
  await openCapsule(page);
  await page.getByTestId("player-toggle").click();
  const seek = page.getByTestId("player-seek");
  const box = (await seek.boundingBox())!;
  const late = rtl ? box.x + box.width * 0.1 : box.x + box.width * 0.9;
  await page.mouse.click(late, box.y + box.height / 2);
  const inChapter = (await audioState(page)).currentTime;
  expect(inChapter).toBeGreaterThan(2);
  expect(inChapter).toBeLessThan(3.5);
  await expect(page.getByTestId("player-chapter")).toHaveAttribute(
    "data-chapter-idx",
    "0",
  );

  await page.getByTestId("player-scope").click();
  await page.getByTestId("reading-close").click();
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await enterReading(page);
  await expect(page.getByTestId("player-time")).toHaveAttribute(
    "data-scope",
    "book",
  );
});

test("the chapters drawer lists each chapter with its length and jumps to it", async ({
  page,
}) => {
  await openCapsule(page);
  await page.getByTestId("player-chapter").click();
  const menu = page.getByTestId("player-chapters-menu");
  await expect(menu).toBeVisible();
  await expect(menu.getByRole("menuitemradio")).toHaveCount(2);
  await expect(menu.getByTestId("player-chapter-length")).toHaveCount(2);
  await expect(page.getByTestId("player-chapter-item-0")).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await page.getByTestId("player-chapter-item-1").click();
  await expect(menu).toHaveCount(0);
  await expect(page.getByTestId("player-chapter")).toHaveAttribute(
    "data-chapter-idx",
    "1",
  );
  await page.getByTestId("player-chapter").click();
  await page.keyboard.press("Escape");
  await expect(menu).toHaveCount(0);
  await expect(page.getByTestId("reading-view")).toBeVisible();
});

test("next and previous chapter jump between chapter starts", async ({
  page,
}) => {
  await openCapsule(page);
  await page.getByTestId("player-toggle").click();
  await page.getByTestId("player-next-chapter").click();
  await expect
    .poll(async () => (await audioState(page)).currentTime)
    .toBeGreaterThanOrEqual(3.4);
  await expect(page.getByTestId("player-chapter")).toHaveAttribute(
    "data-chapter-idx",
    "1",
  );
  await expect(page.getByTestId("player-next-chapter")).toBeDisabled();
  await page.getByTestId("player-prev-chapter").click();
  await expect
    .poll(async () => (await audioState(page)).currentTime)
    .toBeLessThan(1);
});

test("the speed menu sets an exact rate and Escape closes only the menu", async ({
  page,
}) => {
  await openCapsule(page);
  await page.getByTestId("player-rate").click();
  const menu = page.getByTestId("player-rate-menu");
  await expect(menu).toBeVisible();
  await expect(menu.getByRole("menuitemradio")).toHaveCount(6);
  await page.getByTestId("player-rate-1.75").click();
  await expect(menu).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(() => document.querySelector("audio")?.playbackRate),
    )
    .toBe(1.75);

  await page.getByTestId("player-rate").click();
  await expect(menu).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(menu).toHaveCount(0);
  await expect(page.getByTestId("reading-view")).toBeVisible();
});

test("the volume slider sets the element's volume", async ({ page }) => {
  await openCapsule(page);
  await page.getByTestId("player-volume").fill("40");
  await expect
    .poll(() => page.evaluate(() => document.querySelector("audio")?.volume))
    .toBeCloseTo(0.4, 2);
});

test("hovering the scrubber previews the time there, and a click at its start seeks to the start in either direction", async ({
  page,
}) => {
  await openCapsule(page);
  await page.getByTestId("player-toggle").click();
  await page.getByTestId("player-scope").click();
  const seek = page.getByTestId("player-seek");
  const box = (await seek.boundingBox())!;
  const late = rtl ? box.x + box.width * 0.1 : box.x + box.width * 0.9;
  await page.mouse.move(late, box.y + box.height / 2);
  await expect(page.getByTestId("player-hover-time")).toBeVisible();
  await page.mouse.click(late, box.y + box.height / 2);
  await expect
    .poll(async () => (await audioState(page)).currentTime)
    .toBeGreaterThan(7);
  const early = rtl ? box.x + box.width - 4 : box.x + 4;
  await page.mouse.click(early, box.y + box.height / 2);
  await expect
    .poll(async () => (await audioState(page)).currentTime)
    .toBeLessThan(1);
});

test("one scrub gesture keeps the chapter it started in, even past the chapter end", async ({
  page,
}) => {
  await openCapsule(page);
  await page.getByTestId("player-toggle").click();
  await page.evaluate(() => {
    const audio = document.querySelector("audio");
    if (audio != null) audio.currentTime = 0.5;
  });
  const box = (await page.getByTestId("player-seek").boundingBox())!;
  const y = box.y + box.height / 2;
  const start = rtl ? box.x + box.width - 4 : box.x + 4;
  const end = rtl ? box.x + 1 : box.x + box.width - 1;
  await page.mouse.move(start, y);
  await page.mouse.down();
  await page.mouse.move(end, y, { steps: 4 });
  await page.mouse.move(end + (rtl ? -2 : 2), y);
  await page.mouse.move(end, y);
  await page.mouse.up();
  const time = (await audioState(page)).currentTime;
  expect(time, "the drag stays inside chapter 1's range").toBeLessThan(4);
});

test("the chapters drawer takes keyboard focus, moves with the arrows and hands focus back", async ({
  page,
}) => {
  await openCapsule(page);
  await page.getByTestId("player-toggle").click();
  await page.getByTestId("reading-mode-toggle").click();
  const facsimile = page.getByTestId("reading-facsimile");
  await expect(facsimile).toHaveAttribute("data-page", "1");
  const trigger = page.getByTestId("player-chapter");
  await trigger.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("player-chapter-item-0")).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(page.getByTestId("player-chapter-item-1")).toBeFocused();
  await page.keyboard.press(rtl ? "ArrowLeft" : "ArrowRight");
  await expect(facsimile).toHaveAttribute("data-page", "1");
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("player-chapters-menu")).toHaveCount(0);
  await expect(trigger).toBeFocused();

  await page.keyboard.press("Enter");
  await page.keyboard.press("End");
  await expect(page.getByTestId("player-chapter-item-1")).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("player-chapters-menu")).toHaveCount(0);
  await expect(trigger).toBeFocused();
  await expect(trigger).toHaveAttribute("data-chapter-idx", "1");
});

test("the speed and sleep menus take keyboard focus, move with the arrows and hand it back", async ({
  page,
}) => {
  await openCapsule(page);
  const rate = page.getByTestId("player-rate");
  await rate.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("player-rate-1")).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(page.getByTestId("player-rate-1.25")).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("player-rate-menu")).toHaveCount(0);
  await expect(rate).toBeFocused();
  await expect
    .poll(() =>
      page.evaluate(() => document.querySelector("audio")?.playbackRate),
    )
    .toBe(1.25);

  const sleep = page.getByTestId("player-sleep");
  await sleep.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("player-sleep-off")).toBeFocused();
  await page.keyboard.press("End");
  await expect(page.getByTestId("player-sleep-chapter")).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("player-sleep-menu")).toHaveCount(0);
  await expect(sleep).toBeFocused();
  await expect(page.getByTestId("reading-view")).toBeVisible();
});
