import { expect, test } from "@playwright/test";

import { msg } from "./i18nText";
import { fixtureBook, fixtureChapters, installTauriMock } from "./tauriMock";

for (const viewport of [
  { width: 1100, height: 720 },
  { width: 900, height: 600 },
]) {
  test(`at ${viewport.width}x${viewport.height} a failed start is shown where the button was pressed`, async ({
    page,
  }) => {
    await page.setViewportSize(viewport);
    await installTauriMock(page, {
      books: [fixtureBook()],
      jobScenario: {
        chapters: fixtureChapters(),
        createError: "ERR_FFMPEG_MISSING: encoder not found",
      },
    });
    await page.goto("/");
    await page.getByTestId("nav-library").click();
    await page.getByTestId("create-audio-book-fixture").click();
    const start = page.getByTestId("start-job");
    await start.click();

    const error = page.getByTestId("cast-error");
    await expect(error).toBeVisible();
    await expect(error).toHaveAttribute("role", "alert");
    await expect(error).toContainText(msg("cast.ffmpegMissing"));
    await expect(
      error,
      "the message is on screen without scrolling the dialog",
    ).toBeInViewport();

    const [errorBox, startBox] = await Promise.all([
      error.boundingBox(),
      start.boundingBox(),
    ]);
    expect(
      startBox!.y - (errorBox!.y + errorBox!.height),
      "the message sits just above the button that failed",
    ).toBeLessThan(120);
    expect(startBox!.y).toBeGreaterThan(errorBox!.y);
  });
}
