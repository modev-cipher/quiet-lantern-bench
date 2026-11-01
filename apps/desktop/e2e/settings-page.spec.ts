import { expect, test } from "@playwright/test";

import { msg } from "./i18nText";
import { fixtureBook, installTauriMock } from "./tauriMock";

async function openSettings(page: import("@playwright/test").Page) {
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("settings-frame")).toBeVisible();
}

test("Settings is one page: one title and an index of its three sections", async ({
  page,
}) => {
  await openSettings(page);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(
    msg("settings.title"),
  );
  const index = page.getByRole("navigation", {
    name: msg("settings.sections"),
  });
  await expect(index.getByRole("button")).toHaveText([
    msg("nav.general"),
    msg("nav.voicesAndModels"),
    msg("nav.providers"),
  ]);
  await expect(page.getByTestId("settings-tab-general")).toHaveAttribute(
    "aria-current",
    "page",
  );
  await expect(
    page.getByRole("heading", { level: 2, name: msg("nav.general") }),
  ).toBeVisible();
});

test("each section opens in the same frame and keeps the top bar on Settings", async ({
  page,
}) => {
  await openSettings(page);
  const frame = page.getByTestId("settings-frame");

  await page.getByTestId("nav-models").click();
  await expect(frame.getByTestId("models-panel")).toBeVisible();
  await expect(
    frame.getByRole("heading", { level: 2, name: msg("nav.voicesAndModels") }),
  ).toBeVisible();
  await expect(page.getByTestId("nav-models")).toHaveAttribute(
    "aria-current",
    "page",
  );
  await expect(page.getByTestId("nav-settings")).toHaveAttribute(
    "aria-current",
    "page",
  );

  await page.getByTestId("nav-providers").click();
  await expect(frame.getByTestId("providers-page")).toBeVisible();
  await expect(
    frame.getByRole("heading", { level: 2, name: msg("nav.providers") }),
  ).toBeVisible();
  await expect(
    page.getByTestId("nav-settings"),
    "the top bar still says you are in Settings while on Providers",
  ).toHaveAttribute("aria-current", "page");
  await expect(page.getByRole("heading", { level: 1 })).toHaveCount(1);
});

test("at the minimum window the index sits above the section instead of beside it", async ({
  page,
}) => {
  await page.setViewportSize({ width: 900, height: 600 });
  await openSettings(page);
  const index = await page
    .getByRole("navigation", { name: msg("settings.sections") })
    .boundingBox();
  const section = await page.getByTestId("settings-panel").boundingBox();
  expect(index!.y + index!.height).toBeLessThanOrEqual(section!.y + 1);
});

test("at the default window the index sits at the reading-start edge beside the section", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1100, height: 720 });
  await openSettings(page);
  const index = await page
    .getByRole("navigation", { name: msg("settings.sections") })
    .boundingBox();
  const section = await page.getByTestId("settings-panel").boundingBox();
  const rtl = (await page.locator("html").getAttribute("dir")) === "rtl";
  if (rtl) {
    expect(index!.x).toBeGreaterThanOrEqual(section!.x + section!.width - 1);
  } else {
    expect(index!.x + index!.width).toBeLessThanOrEqual(section!.x + 1);
  }
});
