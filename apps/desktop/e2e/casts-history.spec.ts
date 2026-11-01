import { expect, test, type Page } from "@playwright/test";

import { fixtureBook, fixtureChapters, installTauriMock } from "./tauriMock";

function seededJob(
  id: string,
  state: string,
  createdAt: number,
): Record<string, unknown> {
  return {
    id,
    bookId: "book-fixture",
    scope: { kind: "chapters", chapter_ids: ["ch-1"] },
    mode: "narrator",
    voiceProfileId: "preset-kokoro-af-heart",
    outputFormat: "mp3",
    modelOverridesJson: null,
    state,
    overnight: false,
    etaSeconds: null,
    reportJson: null,
    outputPathsJson: state === "DONE" ? `["/exports/${id}.mp3"]` : null,
    createdAt,
    updatedAt: createdAt,
  };
}

async function openCasts(page: Page): Promise<void> {
  await page.setViewportSize({ width: 1440, height: 900 });
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      seededJobs: [
        seededJob("done-old", "DONE", 1_000),
        seededJob("failed", "FAILED", 2_000),
        seededJob("done-new", "DONE", 3_000),
      ],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-jobs").click();
  await page.getByTestId("open-production").click();
  await expect(page.getByTestId("jobs-panel")).toBeVisible();
}

test("finished casts sit in History: the newest and the failed stay open, older ones fold to one line", async ({
  page,
}) => {
  await openCasts(page);
  await expect(page.getByTestId("jobs-now")).toHaveCount(0);
  await expect(page.getByTestId("jobs-history")).toBeVisible();
  await expect(page.getByTestId("job-history-done-new")).toBeVisible();
  await expect(page.getByTestId("job-row-done-new")).toBeVisible();
  await expect(page.getByTestId("job-row-failed")).toBeVisible();
  await expect(page.getByTestId("job-row-done-old")).toHaveCount(0);
  await expect(page.getByTestId("job-expand-done-old")).toHaveAttribute(
    "aria-expanded",
    "false",
  );

  await page.getByTestId("job-expand-done-old").click();
  await expect(page.getByTestId("job-row-done-old")).toBeVisible();
  await expect(page.getByTestId("job-expand-done-old")).toHaveAttribute(
    "aria-expanded",
    "true",
  );
  await page.getByTestId("job-expand-done-new").click();
  await expect(page.getByTestId("job-row-done-new")).toHaveCount(0);
});
