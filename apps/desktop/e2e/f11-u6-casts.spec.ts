// F11-U6 — the delete-cast gate (spec §3 J7, §7.4 D8–D10) and the batch
// queue's visible order (C9/C10). Testids + catalog text only (runs under
// I18N_E2E_LOCALE=ar). The mock stands in for the runtime: `job_delete_preview`
// answers `{ session_count, live }`, `delete_job` refuses a live cast, and
// `persistJobs` keeps rows across a document reload the way SQLite does.
import { expect, test, type Page } from "@playwright/test";

import { msg } from "./i18nText";
import {
  fixtureBook,
  fixtureChapters,
  installTauriMock,
  type MockSessionRow,
} from "./tauriMock";

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
    outputPathsJson: state === "DONE" ? '["/exports/x.mp3"]' : null,
    createdAt,
    updatedAt: createdAt,
  };
}

function session(id: string, jobId: string): MockSessionRow {
  return {
    id,
    book_id: "book-fixture",
    job_id: jobId,
    started_at: Date.now() - 7_200_000,
    ended_at: Date.now() - 6_600_000,
    end_reason: "ended",
    duration_ms: 600_000,
    turn_count: 3,
    summary_excerpt: null,
    position: { kind: "ms", value: 1000 },
    job_resolves: true,
    provider_changed: false,
  };
}

async function openJobs(page: Page): Promise<void> {
  await page.getByTestId("nav-jobs").click();
  await page.getByTestId("open-production").click();
  await expect(page.getByTestId("jobs-panel")).toBeVisible();
}

async function deleteCalls(page: Page): Promise<number> {
  return page.evaluate(
    () =>
      (window as unknown as { __mockCalls: Record<string, number> }).__mockCalls
        .delete_job ?? 0,
  );
}

test("J7.3: deleting a cast with tutor sessions asks first, names the count, and cascades on confirm", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      seededJobs: [seededJob("job-done", "DONE", Date.now() - 10_000)],
    },
    voiceSessionsScenario: {
      rows: [session("s-1", "job-done"), session("s-2", "job-done")],
    },
  });
  await page.goto("/");
  await openJobs(page);
  const gate = page.getByTestId("job-delete-gate-job-done");
  await expect(gate).toHaveAttribute("data-gate", "confirm");
  await page.getByTestId("delete-job-job-done").click();
  await expect(gate).toContainText(msg("job.deleteQuestion", { count: 2 }));
  const confirm = page.getByTestId("confirm-delete-job-job-done");
  await expect(confirm).toHaveText(msg("job.deleteConfirm", { count: 2 }));
  await expect(confirm).toBeFocused();
  await page.getByTestId("cancel-delete-job-job-done").click();
  await expect(page.getByTestId("job-row-job-done")).toBeVisible();
  expect(await deleteCalls(page)).toBe(0);

  await page.getByTestId("delete-job-job-done").click();
  await page.getByTestId("confirm-delete-job-job-done").click();
  await expect(page.getByTestId("job-row-job-done")).toHaveCount(0);
  expect(await deleteCalls(page)).toBe(1);
});

test("J7.4 / D10: a cast whose tutor session is live cannot be deleted", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      seededJobs: [seededJob("job-live", "DONE", Date.now() - 10_000)],
      liveJobIds: ["job-live"],
    },
    voiceSessionsScenario: { rows: [session("s-live", "job-live")] },
  });
  await page.goto("/");
  await openJobs(page);
  await expect(page.getByTestId("job-delete-gate-job-live")).toHaveAttribute(
    "data-gate",
    "refused",
  );
  const trigger = page.getByTestId("delete-job-job-live");
  await expect(trigger).toBeDisabled();
  await expect(trigger).toHaveAttribute("title", msg("job.deleteLiveTitle"));
  await trigger.click({ force: true });
  await expect(page.getByTestId("confirm-delete-job-job-live")).toHaveCount(0);
  await expect(page.getByTestId("job-row-job-live")).toBeVisible();
  expect(await deleteCalls(page)).toBe(0);
});

test("C9: queued casts state their order, and the order survives a restart", async ({
  page,
}) => {
  const now = Date.now();
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      persistJobs: true,
      seededJobs: [
        seededJob("job-third", "QUEUED", now - 1_000),
        seededJob("job-first", "QUEUED", now - 3_000),
        seededJob("job-second", "QUEUED", now - 2_000),
        seededJob("job-running", "RENDERING", now - 5_000),
      ],
    },
  });
  await page.goto("/");
  await openJobs(page);
  const expectOrder = async () => {
    await expect(page.getByTestId("job-queue-position-job-first")).toHaveText(
      msg("job.queueNext"),
    );
    await expect(page.getByTestId("job-queue-position-job-second")).toHaveText(
      msg("job.queueAhead", { count: 1 }),
    );
    await expect(page.getByTestId("job-queue-position-job-third")).toHaveText(
      msg("job.queueAhead", { count: 2 }),
    );
    await expect(
      page.getByTestId("job-queue-position-job-running"),
    ).toHaveCount(0);
  };
  await expectOrder();
  // Mutate the queue before the restart so a re-seed from the fixture cannot
  // pass for persistence: cancel the newest queued cast.
  await page.getByTestId("cancel-job-job-third").click();
  await expect(page.getByTestId("job-row-job-third")).toContainText(
    msg("job.cancelled"),
  );
  await expect(page.getByTestId("job-queue-position-job-third")).toHaveCount(0);

  // "Restart": the document reloads and the mock rehydrates its rows from the
  // persisted store (the SQLite role); the order and the cancellation must
  // both come back.
  await page.reload();
  await openJobs(page);
  await expect(page.getByTestId("job-queue-position-job-first")).toHaveText(
    msg("job.queueNext"),
  );
  await expect(page.getByTestId("job-queue-position-job-second")).toHaveText(
    msg("job.queueAhead", { count: 1 }),
  );
  await expect(page.getByTestId("job-row-job-third")).toContainText(
    msg("job.cancelled"),
  );
  await expect(page.getByTestId("job-queue-position-job-third")).toHaveCount(0);
});

test("C10: a queued cast can be cancelled; it settles and leaves the queue order to the rest", async ({
  page,
}) => {
  const now = Date.now();
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      seededJobs: [
        seededJob("job-first", "QUEUED", now - 3_000),
        seededJob("job-second", "QUEUED", now - 2_000),
      ],
    },
  });
  await page.goto("/");
  await openJobs(page);
  await expect(page.getByTestId("job-queue-position-job-second")).toHaveText(
    msg("job.queueAhead", { count: 1 }),
  );
  await page.getByTestId("cancel-job-job-first").click();
  await expect(page.getByTestId("job-row-job-first")).toContainText(
    msg("job.cancelled"),
  );
  await expect(page.getByTestId("job-queue-position-job-first")).toHaveCount(0);
  await expect(page.getByTestId("job-queue-position-job-second")).toHaveText(
    msg("job.queueNext"),
  );
  // A settled cast is deletable at once (zero sessions → D9 path).
  await expect(page.getByTestId("job-delete-gate-job-first")).toHaveAttribute(
    "data-gate",
    "immediate",
  );
});

test("C9 tie: casts created in the same millisecond display in the order the worker restores", async ({
  page,
}) => {
  // Source of truth is the store's (created_at, id) key: the runtime lists and
  // restores in that order; the panel shows newest first, so its DOM order is
  // that key reversed and every position label must agree with it.
  const stamp = Date.now() - 5_000;
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      seededJobs: [
        seededJob("job-a", "QUEUED", stamp),
        seededJob("job-b", "QUEUED", stamp),
      ],
    },
  });
  await page.goto("/");
  await openJobs(page);
  await expect(page.getByTestId("job-queue-position-job-a")).toHaveText(
    msg("job.queueNext"),
  );
  const domOrder = await page.evaluate(() =>
    Array.from(
      document.querySelectorAll<HTMLElement>(
        '[data-testid^="job-queue-position-"]',
      ),
    ).map((el) => el.dataset.testid),
  );
  expect(domOrder).toEqual([
    "job-queue-position-job-b",
    "job-queue-position-job-a",
  ]);
});

// C17 (V1-CLOSEOUT): a cast resumed from pause keeps its creation-order slot.
// The mock mirrors the runtime: `resume_job` → QUEUED, and nothing starts
// while another cast is running, so the resumed cast waits where its
// `createdAt` puts it — before the cast created after it.
test("C17: a resumed cast keeps its creation-order slot, and the slot survives a restart", async ({
  page,
}) => {
  const now = Date.now();
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      persistJobs: true,
      seededJobs: [
        seededJob("job-running", "RENDERING", now - 5_000),
        seededJob("job-resumed", "PAUSED", now - 3_000),
        seededJob("job-later", "QUEUED", now - 2_000),
      ],
    },
  });
  await page.goto("/");
  await openJobs(page);
  await expect(page.getByTestId("job-queue-position-job-later")).toHaveText(
    msg("job.queueNext"),
  );
  await expect(page.getByTestId("job-queue-position-job-resumed")).toHaveCount(0);
  await page.getByTestId("resume-job-job-resumed").click();
  const expectSlots = async () => {
    await expect(page.getByTestId("job-queue-position-job-resumed")).toHaveText(
      msg("job.queueNext"),
    );
    await expect(page.getByTestId("job-queue-position-job-later")).toHaveText(
      msg("job.queueAhead", { count: 1 }),
    );
    await expect(page.getByTestId("job-queue-position-job-running")).toHaveCount(0);
  };
  await expectSlots();
  await page.reload();
  await openJobs(page);
  await expectSlots();
});
