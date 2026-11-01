import { expect, test } from "@playwright/test";

import { castStep } from "./castSteps";

import {
  fixtureBook,
  fixtureCatalogModels,
  fixtureChapters,
  fixtureHardware,
  installTauriMock,
} from "./tauriMock";

for (const viewport of [
  { width: 1100, height: 720 },
  { width: 1440, height: 900 },
]) {
  test(`at ${viewport.width} wide the step summary reads as aligned label and value columns`, async ({
    page,
  }) => {
    await page.setViewportSize(viewport);
    await installTauriMock(page, {
      books: [fixtureBook()],
      jobScenario: { chapters: fixtureChapters(), stageDelayMs: 10 },
      modelScenario: {
        catalogModels: fixtureCatalogModels(),
        hardware: fixtureHardware(),
      },
    });
    await page.goto("/");
    await page.getByTestId("nav-library").click();
    await page.getByTestId("create-audio-book-fixture").click();
    await castStep(page, "review");
    const summary = page.getByTestId("phase-summary");
    await expect(summary).toBeVisible();

    const rows = await summary
      .locator('[data-testid^="phase-summary-"]')
      .evaluateAll((elements) =>
        elements.map((row) => {
          const rtl = getComputedStyle(row).direction === "rtl";
          const start = (box: DOMRect) =>
            Math.round(rtl ? box.right : box.left);
          const label = row.children[0] as HTMLElement;
          const value = row.children[1] as HTMLElement;
          const title = value.firstElementChild as HTMLElement;
          const range = document.createRange();
          range.selectNodeContents(title);
          return {
            labelStart: start(label.getBoundingClientRect()),
            valueStart: start(value.getBoundingClientRect()),
            textStart: start(range.getBoundingClientRect()),
          };
        }),
      );
    expect(rows.length).toBeGreaterThan(1);
    for (const row of rows) {
      expect(row.labelStart, "labels share one edge").toBe(rows[0].labelStart);
      expect(
        row.valueStart,
        "values start on one shared edge instead of each row ending ragged against the right",
      ).toBe(rows[0].valueStart);
      expect(
        Math.abs(row.textStart - row.valueStart),
        "the value's text starts at its column's edge",
      ).toBeLessThanOrEqual(1);
    }
  });
}
