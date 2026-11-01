import { expect, test } from "@playwright/test";

import { e2eLocale } from "./i18nText";
import { audioState, openReading, play } from "./readingHarness";

const rtl = e2eLocale === "ar";

test("the strip's seek bar seeks from its visual start in either direction", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await expect
    .poll(async () => (await audioState(page)).duration)
    .toBeGreaterThan(8);
  await page.getByTestId("mini-player").getByTestId("player-toggle").click();
  const seek = page.getByTestId("mini-player").getByTestId("player-seek");
  const box = (await seek.boundingBox())!;
  const y = box.y + box.height / 2;
  const nearEnd = rtl ? box.x + box.width * 0.1 : box.x + box.width * 0.9;
  const nearStart = rtl ? box.x + box.width - 3 : box.x + 3;

  await page.mouse.click(nearEnd, y);
  await expect
    .poll(async () => (await audioState(page)).currentTime)
    .toBeGreaterThan(7);
  await page.mouse.click(nearStart, y);
  await expect
    .poll(async () => (await audioState(page)).currentTime)
    .toBeLessThan(1);

  await seek.focus();
  const forward = rtl ? "ArrowLeft" : "ArrowRight";
  await page.keyboard.press(forward);
  await expect
    .poll(async () => (await audioState(page)).currentTime)
    .toBeGreaterThan(3);
});

test("the strip's progress fill grows from the inline start", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await page.getByTestId("mini-player").getByTestId("player-toggle").click();
  const seek = page.getByTestId("mini-player").getByTestId("player-seek");
  const box = (await seek.boundingBox())!;
  await page.mouse.click(box.x + box.width * 0.5, box.y + box.height / 2);
  const fill = seek.getByTestId("player-seek-fill");
  const f = (await fill.boundingBox())!;
  if (rtl)
    expect(Math.abs(f.x + f.width - (box.x + box.width))).toBeLessThanOrEqual(
      1,
    );
  else expect(Math.abs(f.x - box.x)).toBeLessThanOrEqual(1);
});
