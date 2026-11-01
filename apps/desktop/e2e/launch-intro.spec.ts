import { expect, test } from "@playwright/test";

import { fixtureBook, installTauriMock } from "./tauriMock";

test("the mark opens the app from the centre and settles on the bar's own mark", async ({
  page,
}) => {
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.goto("/");
  const intro = page.getByTestId("launch-intro");
  await expect(intro).toHaveCount(1);

  const start = await page.getByTestId("launch-intro-mark").boundingBox();
  const viewport = page.viewportSize()!;
  expect(
    Math.abs(start!.x + start!.width / 2 - viewport.width / 2),
    "the mark starts on the vertical centre line",
  ).toBeLessThan(viewport.width * 0.2);

  await expect(intro).toHaveCount(0);
  const settled = await page.getByTestId("brand-mark").boundingBox();
  const rtl = (await page.locator("html").getAttribute("dir")) === "rtl";
  const fromStart = rtl
    ? viewport.width - (settled!.x + settled!.width)
    : settled!.x;
  expect(
    fromStart,
    "the bar's mark sits at the reading-start corner",
  ).toBeLessThan(40);
  expect(settled!.y).toBeLessThan(40);
});

test("the intro never blocks the app: a click lands while it plays", async ({
  page,
}) => {
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.goto("/");
  await expect(page.getByTestId("launch-intro")).toHaveCount(1);
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("appearance-settings")).toBeVisible();
});

async function watchForIntro(page: import("@playwright/test").Page) {
  await page.addInitScript(() => {
    const seen = { intro: false };
    (window as unknown as Record<string, unknown>).__introSeen = seen;
    new MutationObserver(() => {
      if (document.querySelector('[data-testid="launch-intro"]') != null) {
        seen.intro = true;
      }
    }).observe(document, { childList: true, subtree: true });
  });
}

async function introWasSeen(page: import("@playwright/test").Page) {
  return page.evaluate(
    () =>
      (
        (window as unknown as Record<string, unknown>).__introSeen as {
          intro: boolean;
        }
      ).intro,
  );
}

test("with reduced motion the intro is never mounted", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await watchForIntro(page);
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.goto("/");
  await expect(page.getByTestId("library")).toBeVisible();
  await expect(page.getByTestId("brand-mark")).toBeVisible();
  expect(await introWasSeen(page)).toBe(false);
});

test("without reduced motion the same probe does see it", async ({ page }) => {
  await watchForIntro(page);
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.goto("/");
  await expect(page.getByTestId("library")).toBeVisible();
  await expect(page.getByTestId("launch-intro")).toHaveCount(0);
  expect(await introWasSeen(page)).toBe(true);
});

test.describe("right to left", () => {
  test("the mark settles at the reading-start corner, which is the right", async ({
    page,
  }) => {
    await installTauriMock(page, { books: [fixtureBook()] });
    await page.addInitScript(() => {
      localStorage.setItem("txtcaster.direction", "rtl");
    });
    await page.goto("/");
    await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
    await expect(page.getByTestId("launch-intro")).toHaveCount(0, {
      timeout: 5_000,
    });
    const settled = await page.getByTestId("brand-mark").boundingBox();
    const viewport = page.viewportSize()!;
    expect(viewport.width - (settled!.x + settled!.width)).toBeLessThan(40);
  });
});
