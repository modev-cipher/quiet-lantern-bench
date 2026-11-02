import { expect, test } from "@playwright/test";

import {
  fixtureBook,
  fixtureCatalogModels,
  fixtureChapters,
  fixtureHardware,
  installTauriMock,
} from "./tauriMock";
import { castStep } from "./castSteps";
import { e2eLocale, fragment, msg, storeError } from "./i18nText";
import { OVERPROMISE, QUEUED_OVERNIGHT } from "./overnightCopy";

test("creates a job and follows it through the stepper to an exported mp3", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 250,
      outputPath: "/exports/fixture-book.mp3",
    },
  });
  await page.goto("/");

  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  const modal = page.getByTestId("job-modal");
  await expect(modal).toBeVisible();
  await expect(modal).toContainText("Fixture Book Title");

  await expect(modal).toContainText("Getting Started");
  await expect(page.getByTestId("chapter-check-ch-1")).toBeChecked();
  await expect(modal).toContainText(
    msg("cast.selectedCount", { selected: 3, total: 3 }),
  );

  await expect(page.getByTestId("eta-label")).toContainText("~");

  await page.getByTestId("start-job").click();
  await expect(modal).not.toBeVisible();

  const row = page.getByTestId(/^job-row-/);
  await expect(row).toBeVisible();
  await expect(row).toContainText("Fixture Book Title");
  await expect(row).toContainText("narrator");

  await expect(page.getByTestId("job-step-job-1-4")).toHaveAttribute(
    "data-status",
    "active",
    { timeout: 10_000 },
  );

  await expect
    .poll(
      async () => {
        const bar = page.getByTestId("job-progress-bar-job-1");
        if ((await bar.count()) === 0) return 0;
        const width = await bar.evaluate(
          (el) => (el as HTMLElement).style.width,
        );
        return Number.parseFloat(width);
      },
      { timeout: 10_000 },
    )
    .toBeGreaterThanOrEqual(30);

  await expect(page.getByTestId("job-output-job-1")).toContainText(
    "/exports/fixture-book.mp3",
    { timeout: 10_000 },
  );
});

test("supports page-range scope with validation", async ({ page }) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: { chapters: fixtureChapters() },
  });
  await page.goto("/");

  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("scope-pages-tab").click();

  await page.getByTestId("page-end-input").fill("99");
  await expect(page.getByTestId("page-range-error")).toContainText(
    msg("cast.range.pageCount", { count: 12 }),
  );
  await expect(page.getByTestId("start-job")).toBeDisabled();

  await page.getByTestId("page-end-input").fill("6");
  await expect(page.getByTestId("page-range-error")).not.toBeVisible();
  await expect(page.getByTestId("start-job")).toBeEnabled();
});

test("select-all toggle clears and restores the chapter selection", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: { chapters: fixtureChapters() },
  });
  await page.goto("/");

  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  const toggle = page.getByTestId("select-all-chapters");
  await expect(toggle).toBeChecked();
  await expect(toggle).toHaveAccessibleName(msg("cast.deselectAll"));

  await toggle.click();
  await expect(page.getByTestId("job-modal")).toContainText(
    msg("cast.selectedCount", { selected: 0, total: 3 }),
  );
  await expect(page.getByTestId("start-job")).toBeDisabled();

  await toggle.click();
  await expect(page.getByTestId("job-modal")).toContainText(
    msg("cast.selectedCount", { selected: 3, total: 3 }),
  );
  await expect(page.getByTestId("start-job")).toBeEnabled();
});

test("queue overnight creates a queued job", async ({ page }) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: { chapters: fixtureChapters() },
  });
  await page.goto("/");

  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "review");
  await page.getByTestId("queue-overnight").click();

  const row = page.getByTestId("job-row-job-1");
  await expect(row).toBeVisible();
  await expect(row).toContainText(msg("job.overnightBadge"));
  await expect(row).toContainText(msg("job.queuedOvernight"));
  await expect(row).toContainText(QUEUED_OVERNIGHT[e2eLocale]);
  await expect(row).not.toContainText(OVERPROMISE);
});

test("a cast queued behind a running one is not called overnight work", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      seededJobs: [
        {
          id: "job-running",
          bookId: "book-fixture",
          scope: { kind: "chapters", chapter_ids: ["ch-1"] },
          mode: "narrator",
          voiceProfileId: "preset-kokoro-af-heart",
          outputFormat: "mp3",
          modelOverridesJson: null,
          state: "RENDERING",
          overnight: false,
          etaSeconds: null,
          reportJson: null,
          outputPathsJson: null,
          createdAt: Date.now() - 1000,
          updatedAt: Date.now() - 1000,
        },
        {
          id: "job-waiting",
          bookId: "book-fixture",
          scope: { kind: "chapters", chapter_ids: ["ch-1"] },
          mode: "narrator",
          voiceProfileId: "preset-kokoro-af-heart",
          outputFormat: "mp3",
          modelOverridesJson: null,
          state: "QUEUED",
          overnight: false,
          etaSeconds: null,
          reportJson: null,
          outputPathsJson: null,
          createdAt: Date.now(),
          updatedAt: Date.now(),
        },
      ],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-jobs").click();
  await page.getByTestId("open-production").click();

  const waiting = page.getByTestId("job-queued-job-waiting");
  await expect(waiting).toHaveText(msg("job.waitingQueue"));
  await expect(page.getByTestId("job-row-job-waiting")).not.toContainText(
    "overnight batch",
  );
});

test("deletes a finished job from the panel", async ({ page }) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 50,
      outputPath: "/exports/fixture-book.mp3",
    },
  });
  await page.goto("/");

  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("start-job").click();

  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 10_000,
  });
  await expect(page.getByTestId("delete-job-job-1")).toBeVisible();
  await page.getByTestId("delete-job-job-1").click();
  await expect(page.getByTestId("job-row-job-1")).not.toBeVisible();
  await expect(page.getByTestId("jobs-panel")).toContainText(
    msg("job.noCastsYet"),
  );
});

test("surfaces create_job errors inside the modal", async ({ page }) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      createError: "voice profile 'missing' not found",
    },
  });
  await page.goto("/");

  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("start-job").click();

  const modal = page.getByTestId("job-modal");
  await expect(modal).toBeVisible();
  await expect(modal.getByRole("alert")).toContainText(
    storeError("voice profile 'missing' not found"),
  );
});

test("the cast's own model explains figures, and turning it off records the opt-out", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: { chapters: fixtureChapters() },
    modelScenario: {
      catalogModels: fixtureCatalogModels(),
      hardware: fixtureHardware(),
      installed: [
        { modelId: "kokoro-82m", diskBytes: 355_000_000 },
        { modelId: "gemma4-e4b-q4km", diskBytes: 6_000_000_000 },
      ],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");
  await page.getByTestId("mode-teacher").click();

  const toggle = page.getByTestId("cast-explain-figures");
  await expect(toggle).toBeChecked();
  await expect(page.getByTestId("figure-sublabel")).toContainText(
    fragment("cast.figuresExplainedBy", 1),
  );
  await toggle.uncheck();
  await expect(page.getByTestId("figure-sublabel")).toHaveText(
    msg("cast.skipFiguresHint"),
  );
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-modal")).not.toBeVisible();
  const calls = await page.evaluate(
    () =>
      (
        window as unknown as {
          __createJobCalls: Array<{ modelOverridesJson: string }>;
        }
      ).__createJobCalls,
  );
  const overrides = JSON.parse(calls[calls.length - 1]!.modelOverridesJson);
  expect(overrides.explain_figures).toBe(false);
  expect(overrides).not.toHaveProperty("vision_binding");
});

test("a cast without a picture-reading model says figures get a simple note", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: { chapters: fixtureChapters() },
    modelScenario: {
      catalogModels: fixtureCatalogModels(),
      hardware: fixtureHardware(),
      installed: [{ modelId: "kokoro-82m", diskBytes: 355_000_000 }],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");
  await page.getByTestId("mode-teacher").click();

  await expect(page.getByTestId("figure-sublabel")).toHaveText(
    msg("cast.figuresUnavailable"),
  );
});

test("voice picker groups voices by language with meta line and preview", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      voiceProfiles: [
        {
          id: "preset-kokoro-af-heart",
          name: "Heart · American English (female)",
          engine: "kokoro-82m",
          kind: "preset",
          presetVoiceId: "af_heart",
        },
        {
          id: "preset-kokoro-jm-kumo",
          name: "Kumo · Japanese (male)",
          engine: "kokoro-82m",
          kind: "preset",
          presetVoiceId: "jm_kumo",
        },
        {
          id: "preset-kokoro-zf-xiaobei",
          name: "Xiaobei · Chinese (female)",
          engine: "kokoro-82m",
          kind: "preset",
          presetVoiceId: "zf_xiaobei",
        },
      ],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");

  const select = page.getByTestId("voice-select");
  await expect(select.locator("optgroup")).toHaveCount(3);
  await expect(select.locator("optgroup").first()).toHaveAttribute(
    "label",
    `🇺🇸 ${msg("voice.lang.americanEnglish")}`,
  );
  // The whole meta line, gender included (the split assert let a
  // wrong-gender render pass).
  await expect(page.getByTestId("voice-meta")).toHaveText(
    msg("voice.meta.female", {
      flag: "🇺🇸",
      name: "Heart",
      language: msg("voice.lang.americanEnglish"),
    }),
  );

  await select.selectOption("preset-kokoro-jm-kumo");
  await expect(page.getByTestId("voice-meta")).toHaveText(
    msg("voice.meta.male", {
      flag: "🇯🇵",
      name: "Kumo",
      language: msg("voice.lang.japanese"),
    }),
  );
  await expect(page.getByTestId("voice-preview")).toBeVisible();
});

test("voice preview surfaces errors and allows retry", async ({ page }) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      previewError: "engine is not ready",
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");

  const preview = page.getByTestId("voice-preview");
  await preview.click();
  await expect(preview).toHaveAttribute("data-state", "error");
  await expect(preview).toContainText(msg("voice.preview.retry"));
});

test("previewing a local voice without its engine offers the setup instead of an error", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      previewError: "ERR_ENGINE_LOCAL_AI_REQUIRED:localAi",
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");

  const preview = page.getByTestId("voice-preview");
  await preview.click();
  await expect(page.getByTestId("engine-setup-sheet")).toBeVisible();
  await expect(preview).toHaveAttribute("data-state", "idle");
});

test("job modal keeps its steps beside the panel when wide and stacks them when narrow", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: { chapters: fixtureChapters() },
  });

  const directionAt = async (width: number) => {
    await page.setViewportSize({ width, height: 900 });
    const dialog = page.getByRole("dialog", {
      name: msg("cast.dialogLabel", { title: "Fixture Book Title" }),
    });
    await expect(dialog).toBeVisible();
    return dialog.evaluate((el) => getComputedStyle(el).flexDirection);
  };

  await page.setViewportSize({ width: 1362, height: 900 });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await expect(page.getByTestId("job-modal")).toBeVisible();

  expect(await directionAt(1362)).toBe("row");
  expect(await directionAt(800)).toBe("row");
  expect(await directionAt(700)).toBe("column");
});

test("a job paused on engine contention explains itself and how to clear it", async ({
  page,
}) => {
  const report = JSON.stringify({
    plan_revisions: 1,
    recovery_log: [
      { stage: "SCRIPTING", rev: 1, action: "gpu layers reduced 20%", at: 1 },
    ],
    contention: {
      episodes: 2,
      retries: 9,
      waited_s: 200,
      by_stage: { SCRIPTING: { episodes: 2, retries: 9, waited_s: 200 } },
      paused: {
        stage: "SCRIPTING",
        retries: 3,
        waited_s: 121,
        resume_attempts: 2,
        max_pauses: 3,
        at: 1783468800000,
      },
    },
  });
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      seededJobs: [
        {
          id: "job-paused",
          bookId: "book-fixture",
          scope: { kind: "chapters", chapter_ids: ["ch-1"] },
          mode: "narrator",
          voiceProfileId: "preset-kokoro-af-heart",
          outputFormat: "mp3",
          modelOverridesJson: null,
          state: "PAUSED",
          overnight: false,
          etaSeconds: null,
          reportJson: report,
          outputPathsJson: null,
          createdAt: Date.now(),
          updatedAt: Date.now(),
        },
      ],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-jobs").click();
  await page.getByTestId("open-production").click();

  await expect(page.getByTestId("job-row-job-paused")).toBeVisible();
  const execution = page.getByTestId("job-execution-job-paused");
  await expect(execution).toBeVisible();
  await expect(execution).toContainText(
    msg("job.pausedContention", {
      stage: msg("job.stage.script"),
      waited: 200,
      count: 9,
      attempt: msg("job.pauseOf", { attempts: 2, cap: 3 }),
    }),
  );
  await expect(execution).toContainText(
    msg("job.reducedHardwareAction", {
      count: 1,
      action: "gpu layers reduced 20%",
    }),
  );
});

test("the modal takes focus on open and returns it to the trigger on close", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 250,
      outputPath: "/exports/fixture-book.mp3",
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  const trigger = page.getByTestId("create-audio-book-fixture");
  await trigger.focus();
  await page.keyboard.press("Enter");

  const dialog = page.getByRole("dialog", {
    name: msg("cast.dialogLabel", { title: "Fixture Book Title" }),
  });
  await expect(dialog).toBeVisible();
  await expect(dialog).toBeFocused();

  await page.keyboard.press("Escape");
  await expect(page.getByTestId("job-modal")).not.toBeVisible();
  await expect(trigger).toBeFocused();
});

test("send to phone from a finished cast opens the shelf with the modal", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 30,
      outputPath: "/exports/fixture-book.mp3",
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 15_000,
  });
  await page.getByTestId("job-send-job-1").click();
  await expect(page.getByTestId("send-to-phone-modal")).toBeVisible();
  await expect(page.getByTestId("nav-audio")).toHaveAttribute(
    "aria-selected",
    "true",
  );
});
