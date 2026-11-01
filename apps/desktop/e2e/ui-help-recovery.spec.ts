import { expect, test } from "@playwright/test";

import en from "../src/i18n/messages/en/shell.json" with { type: "json" };
import ar from "../src/i18n/messages/ar/shell.json" with { type: "json" };
import { BUG_REPORT_URL } from "../src/reportIssueModel";
import { fixtureBook, installTauriMock } from "./tauriMock";

for (const locale of ["en", "ar"] as const) {
  const copy = locale === "ar" ? ar : en;
  for (const theme of ["light", "dark"] as const) {
    for (const viewport of [
      { width: 900, height: 600 },
      { width: 450, height: 300 },
    ]) {
      test(`${locale} ${theme} Help stays reachable at ${viewport.width}x${viewport.height}`, async ({
        page,
      }) => {
        await page.setViewportSize(viewport);
        await installTauriMock(page, {
          hostLocale: locale,
          books: [fixtureBook()],
        });
        await page.emulateMedia({ colorScheme: theme });
        await page.goto("/");
        await expect(page.getByTestId("launch-intro")).toHaveCount(0);
        await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
        const opener = page.getByTestId("nav-report-issue");
        await opener.click();
        const dialog = page
          .getByTestId("report-issue-modal")
          .getByRole("dialog");
        const box = (await dialog.boundingBox())!;
        expect(box.y).toBeGreaterThanOrEqual(0);
        expect(box.y + box.height).toBeLessThanOrEqual(viewport.height);
        const close = page.getByTestId("report-issue-close");
        const body = page.getByTestId("report-issue-body");
        await close.focus();
        for (const option of await dialog
          .locator('[data-testid^="issue-option-"]')
          .all()) {
          await page.keyboard.press("Tab");
          await expect(option).toBeFocused();
          await expect(option).toBeInViewport({ ratio: 0.99 });
          const optionBox = (await option.boundingBox())!;
          const bodyBox = (await body.boundingBox())!;
          expect(optionBox.y).toBeGreaterThanOrEqual(bodyBox.y - 1);
          expect(optionBox.y + optionBox.height).toBeLessThanOrEqual(
            bodyBox.y + bodyBox.height + 1,
          );
          await expect(close).toBeInViewport({ ratio: 1 });
          await expect(dialog.getByRole("heading")).toBeInViewport({
            ratio: 1,
          });
        }
        await page.screenshot({
          path: test.info().outputPath("help-layout.png"),
        });
        await close.click();
        await expect(dialog).toHaveCount(0);
        await expect(opener).toBeFocused();
      });
    }

    test(`${locale} ${theme} a failed Help link stays open with copy and retry`, async ({
      page,
    }) => {
      await page.setViewportSize({ width: 450, height: 300 });
      await installTauriMock(page, {
        hostLocale: locale,
        books: [fixtureBook()],
      });
      await page.emulateMedia({ colorScheme: theme });
      await page.goto("/");
      await expect(page.getByTestId("launch-intro")).toHaveCount(0);
      await page.evaluate(() => {
        const runtime = window as unknown as {
          __TAURI_INTERNALS__: {
            invoke: (
              command: string,
              args?: unknown,
              options?: unknown,
            ) => Promise<unknown>;
          };
        };
        const original = runtime.__TAURI_INTERNALS__.invoke;
        let attempts = 0;
        runtime.__TAURI_INTERNALS__.invoke = async (command, args, options) => {
          if (command === "plugin:opener|open_url" && attempts++ < 2)
            throw new Error("browser unavailable");
          return original(command, args, options);
        };
        Object.defineProperty(navigator, "clipboard", {
          configurable: true,
          value: {
            writeText: async (text: string) =>
              sessionStorage.setItem("help-copied-url", text),
          },
        });
      });
      const opener = page.getByTestId("nav-report-issue");
      await opener.click();
      await page.getByTestId("issue-option-bug").click();
      const dialog = page.getByTestId("report-issue-modal").getByRole("dialog");
      await expect(dialog.getByRole("alert").first()).toBeVisible();
      const link = dialog.getByRole("textbox", { name: copy["issue.link"] });
      await expect(link).toHaveValue(BUG_REPORT_URL);
      await expect(link).toHaveAttribute("dir", "ltr");
      await dialog.getByRole("button", { name: copy["issue.retry"] }).focus();
      await dialog.getByRole("button", { name: copy["issue.retry"] }).click();
      await expect(dialog.getByRole("alert").first()).toBeFocused();
      await dialog
        .getByRole("button", { name: copy["issue.copyLink"] })
        .click();
      await expect(dialog.getByRole("status")).toHaveText(
        copy["issue.linkCopied"],
      );
      await expect(dialog.getByTestId("issue-copy-link")).toHaveText(
        copy["issue.linkCopied"],
      );
      await expect(dialog.getByTestId("issue-copy-link")).toBeInViewport({
        ratio: 0.99,
      });
      const copiedBox = (await dialog
        .getByTestId("issue-copy-link")
        .boundingBox())!;
      const bodyBox = (await page
        .getByTestId("report-issue-body")
        .boundingBox())!;
      expect(copiedBox.y).toBeGreaterThanOrEqual(bodyBox.y - 1);
      expect(copiedBox.y + copiedBox.height).toBeLessThanOrEqual(
        bodyBox.y + bodyBox.height + 1,
      );
      expect(
        await page.evaluate(() => sessionStorage.getItem("help-copied-url")),
      ).toBe(BUG_REPORT_URL);
      await expect(page.getByTestId("report-issue-close")).toBeInViewport({
        ratio: 1,
      });
      await page.screenshot({
        path: test.info().outputPath("help-recovery.png"),
      });
      await dialog.getByRole("button", { name: copy["issue.retry"] }).click();
      await expect(dialog).toHaveCount(0);
      await expect(opener).toBeFocused();
    });
  }
}
