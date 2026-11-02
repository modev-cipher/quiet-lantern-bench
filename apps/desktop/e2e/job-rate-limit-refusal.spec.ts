import { expect, test } from "@playwright/test";

import { fixtureBook, fixtureChapters, installTauriMock } from "./tauriMock";
import { storeError } from "./i18nText";

const DAILY_QUOTA_REASON = {
  kind: "rate_limit",
  class: "daily",
  provider: "google",
  retry_at_epoch_ms: null,
  segment_idx: 4,
  last_error_message: "429 RESOURCE_EXHAUSTED",
  attempt: 5,
};

const PER_MINUTE_REASON = {
  kind: "rate_limit",
  class: "per_minute",
  provider: "google",
  retry_at_epoch_ms: Date.now() + 600_000,
  segment_idx: 4,
  last_error_message: "429 RESOURCE_EXHAUSTED",
  attempt: 2,
};

const REFUSAL = "the worker is shutting down, so this cast was not switched";

function pausedJob(id: string, reason: unknown) {
  return {
    id,
    bookId: "book-fixture",
    scope: { kind: "chapters", chapter_ids: ["ch-1"] },
    mode: "narrator",
    voiceProfileId: "preset-kokoro-af-heart",
    outputFormat: "mp3",
    modelOverridesJson: null,
    state: "PAUSED",
    overnight: false,
    etaSeconds: null,
    reportJson: JSON.stringify({ paused_reason: reason }),
    outputPathsJson: null,
    createdAt: Date.now(),
    updatedAt: Date.now(),
  };
}

function pausedDailyJob() {
  return pausedJob("job-daily", DAILY_QUOTA_REASON);
}

function pausedPerMinuteJob() {
  return pausedJob("job-per-minute", PER_MINUTE_REASON);
}

test("a daily-quota card offers no overnight switch, because nothing would resume it", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      seededJobs: [pausedDailyJob()],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-jobs").click();
  await page.getByTestId("open-production").click();

  await expect(page.getByTestId("job-rate-limit-card-job-daily")).toBeVisible();
  await expect(page.getByTestId("job-rate-limit-try-now-job-daily")).toBeVisible();
  await expect(page.getByTestId("job-rate-limit-overnight-job-daily")).toHaveCount(0);
});

test("a refused switch on the per-minute card reaches the user too", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (line) => {
    if (line.type() === "error") consoleErrors.push(line.text());
  });
  const unhandled: string[] = [];
  page.on("pageerror", (error) => unhandled.push(error.message));

  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      seededJobs: [pausedPerMinuteJob()],
      switchToOvernightError: REFUSAL,
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-jobs").click();
  await page.getByTestId("open-production").click();

  const card = page.getByTestId("job-rate-limit-card-job-per-minute");
  await expect(card).toBeVisible();
  await expect(card).toHaveAttribute("role", "status");

  const alert = page.getByTestId("job-rate-limit-error-job-per-minute");
  await expect(alert).toHaveCount(0);

  await page.getByTestId("job-rate-limit-overnight-job-per-minute").click();

  await expect(alert).toBeVisible();
  await expect(alert).toContainText(storeError(REFUSAL));
  expect(
    unhandled,
    "a dropped rejection crashes nothing but leaves the user with a dead button",
  ).toEqual([]);
  expect(
    consoleErrors.filter((text) => text.includes(REFUSAL)),
    "the refusal leaked to the console instead of the alert; that is the same defect the user \
cannot see",
  ).toEqual([]);
});

test("the per-minute refusal is the only alert a role query can reach", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      seededJobs: [pausedPerMinuteJob()],
      switchToOvernightError: REFUSAL,
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-jobs").click();
  await page.getByTestId("open-production").click();

  await page.getByTestId("job-rate-limit-overnight-job-per-minute").click();

  await expect(
    page.getByTestId("job-rate-limit-error-job-per-minute"),
  ).toBeVisible();
  const byRole = page.getByRole("alert");
  await expect(byRole).toHaveCount(1);
  await expect(byRole).toContainText(storeError(REFUSAL));
  await expect(
    page.getByTestId("job-rate-limit-card-job-per-minute").getByRole("alert"),
    "an alert nested inside the role='status' card makes getByRole('alert') ambiguous between the \
refusal and the card itself, which breaks every spec that selects the alert by role",
  ).toHaveCount(0);
});

test("the per-minute card reports nothing when the backend accepts the switch", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      seededJobs: [pausedPerMinuteJob()],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-jobs").click();
  await page.getByTestId("open-production").click();

  await page.getByTestId("job-rate-limit-overnight-job-per-minute").click();

  await expect(
    page.getByTestId("job-rate-limit-error-job-per-minute"),
  ).toHaveCount(0);
});
