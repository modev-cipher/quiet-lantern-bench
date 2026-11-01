import { expect, test, type Page } from "@playwright/test";

import { installTauriMock } from "./tauriMock";

// I18N-1 pins (owner law #25 Q3 / Lane 1): default = host OS locale; the
// Settings toggle wins, applies at once without a reload, and survives a
// restart; direction follows the UI locale unless the direction preference
// overrides it. The mock's `host_locale` stands in for the OS.

const html = (page: Page) => page.locator("html");

async function openSettings(page: Page, hostLocale: string | null) {
  await installTauriMock(page, { books: [], hostLocale });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("language-settings")).toBeVisible();
}

test("host locale ar → Arabic UI, RTL root", async ({ page }) => {
  await openSettings(page, "ar-EG");
  await expect(html(page)).toHaveAttribute("lang", "ar");
  await expect(html(page)).toHaveAttribute("dir", "rtl");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("الإعدادات");
  await expect(page.getByTestId("language-choice-system")).toHaveAttribute(
    "aria-checked",
    "true",
  );
});

test("host locale en_US.UTF-8 (POSIX shape) → English, LTR", async ({ page }) => {
  await openSettings(page, "en_US.UTF-8");
  await expect(html(page)).toHaveAttribute("lang", "en");
  await expect(html(page)).toHaveAttribute("dir", "ltr");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Settings");
});

test("no host answer falls back to the webview language (en-US here)", async ({
  page,
}) => {
  await openSettings(page, null);
  await expect(html(page)).toHaveAttribute("lang", "en");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Settings");
});

test("the toggle switches instantly (no navigation) and persists across a restart", async ({
  page,
}) => {
  await openSettings(page, "en-US");
  const navigations: string[] = [];
  page.on("framenavigated", (frame) => {
    if (frame === page.mainFrame()) navigations.push(frame.url());
  });
  await page.getByTestId("language-choice-ar").click();
  await expect(html(page)).toHaveAttribute("lang", "ar");
  await expect(html(page)).toHaveAttribute("dir", "rtl");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("الإعدادات");
  await expect(page.getByTestId("nav-settings")).toContainText("الإعدادات");
  expect(navigations, "switching must not reload or navigate").toEqual([]);
  const invoked = await page.evaluate(
    () =>
      (window as unknown as { __invokedCommands: string[] }).__invokedCommands,
  );
  expect(invoked.filter((c) => c === "set_locale_preference")).toHaveLength(1);

  // restart: the mock keeps the persisted preference in sessionStorage
  await page.reload();
  await expect(html(page)).toHaveAttribute("lang", "ar");
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("language-choice-ar")).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await page.getByTestId("language-choice-en").click();
  await expect(html(page)).toHaveAttribute("lang", "en");
  await expect(html(page)).toHaveAttribute("dir", "ltr");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Settings");
});

test("an explicit direction preference overrides the locale's direction", async ({
  page,
}) => {
  await openSettings(page, "ar");
  await expect(html(page)).toHaveAttribute("dir", "rtl");
  await page.getByTestId("direction-choice-ltr").click();
  await expect(html(page)).toHaveAttribute("dir", "ltr");
  await expect(html(page)).toHaveAttribute("lang", "ar");
  await page.getByTestId("direction-choice-system").click();
  await expect(html(page)).toHaveAttribute("dir", "rtl");
});
