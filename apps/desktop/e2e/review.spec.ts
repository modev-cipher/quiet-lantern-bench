import { expect, test } from "@playwright/test";
import { castStep } from "./castSteps";
import { msg } from "./i18nText";

import {
  createJobCalls,
  fixtureBook,
  fixtureChapters,
  installTauriMock,
  type MockSegment,
} from "./tauriMock";

function segment(overrides: Partial<MockSegment> = {}): MockSegment {
  return {
    id: "seg-1",
    chapterId: "ch-1",
    idx: 0,
    pageNo: 1,
    kind: "text",
    text: "The lion sleeps tonight.",
    flags: [],
    hasAudio: true,
    durationMs: 900,
    edited: false,
    wer: null,
    alignSource: "whisperx",
    ...overrides,
  };
}

const VERIFY_REPORT = JSON.stringify({
  segment_count: 3,
  verify: {
    enabled: true,
    threshold: 0.15,
    counts: { ok: 2, failed: 1, missing: 0, unscoreable_ref: 0 },
    worst: [
      {
        segment_id: "seg-2",
        chapter_id: "ch-1",
        idx: 1,
        band: "failed",
        wer: 0.42,
        reference_words: 12,
      },
    ],
  },
});

const SEGMENTS = [
  segment(),
  segment({
    id: "seg-2",
    idx: 1,
    text: "The lyin sleps tonite.",
    flags: ["verify_failed"],
  }),
  segment({ id: "seg-3", idx: 2, text: "A third sentence." }),
];

async function startDoneJob(
  page: import("@playwright/test").Page,
  extra: Record<string, unknown> = {},
) {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 10,
      reportJson: VERIFY_REPORT,
      segments: SEGMENTS.map((row) => ({ ...row })),
      ...extra,
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");
  await page.getByTestId("verify-toggle").check();
  await expect(page.getByTestId("verify-toggle")).toBeChecked();
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-notices-job-1")).toBeVisible({
    timeout: 15_000,
  });
}

test("the accuracy check reaches the backend and its failures surface as a notice", async ({
  page,
}) => {
  await startDoneJob(page);
  expect(await createJobCalls(page)).toEqual([
    expect.objectContaining({ verify: true }),
  ]);
  const notices = page.getByTestId("job-notices-job-1");
  await expect(notices).toContainText(
    msg("job.accuracyDiff", { count: 1, pct: 15 }),
  );
  await expect(page.getByTestId("review-summary-job-1")).toContainText(
    msg("review.summary", { count: 1, total: 3 }),
  );
});

const UNCHECKED_REPORT = JSON.stringify({
  segment_count: 3,
  verify: {
    enabled: true,
    threshold: 0.15,
    counts: {
      ok: 0,
      failed: 0,
      missing: 0,
      unscoreable_ref: 0,
      unchecked: 3,
    },
    worst: [],
  },
});

test("an accuracy check that never ran does not read as an accuracy check that passed", async ({
  page,
}) => {
  await startDoneJob(page, { reportJson: UNCHECKED_REPORT });

  const summary = page.getByTestId("review-summary-job-1");
  await expect(summary).toContainText(
    msg("review.uncheckedSummary", { count: 3, total: 3 }),
  );
  await expect(summary).not.toContainText(
    msg("review.summary", { count: 0, total: 3 }),
  );
  await expect(summary).not.toContainText(msg("review.off"));

  await expect(page.getByTestId("job-notices-job-1")).toContainText(
    msg("job.accuracyUnchecked", { count: 3, total: 3 }),
  );

  await page.getByTestId("review-toggle-job-1").click();
  const notice = page.getByTestId("review-unchecked-job-1");
  await expect(notice).toBeVisible();
  await expect(notice).toContainText(
    msg("review.uncheckedNotice", { count: 3 }),
  );
  await expect(notice).not.toContainText(msg("review.band.missing"));

  await expect(page.getByTestId("review-empty-job-1")).toHaveCount(0);
});

test("the accuracy check is off unless it is asked for", async ({ page }) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: { chapters: fixtureChapters(), stageDelayMs: 10 },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");
  await expect(page.getByTestId("verify-toggle")).not.toBeChecked();
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 15_000,
  });
  expect(await createJobCalls(page)).toEqual([
    expect.objectContaining({ verify: false }),
  ]);
  await expect(page.getByTestId("review-summary-job-1")).toContainText(
    msg("review.off"),
  );
});

test("editing a flagged sentence re-records it and returns the job to the pipeline", async ({
  page,
}) => {
  await startDoneJob(page);
  await page.getByTestId("review-toggle-job-1").click();
  await expect(page.getByTestId("review-row-seg-2")).toBeVisible();
  await expect(page.getByTestId("review-text-seg-2")).toContainText(
    "The lyin sleps tonite.",
  );
  await expect(page.getByTestId("review-wer-seg-2")).toContainText(
    msg("review.wer", { percent: 42 }),
  );
  await expect(page.getByTestId("review-row-seg-1")).toHaveCount(0);

  await page.getByTestId("review-edit-seg-2").click();
  const input = page.getByTestId("review-input-seg-2");
  await expect(input).toHaveValue("The lyin sleps tonite.");
  await input.fill("The lion sleeps tonight.");
  await page.getByTestId("review-save-seg-2").click();

  await expect(page.getByTestId("review-blocked-job-1")).toBeVisible({
    timeout: 15_000,
  });
  await expect(page.getByTestId("review-row-seg-2")).toBeVisible();
  await expect(page.getByTestId("review-edit-seg-2")).toBeDisabled();
  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 20_000,
  });
  await expect(page.getByTestId("review-text-seg-2")).toContainText(
    "The lion sleeps tonight.",
  );
  await expect(page.getByTestId("review-revert-seg-2")).toBeVisible();
});

test("reverting an edited sentence restores what the pipeline wrote and re-records it", async ({
  page,
}) => {
  await startDoneJob(page);
  await page.getByTestId("review-toggle-job-1").click();
  await page.getByTestId("review-edit-seg-2").click();
  await page.getByTestId("review-input-seg-2").fill("A hand-written line.");
  await page.getByTestId("review-save-seg-2").click();
  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 20_000,
  });
  await expect(page.getByTestId("review-text-seg-2")).toContainText(
    "A hand-written line.",
  );

  const revert = page.getByTestId("review-revert-seg-2");
  await expect(revert).toBeEnabled();
  await revert.click();

  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 20_000,
  });
  await expect(page.getByTestId("review-text-seg-2")).toContainText(
    "The lyin sleps tonite.",
  );
  await expect(page.getByTestId("review-revert-seg-2")).toHaveCount(0, {
    timeout: 10_000,
  });
});

test("a sentence cannot be re-recorded while the cast is still running", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 400,
      reportJson: VERIFY_REPORT,
      segments: SEGMENTS.map((row) => ({ ...row })),
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");
  await page.getByTestId("verify-toggle").check();
  await page.getByTestId("start-job").click();

  await expect(page.getByTestId("job-row-job-1")).toBeVisible();
  await page.getByTestId("review-toggle-job-1").click();
  await expect(page.getByTestId("review-blocked-job-1")).toContainText(
    msg("review.block.running"),
  );
  await expect(page.getByTestId("review-edit-seg-2")).toHaveCount(0);

  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 30_000,
  });
  await expect(page.getByTestId("review-blocked-job-1")).toHaveCount(0);
  await expect(page.getByTestId("review-edit-seg-2")).toBeEnabled();
});

test("a stale export refuses to play and offers a re-render that clears it", async ({
  page,
}) => {
  await startDoneJob(page, { staleExport: true });

  await expect(page.getByTestId("job-stale-job-1")).toContainText(
    msg("review.staleExport", { count: 1 }),
  );
  await expect(page.getByTestId("job-output-job-1")).toHaveAttribute(
    "data-stale",
    "true",
  );
  await expect(page.getByTestId("job-reveal-job-1")).toHaveAccessibleName(
    msg("job.revealStale"),
  );

  await page.getByTestId("nav-library").click();
  await page.getByTestId("nav-audio").click();
  const card = page.getByTestId("audio-card-export-job-1");
  await expect(card).toBeVisible();
  await expect(page.getByTestId("audio-stale-export-job-1")).toContainText(
    msg("library.rerenderNeeded"),
  );
  await expect(page.getByTestId("cover-quick-toggle-export-job-1")).toHaveCount(
    0,
  );

  await page.getByTestId("nav-jobs").click();
  await page.getByTestId("open-production").click();
  await page.getByTestId("job-rerender-job-1").click();
  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 20_000,
  });
  await expect(page.getByTestId("job-stale-job-1")).toHaveCount(0);
  await expect(page.getByTestId("job-output-job-1")).toHaveAttribute(
    "data-stale",
    "false",
  );

  await page.getByTestId("nav-library").click();
  await page.getByTestId("nav-audio").click();
  await expect(page.getByTestId("audio-stale-export-job-1")).toHaveCount(0);
  await expect(
    page.getByTestId("cover-quick-toggle-export-job-1"),
  ).toBeVisible();
});
