import { expect, test } from "@playwright/test";

import { msg } from "./i18nText";
import { fixtureBook, installTauriMock } from "./tauriMock";

test("on/off settings are switches that flip and persist, and hints are always shown without moving anything", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();

  const spotlight = page.getByTestId("spotlight-switch");
  await expect(spotlight).toHaveAttribute("role", "switch");
  const before = await spotlight.getAttribute("aria-checked");
  await spotlight.click();
  const after = before === "true" ? "false" : "true";
  await expect(spotlight).toHaveAttribute("aria-checked", after);
  await page.reload();
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("spotlight-switch")).toHaveAttribute(
    "aria-checked",
    after,
  );

  const row = page
    .getByTestId("theme-choice-dark")
    .locator("xpath=ancestor::*[contains(@class,'settings-row')][1]");
  const hint = row.locator(".row-hint").first();
  await page.mouse.move(5, 5);
  await expect(hint).toBeVisible();
  await expect(hint).toHaveText(msg("settings.themeHint"));
  const rowHeight = (await row.boundingBox())!.height;
  await row.hover();
  await page.waitForTimeout(400);
  expect(
    Math.abs((await row.boundingBox())!.height - rowHeight),
    "pointing at a setting never pushes the page around",
  ).toBeLessThanOrEqual(0.5);
});
