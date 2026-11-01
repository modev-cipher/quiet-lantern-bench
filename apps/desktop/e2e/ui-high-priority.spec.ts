import { expect, test, type Locator, type Page } from "@playwright/test";

import catalog from "../../../catalog/catalog.json" with { type: "json" };
import {
  fixtureBook,
  fixtureChapters,
  fixtureHardware,
  installTauriMock,
} from "./tauriMock";

async function expectFocusInside(dialog: Locator) {
  await expect
    .poll(() =>
      dialog.evaluate((node) => node.contains(document.activeElement)),
    )
    .toBe(true);
}

async function checkTabBoundaries(page: Page, dialog: Locator) {
  await page.keyboard.press("Shift+Tab");
  await expectFocusInside(dialog);
  const controls = dialog.locator(
    "button:visible:enabled, a[href]:visible, input:visible:enabled, textarea:visible:enabled, select:visible:enabled",
  );
  await controls.first().focus();
  await page.keyboard.press("Shift+Tab");
  await expect(controls.last()).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(controls.first()).toBeFocused();
}

for (const locale of ["en", "ar"] as const) {
  test.describe(`${locale} high-priority usability`, () => {
    test.beforeEach(async ({ page }) => {
      await page.setViewportSize({ width: 900, height: 600 });
      await installTauriMock(page, {
        hostLocale: locale,
        books: [fixtureBook()],
        jobScenario: { chapters: fixtureChapters() },
        modelScenario: {
          catalogModels: catalog.models,
          hardware: fixtureHardware(),
          installed: [
            { modelId: "kokoro-82m", diskBytes: 355_000_000 },
            { modelId: "chatterbox-0.5b", diskBytes: 3_200_000_000 },
          ],
        },
        seededExports: [
          {
            id: "export-focus",
            bookId: "book-fixture",
            jobId: "job-focus",
            path: "/exports/Fixture Book - full - narrate.mp3",
            bytes: 5_000_000,
            format: "mp3",
            mode: "narrate",
            stale: false,
            createdAt: 1_755_000_000_000,
          },
        ],
      });
      await page.goto("/");
      await expect(page.getByTestId("launch-intro")).toHaveCount(0);
    });

    test("phone transfer contains reverse tab and restores its opener", async ({
      page,
    }) => {
      await page.getByTestId("nav-audio").click();
      const opener = page.getByTestId("send-to-phone");
      await opener.click();
      const dialog = page
        .getByTestId("send-to-phone-modal")
        .getByRole("dialog");
      await expect(dialog).toBeFocused();
      await checkTabBoundaries(page, dialog);
      expect(
        await opener.evaluate((node) => node.closest("[inert]") != null),
      ).toBe(true);
      await opener.evaluate((node) => node.focus());
      await expectFocusInside(dialog);
      await page.keyboard.press("Escape");
      await expect(dialog).toHaveCount(0);
      await expect(opener).toBeFocused();
      expect(
        await opener.evaluate((node) => node.closest("[inert]") != null),
      ).toBe(false);
    });

    test("help contains focus while its backdrop still dismisses it", async ({
      page,
    }) => {
      const opener = page.getByTestId("nav-report-issue");
      await opener.click();
      const dialog = page.getByTestId("report-issue-modal").getByRole("dialog");
      await checkTabBoundaries(page, dialog);
      await page.mouse.click(5, 5);
      await expect(dialog).toHaveCount(0);
      await expect(opener).toBeFocused();
    });

    test("nested cloning owns focus and returns to the unchanged cast", async ({
      page,
    }) => {
      await page.getByTestId("create-audio-book-fixture").click();
      const cast = page.getByTestId("job-modal").getByRole("dialog").first();
      await checkTabBoundaries(page, cast);
      await page.getByTestId("cast-step-sound").click();
      await page.getByTestId("voice-select-button").click();
      await page.keyboard.press("Escape");
      await expect(page.getByTestId("voice-select-menu")).toHaveCount(0);
      await expect(cast).toBeVisible();
      const opener = page.getByTestId("clone-voice-open");
      await opener.click();
      const clone = page.getByTestId("clone-voice-modal").getByRole("dialog");
      await checkTabBoundaries(page, clone);
      await expect(cast).toHaveAttribute("inert", "");
      await page.keyboard.press("Escape");
      await expect(clone).toHaveCount(0);
      await expect(cast).toBeVisible();
      await expect(cast).not.toHaveAttribute("inert", "");
      await expect(opener).toBeFocused();
      await page.keyboard.press("Escape");
      await expect(cast).toHaveCount(0);
      await expect(page.getByTestId("create-audio-book-fixture")).toBeFocused();
    });

    test("the full reader contains focus and returns to the shelf", async ({
      page,
    }) => {
      await page.getByTestId("book-card").getByRole("button").first().click();
      const reader = page.getByTestId("reading-view").getByRole("dialog");
      await checkTabBoundaries(page, reader);
      await page.keyboard.press("Escape");
      await expect(reader).toHaveCount(0);
      await expect(page.getByTestId("library")).toBeVisible();
      expect(
        await page
          .getByTestId("shell-rail")
          .evaluate((node) => node.closest("[inert]") != null),
      ).toBe(false);
    });

    test("the tutor remains nonmodal", async ({ page }) => {
      await page.getByTestId("tutor-launcher").click();
      await expect(page.getByTestId("tutor-panel")).toBeVisible();
      await page.getByTestId("nav-settings").click();
      await expect(page.getByTestId("settings-frame")).toBeVisible();
      await expect(page.getByTestId("tutor-panel")).toBeVisible();
    });

    test("activity and Help remain usable above the reader", async ({
      page,
    }) => {
      await page.getByTestId("book-card").getByRole("button").first().click();
      const reader = page.getByTestId("reading-view").getByRole("dialog");
      await page.getByTestId("book-activity").click();
      const activity = page.getByTestId("activity-tray");
      await expect(activity).toBeFocused();
      await checkTabBoundaries(page, activity);
      await page.keyboard.press("Escape");
      await expect(activity).toHaveCount(0);
      await expect(page.getByTestId("book-activity")).toBeFocused();
      await page.keyboard.press("Control+k");
      await page.getByTestId("palette-action:report").click();
      const help = page.getByTestId("report-issue-modal").getByRole("dialog");
      await expect(help).toBeFocused();
      await checkTabBoundaries(page, help);
      await page.getByTestId("report-issue-close").click();
      await expect(help).toHaveCount(0);
      await expect(reader).toBeVisible();
      await expect(reader).not.toHaveAttribute("inert", "");
      await expectFocusInside(reader);
    });
  });

  for (const viewport of [
    { width: 900, height: 600 },
    { width: 450, height: 300 },
  ]) {
    test(`${locale} expanded onboarding stays reachable at ${viewport.width}x${viewport.height}`, async ({
      page,
    }) => {
      await page.setViewportSize(viewport);
      await installTauriMock(page, {
        hostLocale: locale,
        books: [],
        onboardingComplete: false,
        modelScenario: {
          catalogModels: catalog.models,
          hardware: fixtureHardware(),
        },
      });
      await page.goto("/");
      await page.getByTestId("wizard-next").click();
      await page.getByTestId("wizard-teacher").locator("summary").click();
      const wizard = page.getByTestId("wizard");
      await wizard.evaluate((node) => {
        node.parentElement!.scrollTop = 0;
      });
      await expect
        .poll(async () => (await wizard.boundingBox())!.y)
        .toBeGreaterThanOrEqual(0);
      const choices = wizard.locator('[data-testid^="wizard-option-"]');
      expect(await choices.count()).toBeGreaterThan(4);
      for (const choice of await choices.all()) {
        await choice.scrollIntoViewIfNeeded();
        const box = (await choice.boundingBox())!;
        expect(box.y).toBeGreaterThanOrEqual(-1);
        expect(box.y + box.height).toBeLessThanOrEqual(viewport.height + 1);
      }
      await page.getByTestId("wizard-next").click();
      await expect(page.getByTestId("wizard-finish")).toBeVisible();
    });
  }
}
