import { expect, test } from "@playwright/test";

import { msg } from "./i18nText";
import { fixtureBook, installTauriMock } from "./tauriMock";

const base = {
  bookId: "book-fixture",
  jobId: "job-1",
  bytes: 5_000_000,
  format: "mp3",
  mode: "narrate",
  stale: false,
  createdAt: 1_755_000_000_000,
};

test("a cast card shows its length, and an older variable-rate cast offers a re-render", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    seededExports: [
      {
        ...base,
        id: "export-exact",
        path: "/exports/Fixture - Ch 1-3.mp3",
        durationMs: 2 * 3_600_000 + 14 * 60_000,
        seekExact: true,
      },
      {
        ...base,
        id: "export-drift",
        path: "/exports/Fixture - Ch 4.mp3",
        durationMs: 47 * 60_000,
        seekExact: false,
      },
      {
        ...base,
        id: "export-unknown",
        path: "/exports/Fixture - Ch 5.mp3",
      },
    ],
  });
  await page.goto("/");
  await page.getByTestId("nav-audio").click();

  await expect(page.getByTestId("audio-length-export-exact")).toHaveText(
    msg("library.durationHm", { h: 2, m: 14 }),
  );
  await expect(page.getByTestId("audio-length-export-drift")).toHaveText(
    msg("library.durationM", { m: 47 }),
  );
  await expect(page.getByTestId("audio-length-export-unknown")).toHaveCount(0);

  await expect(page.getByTestId("audio-seek-drift-export-exact")).toHaveCount(
    0,
  );
  await expect(page.getByTestId("audio-seek-drift-export-unknown")).toHaveCount(
    0,
  );
  await expect(page.getByTestId("audio-seek-drift-export-drift")).toContainText(
    msg("library.seekDrift"),
  );

  await page.getByTestId("audio-rerender-export-drift").click();
  await expect(page.getByTestId("job-modal")).toBeVisible();
  await expect(page.getByTestId("mini-player")).toHaveCount(0);
});
