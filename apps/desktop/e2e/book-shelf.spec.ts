import { expect, test, type Page } from "@playwright/test";

import { e2eLocale } from "./i18nText";
import { wavDataUrl } from "./pcmFixture";
import { fixtureBook, installTauriMock } from "./tauriMock";

const rtl = e2eLocale === "ar";

const seeded = {
  id: "export-seeded",
  bookId: "book-listening",
  jobId: "job-1",
  path: "/exports/listening.mp3",
  bytes: 5_000_000,
  format: "mp3",
  mode: "narrate",
  stale: false,
  createdAt: 1_755_000_000_000,
};

const books = [
  fixtureBook({ id: "book-listening", title: "Listening Now", pageCount: 264 }),
  fixtureBook({ id: "book-thin", title: "A Short Essay", pageCount: 40 }),
  fixtureBook({ id: "book-thick", title: "A Very Long Book", pageCount: 900 }),
  fixtureBook({
    id: "book-arabic",
    title: "كتاب القراءة الهادئة",
    pageCount: 190,
  }),
];

async function boot(page: Page): Promise<void> {
  await page.setViewportSize({ width: 1280, height: 860 });
  await installTauriMock(page, {
    books,
    seededExports: [seeded],
    readingScenario: { mediaDataUrl: wavDataUrl() },
  });
  await page.addInitScript(() => {
    localStorage.setItem(
      "txtcaster.listened",
      JSON.stringify({ "export-seeded": 0.3 }),
    );
  });
  await page.goto("/");
  await expect(page.getByTestId("book-grid")).toBeVisible();
}

test("books stand on two shelves: the one being listened to is In progress", async ({
  page,
}) => {
  await boot(page);
  const inProgress = page.getByTestId("shelf-in-progress");
  const onShelf = page.getByTestId("shelf-on-shelf");
  await expect(inProgress.getByTestId("book-card")).toHaveCount(1);
  await expect(
    inProgress.getByTestId("open-book-book-listening"),
  ).toBeVisible();
  await expect(onShelf.getByTestId("book-card")).toHaveCount(3);
  await expect(page.getByTestId("shelf-ribbon-book-listening")).toBeVisible();
  await expect(page.locator('[data-testid^="shelf-ribbon-"]')).toHaveCount(1);
});

test("a spine is as thick as its book, and a right-to-left book is bound the other way", async ({
  page,
}) => {
  await boot(page);
  const spine = (id: string) =>
    page
      .getByTestId(`open-book-${id}`)
      .locator(".shelf-book")
      .getAttribute("data-spine");
  const thin = Number(await spine("book-thin"));
  const thick = Number(await spine("book-thick"));
  expect(thin).toBe(10);
  expect(thick).toBeGreaterThan(thin * 3);
  await expect(
    page.getByTestId("open-book-book-arabic").locator(".shelf-book"),
  ).toHaveAttribute("dir", "rtl");
  await expect(
    page.getByTestId("open-book-book-thin").locator(".shelf-book"),
  ).toHaveAttribute("dir", "ltr");
});

test("pointing at a book reveals its actions; keyboard focus does the same", async ({
  page,
}) => {
  await boot(page);
  const card = page
    .getByTestId("book-card")
    .filter({ has: page.getByTestId("open-book-book-thin") });
  const cast = card.locator('[data-line="actions"]');
  await expect(card.getByTestId("create-audio-book-thin")).toBeAttached();
  await expect(cast).toHaveCSS("opacity", "0");
  await page.getByTestId("open-book-book-thin").hover();
  await expect(cast).toHaveCSS("opacity", "1");

  await page.mouse.move(2, 2);
  await expect(cast).toHaveCSS("opacity", "0");
  await page.getByTestId("open-book-book-thin").focus();
  await expect(cast).toHaveCSS("opacity", "1");
});

test("the book menu floats above the shelf instead of being clipped by the book", async ({
  page,
}) => {
  await boot(page);
  await page.getByTestId("open-book-book-thick").hover();
  await page.getByTestId("book-menu-book-thick").click();
  const menu = page.getByTestId("book-menu-items-book-thick");
  await expect(menu).toBeVisible();
  const parent = await menu.evaluate((el) => el.parentElement?.tagName);
  expect(parent, "the menu is portaled out of the card").toBe("BODY");
  const box = (await menu.boundingBox())!;
  const viewport = page.viewportSize()!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(viewport.width);
  expect(box.y + box.height).toBeLessThanOrEqual(viewport.height);
  await expect(menu.getByRole("menuitem")).toHaveCount(3);

  await page.getByTestId("delete-book-book-thick").click();
  await expect(menu).toHaveCount(0);
  await expect(
    page.getByTestId("confirm-delete-book-book-thick"),
  ).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("confirm-delete-book-book-thick")).toHaveCount(
    0,
  );
});

test("the arrow keys walk the shelf in reading order", async ({ page }) => {
  await boot(page);
  const shelf = page.getByTestId("shelf-on-shelf");
  const first = shelf.locator("[data-shelf-book]").nth(0);
  const second = shelf.locator("[data-shelf-book]").nth(1);
  await first.focus();
  await page.keyboard.press(rtl ? "ArrowLeft" : "ArrowRight");
  await expect(second).toBeFocused();
  await page.keyboard.press(rtl ? "ArrowRight" : "ArrowLeft");
  await expect(first).toBeFocused();
  await page.keyboard.press("End");
  await expect(shelf.locator("[data-shelf-book]").last()).toBeFocused();
});

test("on launch the books settle only after the intro has revealed the app", async ({
  page,
}) => {
  await boot(page);
  const delay = await page
    .getByTestId("book-grid")
    .evaluate((el) => getComputedStyle(el).getPropertyValue("--intro-delay"));
  expect(Number.parseInt(delay, 10)).toBeGreaterThan(500);
});

test("with reduced motion the books keep their angle and never turn", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await boot(page);
  const book = page.getByTestId("open-book-book-thin").locator(".shelf-book");
  const rest = await book.evaluate((el) => getComputedStyle(el).transform);
  await page.getByTestId("open-book-book-thin").hover();
  const hovered = await book.evaluate((el) => getComputedStyle(el).transform);
  expect(hovered).toBe(rest);
  await expect(page.getByTestId("launch-intro")).toHaveCount(0);
});

test("Tab out of a book menu continues from the menu button in both directions", async ({
  page,
}) => {
  await boot(page);
  const trigger = page.getByTestId("book-menu-book-thin");
  await trigger.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("book-menu-items-book-thin")).toBeVisible();
  await page.keyboard.press("Tab");
  await expect(page.getByTestId("book-menu-items-book-thin")).toHaveCount(0);
  await expect(page.getByTestId("open-book-book-thick")).toBeFocused();

  await trigger.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("book-menu-items-book-thin")).toBeVisible();
  await page.keyboard.press("Shift+Tab");
  await expect(page.getByTestId("book-menu-items-book-thin")).toHaveCount(0);
  await expect(page.getByTestId("talk-book-book-thin")).toBeFocused();
});

test("cancelling a removal hands focus back to the menu that started it", async ({
  page,
}) => {
  await boot(page);
  const trigger = page.getByTestId("book-menu-book-thin");
  for (const cancel of ["Escape", "Keep"] as const) {
    await trigger.focus();
    await page.keyboard.press("Enter");
    await page.getByTestId("delete-book-book-thin").focus();
    await page.keyboard.press("Enter");
    await expect(
      page.getByTestId("confirm-delete-book-book-thin"),
    ).toBeFocused();
    if (cancel === "Escape") await page.keyboard.press("Escape");
    else await page.getByTestId("cancel-delete-book-book-thin").click();
    await expect(page.getByTestId("confirm-delete-book-book-thin")).toHaveCount(
      0,
    );
    await expect(trigger).toBeFocused();
  }
});
