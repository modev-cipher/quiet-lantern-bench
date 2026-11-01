import { expect, test, type Page } from "@playwright/test";

import { e2eLocale } from "./i18nText";
import { enterReading, openReading, play } from "./readingHarness";

const VIEW = { width: 1440, height: 900 };
const rtl = e2eLocale === "ar";

async function openReader(page: Page): Promise<void> {
  await page.setViewportSize(VIEW);
  await openReading(page);
  await play(page);
  await enterReading(page);
  await expect(page.getByTestId("study-companion")).toBeVisible();
}

async function position(page: Page): Promise<string> {
  return page
    .getByTestId("study-companion")
    .evaluate((el) => getComputedStyle(el).position);
}

test("the tutor pops out into a floating window that moves, folds, remembers itself and docks back", async ({
  page,
}) => {
  await openReader(page);
  const companion = page.getByTestId("study-companion");
  const surfaceBefore = (await page
    .getByTestId("reading-surface")
    .boundingBox())!;

  await page.getByTestId("companion-float").click();
  await expect.poll(() => position(page)).toBe("fixed");
  const floated = (await companion.boundingBox())!;
  expect(floated.x).toBeGreaterThanOrEqual(0);
  expect(floated.x + floated.width).toBeLessThanOrEqual(VIEW.width);
  expect(floated.y + floated.height).toBeLessThanOrEqual(VIEW.height);
  if (rtl) expect(floated.x).toBeLessThan(VIEW.width / 2);
  else expect(floated.x).toBeGreaterThan(VIEW.width / 2);
  const surfaceAfter = (await page
    .getByTestId("reading-surface")
    .boundingBox())!;
  expect(
    surfaceAfter.width,
    "the reading column takes the space the docked rail gave up",
  ).toBeGreaterThan(surfaceBefore.width + 100);
  await expect(page.getByTestId("companion-resize-handle")).toHaveCount(0);

  const grip = (await page.getByTestId("companion-drag").boundingBox())!;
  const from = { x: grip.x + grip.width / 2, y: grip.y + grip.height / 2 };
  const dx = rtl ? 300 : -300;
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(from.x + dx, from.y + 120, { steps: 6 });
  await page.mouse.up();
  const moved = (await companion.boundingBox())!;
  expect(Math.abs(moved.x - (floated.x + dx))).toBeLessThanOrEqual(2);
  expect(Math.abs(moved.y - (floated.y + 120))).toBeLessThanOrEqual(2);

  await page.mouse.move(from.x + dx, from.y + 120);
  await page.mouse.down();
  await page.mouse.move(-500, -500, { steps: 4 });
  await page.mouse.up();
  const pinned = (await companion.boundingBox())!;
  expect(
    pinned.x,
    "a window flung off-screen stays reachable",
  ).toBeGreaterThanOrEqual(0);
  expect(pinned.y).toBeGreaterThanOrEqual(0);

  await page.getByTestId("companion-fold").click();
  await expect(companion).toBeHidden();
  const pill = page.getByTestId("companion-pill");
  await expect(pill).toBeVisible();
  await pill.click();
  await expect(companion).toBeVisible();
  await expect(page.getByTestId("voice-session-panel")).toBeVisible();

  await page.keyboard.press("Escape");
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await enterReading(page);
  await expect.poll(() => position(page)).toBe("fixed");
  const reopened = (await companion.boundingBox())!;
  expect(Math.abs(reopened.x - pinned.x)).toBeLessThanOrEqual(1);
  expect(Math.abs(reopened.y - pinned.y)).toBeLessThanOrEqual(1);

  await page.getByTestId("companion-dock").click();
  await expect.poll(() => position(page)).not.toBe("fixed");
  await expect(page.getByTestId("companion-resize-handle")).toBeVisible();
  const docked = (await companion.boundingBox())!;
  if (rtl) expect(docked.x).toBeLessThanOrEqual(1);
  else expect(docked.x + docked.width).toBeGreaterThanOrEqual(VIEW.width - 1);
});

test("a folded tutor stays folded across a reopen and its pill shows a live session", async ({
  page,
}) => {
  await openReader(page);
  await page.getByTestId("companion-float").click();
  await page.getByTestId("companion-fold").click();
  await page.keyboard.press("Escape");
  await enterReading(page);
  await expect(page.getByTestId("companion-pill")).toBeVisible();
  await expect(page.getByTestId("study-companion")).toBeHidden();
  await expect(page.getByTestId("companion-pill-live")).toHaveCount(0);
});
