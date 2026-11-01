import { expect, test } from "@playwright/test";

import { wavDataUrl } from "./pcmFixture";
import { fixtureBook, installTauriMock } from "./tauriMock";

const seeded = {
  id: "export-seeded",
  bookId: "book-fixture",
  jobId: "job-1",
  path: "/exports/fixture-book.mp3",
  bytes: 5_000_000,
  format: "mp3",
  mode: "narrate",
  stale: false,
  createdAt: 1_755_000_000_000,
};

for (const viewport of [
  { width: 900, height: 600 },
  { width: 1100, height: 720 },
]) {
  test(`at ${viewport.width}x${viewport.height} the page can scroll clear of the mini-player`, async ({
    page,
  }) => {
    await page.setViewportSize(viewport);
    await installTauriMock(page, {
      books: [fixtureBook()],
      seededExports: [seeded],
      readingScenario: { mediaDataUrl: wavDataUrl() },
    });
    await page.goto("/");
    await page.getByTestId("nav-audio").click();
    await page.getByTestId("cover-quick-toggle-export-seeded").click();
    const player = page.getByTestId("mini-player");
    await expect(player).toBeVisible();

    await page.evaluate(() => {
      const scroller = document.querySelector("#main-content > div");
      scroller?.scrollTo({ top: scroller.scrollHeight });
    });
    const gap = await page.evaluate(() => {
      const library = document.querySelector('[data-testid="library"]');
      const mini = document.querySelector('[data-testid="mini-player"]');
      if (library == null || mini == null) return null;
      const last = library.lastElementChild ?? library;
      return (
        mini.getBoundingClientRect().top - last.getBoundingClientRect().bottom
      );
    });
    expect(
      gap,
      "scrolled to the end, the last of the page's content sits above the mini-player instead of under it",
    ).toBeGreaterThanOrEqual(0);
  });
}
