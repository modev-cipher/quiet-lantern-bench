import { expect, test, type Locator, type Page } from "@playwright/test";

import { fixtureBook, installTauriMock } from "./tauriMock";

async function boot(page: Page): Promise<void> {
  await page.setViewportSize({ width: 1440, height: 900 });
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.goto("/");
}

async function expectThumbOn(group: Locator, option: Locator): Promise<void> {
  const thumb = group.getByTestId("glide-thumb");
  await expect
    .poll(async () => {
      const a = (await thumb.boundingBox())!;
      const b = (await option.boundingBox())!;
      return Math.max(
        Math.abs(a.x - b.x),
        Math.abs(a.y - b.y),
        Math.abs(a.width - b.width),
        Math.abs(a.height - b.height),
      );
    })
    .toBeLessThanOrEqual(1);
}

test("the theme thumb glides onto each choice in either direction", async ({
  page,
}) => {
  await boot(page);
  await page.getByTestId("nav-settings").click();
  const dark = page.getByTestId("theme-choice-dark");
  const group = page.locator('[role="radiogroup"]').filter({ has: dark });
  await expectThumbOn(group, page.getByTestId("theme-choice-system"));
  await dark.click();
  await expectThumbOn(group, dark);
  await page.getByTestId("theme-choice-light").click();
  await expectThumbOn(group, page.getByTestId("theme-choice-light"));
});

test("the Library tab underline follows the chosen tab", async ({ page }) => {
  await boot(page);
  await page.getByTestId("nav-library").click();
  const books = page.getByTestId("zone-books");
  const audio = page.getByTestId("nav-audio");
  const group = page.locator('[role="tablist"]').filter({ has: audio });
  await expectThumbOn(group, books);
  await audio.click();
  await expectThumbOn(group, audio);
  await books.click();
  await expectThumbOn(group, books);
});

test("flipping the interface direction keeps the theme thumb on its choice", async ({
  page,
}) => {
  await boot(page);
  await page.getByTestId("nav-settings").click();
  const dark = page.getByTestId("theme-choice-dark");
  const group = page.locator('[role="radiogroup"]').filter({ has: dark });
  await dark.click();
  await expectThumbOn(group, dark);
  for (const direction of ["rtl", "ltr", "rtl"]) {
    await page.getByTestId(`direction-choice-${direction}`).click();
    await expect(page.locator("html")).toHaveAttribute("dir", direction);
    await expectThumbOn(group, dark);
  }
});
