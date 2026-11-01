import { expect, test } from "@playwright/test";

import { castStep } from "./castSteps";
import { msg } from "./i18nText";
import { fixtureBook, installTauriMock } from "./tauriMock";

const FORTY = Array.from({ length: 40 }, (_, idx) => ({
  id: `ch-${idx + 1}`,
  idx,
  title: `Chapter ${idx + 1}: a reasonably long chapter title`,
  pageStart: idx * 15 + 1,
  pageEnd: idx * 15 + 15,
  isPseudo: false,
  wordCount: 6000,
}));

for (const viewport of [
  { width: 1100, height: 720 },
  { width: 1440, height: 900 },
]) {
  test(`a 40-chapter book keeps the dialog's options in view at ${viewport.width}×${viewport.height}`, async ({
    page,
  }) => {
    await page.setViewportSize(viewport);
    await installTauriMock(page, {
      books: [fixtureBook({ pageCount: 600, chapterCount: 40 })],
      jobScenario: { chapters: FORTY, stageDelayMs: 10 },
    });
    await page.goto("/");
    await page.getByTestId("nav-library").click();
    await page.getByTestId("create-audio-book-fixture").click();
    await expect(page.getByTestId("job-modal")).toBeVisible();

    const list = page.getByTestId("chapter-list");
    await expect(list).toBeVisible();
    const scrolls = await list.evaluate(
      (el) => el.scrollHeight > el.clientHeight + 4,
    );
    expect(scrolls, "forty chapters scroll inside their own box").toBe(true);

    await list.evaluate((el) => el.scrollTo({ top: el.scrollHeight }));
    await expect(
      page.getByText(msg("cast.selectedCount", { selected: 40, total: 40 })),
      "the selection count stays in view while the list scrolls",
    ).toBeInViewport();

    await castStep(page, "sound");
    const footer = await page
      .getByTestId("job-modal")
      .locator("footer")
      .boundingBox();
    for (const id of ["cast-explain-figures", "verify-toggle"]) {
      const box = await page.getByTestId(id).boundingBox();
      expect(
        box!.y + box!.height,
        `${id} must sit above the footer without scrolling the column`,
      ).toBeLessThanOrEqual(footer!.y);
    }
  });
}

test("at the 900x600 minimum a 40-chapter list scrolls in its own box and the step actions stay in view", async ({
  page,
}) => {
  await page.setViewportSize({ width: 900, height: 600 });
  await installTauriMock(page, {
    books: [fixtureBook({ pageCount: 600, chapterCount: 40 })],
    jobScenario: { chapters: FORTY, stageDelayMs: 10 },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  const list = page.getByTestId("chapter-list");
  await expect(list).toBeVisible();
  expect(
    await list.evaluate((el) => el.scrollHeight > el.clientHeight + 4),
  ).toBe(true);
  await expect(page.getByTestId("cast-next")).toBeInViewport({ ratio: 1 });
  await expect(page.getByTestId("start-job")).toBeInViewport({ ratio: 1 });
});
