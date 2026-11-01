import { expect, test } from "@playwright/test";

import { castStep } from "./castSteps";
import { e2eLocale, fragment, msg } from "./i18nText";
import { OVERNIGHT_HINT, OVERPROMISE, QUEUED_OVERNIGHT } from "./overnightCopy";

import {
  fixtureBook,
  fixtureCatalogModels,
  fixtureChapters,
  fixtureDevice,
  fixtureDevices,
  fixtureHardware,
  installTauriMock,
  NO_DEVICES,
  type MockHardwareProfile,
  type Scenario,
} from "./tauriMock";

function warningScenario(hardware: MockHardwareProfile): Scenario {
  return {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 120,
      outputPath: "/exports/fixture-book.mp3",
    },
    modelScenario: {
      catalogModels: fixtureCatalogModels(),
      hardware,
      installed: [
        { modelId: "kokoro-82m", diskBytes: 355_000_000 },
        { modelId: "qwen3.5-9b-instruct-q4km", diskBytes: 5_700_000_000 },
      ],
    },
  };
}

const fourGbGpu = fixtureHardware({
  ...fixtureDevices([
    fixtureDevice({
      name: "RTX 3050",
      memoryTotalMb: 4_096,
      memoryFreeMb: 4_000,
    }),
  ]),
  ram: { totalMb: 16_384, freeMb: 12_000 },
});

const noGpu = fixtureHardware({
  ...NO_DEVICES,
  ram: { totalMb: 16_384, freeMb: 12_000 },
});

const tinyMachine = fixtureHardware({
  ...NO_DEVICES,
  ram: { totalMb: 8_192, freeMb: 4_096 },
});

test("green fit shows no warning strip", async ({ page }) => {
  await installTauriMock(page, warningScenario(fixtureHardware()));
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();

  await expect(page.getByTestId("job-modal")).toBeVisible();
  await expect(page.getByTestId("fit-warning-strip")).not.toBeVisible();
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-modal")).not.toBeVisible();
});

test("yellow fit shows the eta warning strip and continue starts the job", async ({
  page,
}) => {
  await installTauriMock(page, warningScenario(fourGbGpu));
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");
  await page.getByTestId("mode-teacher").click();

  const strip = page.getByTestId("fit-warning-strip");
  await expect(strip).toBeVisible();
  await expect(strip).toHaveAttribute("data-badge", "yellow");
  await expect(page.getByTestId("fit-warning-eta")).toContainText(
    fragment("fit.eta", 0),
  );

  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-modal")).not.toBeVisible();
  await expect(page.getByTestId(/^job-row-/)).toBeVisible();
});

test("orange fit pre-selects overnight as the primary action", async ({
  page,
}) => {
  await installTauriMock(page, warningScenario(noGpu));
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();

  const strip = page.getByTestId("fit-warning-strip");
  await expect(strip).toBeVisible();
  await expect(strip).toHaveAttribute("data-badge", "orange");

  // ONBOARDING_UX §2 canonical 🟠 copy, and what "pre-selected" means.
  await expect(page.getByTestId("fit-warning-detail")).toHaveText(
    msg("fit.orange.detail"),
  );
  await expect(page.getByTestId("fit-overnight-hint")).toContainText(
    msg("fit.orange.overnightHint"),
  );
  // Discriminating: the approved sentence itself (an attempt while rendering, not a guarantee).
  await expect(page.getByTestId("fit-overnight-hint")).toHaveText(
    OVERNIGHT_HINT[e2eLocale],
  );
  await expect(page.getByTestId("fit-overnight-hint")).not.toContainText(
    OVERPROMISE,
  );

  await castStep(page, "review");
  const overnight = page.getByTestId("queue-overnight");
  const start = page.getByTestId("start-job");
  await expect(overnight).toHaveAttribute("data-primary", "true");
  await expect(start).toHaveAttribute("data-primary", "false");
  await expect(overnight).toHaveAttribute("data-variant", "solid");
  await expect(start).not.toHaveAttribute("data-variant", "solid");

  await overnight.click();
  await expect(page.getByTestId("job-modal")).not.toBeVisible();
  const row = page.getByTestId(/^job-row-/);
  await expect(row).toBeVisible();
  await expect(row).toContainText(msg("job.overnightBadge"));
  // The queue notice says what overnight does (the machine stays awake); there
  // is no "overnight batch" in the worker and the copy no longer promises one.
  await expect(row).toContainText(msg("job.queuedOvernight"));
  await expect(row).toContainText(QUEUED_OVERNIGHT[e2eLocale]);
  await expect(row).not.toContainText(OVERPROMISE);
  await expect(row).not.toContainText("batch");
});

test("yellow and green fits leave both start buttons ordinary (cast is primary)", async ({
  page,
}) => {
  await installTauriMock(page, warningScenario(fourGbGpu));
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");
  await page.getByTestId("mode-teacher").click();
  await expect(page.getByTestId("fit-warning-strip")).toHaveAttribute(
    "data-badge",
    "yellow",
  );
  await expect(page.getByTestId("fit-warning-detail")).toHaveText(
    msg("fit.yellow.detail"),
  );
  await expect(page.getByTestId("fit-overnight-hint")).toHaveCount(0);
  await castStep(page, "review");
  await expect(page.getByTestId("start-job")).toHaveAttribute(
    "data-primary",
    "true",
  );
  await expect(page.getByTestId("queue-overnight")).toHaveAttribute(
    "data-primary",
    "false",
  );
});

test("red fit requires the confirm dialog before starting", async ({
  page,
}) => {
  await installTauriMock(page, warningScenario(tinyMachine));
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");
  await page.getByTestId("mode-teacher").click();

  const strip = page.getByTestId("fit-warning-strip");
  await expect(strip).toBeVisible();
  await expect(strip).toHaveAttribute("data-badge", "red");
  await expect(page.getByTestId("fit-red-copy")).toContainText(
    fragment("fit.red.inline", 1),
  );

  await page.getByTestId("start-job").click();
  const dialog = page.getByTestId("red-confirm-dialog");
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText(msg("fit.red.confirm.body"));

  await expect(page.getByTestId("red-confirm-choose-another")).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(dialog).not.toBeVisible();
  await expect(page.getByTestId("job-modal")).toBeVisible();

  await page.getByTestId("start-job").click();
  await expect(dialog).toBeVisible();
  await page.getByTestId("red-confirm-choose-another").click();
  await expect(dialog).not.toBeVisible();
  await expect(page.getByTestId("job-modal")).toBeVisible();
  await expect(page.getByTestId(/^job-row-/)).not.toBeVisible();

  await page.getByTestId("start-job").click();
  await page.getByTestId("red-confirm-run-anyway").click();
  await expect(page.getByTestId("job-modal")).not.toBeVisible();
  await expect(page.getByTestId(/^job-row-/)).toBeVisible();
});

test("start only becomes enabled once the fit warning is resolved", async ({
  page,
}) => {
  await installTauriMock(page, warningScenario(tinyMachine));
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");
  await page.getByTestId("mode-teacher").click();

  await expect(page.getByTestId("start-job")).toBeEnabled();
  await expect(page.getByTestId("fit-warning-strip")).toHaveAttribute(
    "data-badge",
    "red",
  );
});

test("pick faster model closes the modal and lands on the models view", async ({
  page,
}) => {
  await installTauriMock(page, warningScenario(noGpu));
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();

  await page.getByTestId("pick-faster-model").click();
  await expect(page.getByTestId("job-modal")).not.toBeVisible();
  await expect(page.getByTestId("nav-models")).toHaveAttribute(
    "aria-current",
    "page",
  );
  await expect(page.getByTestId("model-row-kokoro-82m").first()).toBeVisible();
});

test("red fit cannot be bypassed from the keyboard on either start button", async ({
  page,
}) => {
  await installTauriMock(page, warningScenario(tinyMachine));
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");
  await page.getByTestId("mode-teacher").click();
  await expect(page.getByTestId("fit-warning-strip")).toHaveAttribute(
    "data-badge",
    "red",
  );
  // §2: the culprit is named by role (never a vendor id), then what is free.
  await expect(page.getByTestId("fit-red-copy")).toContainText(
    fragment("fit.red.inline", 1),
  );
  await expect(page.getByTestId("fit-red-copy")).not.toContainText("qwen");
  await expect(page.getByTestId("fit-red-copy")).not.toContainText("kokoro");

  const dialog = page.getByTestId("red-confirm-dialog");
  const noJob = page.getByTestId(/^job-row-/);

  // Enter on the primary start button opens the confirm, starts nothing.
  await page.getByTestId("start-job").focus();
  await page.keyboard.press("Enter");
  await expect(dialog).toBeVisible();
  // Enter again lands on the focused "Choose another": the dialog closes, still nothing.
  await expect(page.getByTestId("red-confirm-choose-another")).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(dialog).not.toBeVisible();
  await expect(page.getByTestId("job-modal")).toBeVisible();
  await expect(noJob).toHaveCount(0);

  // Space on the overnight button is gated the same way.
  await castStep(page, "review");
  await page.getByTestId("queue-overnight").focus();
  await page.keyboard.press("Space");
  await expect(dialog).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog).not.toBeVisible();
  await expect(noJob).toHaveCount(0);

  // Programmatic clicks that ignore the confirm layer's backdrop (a script, a
  // stuck key repeat) still hit the gate: the dialog opens, nothing starts.
  for (let i = 0; i < 3; i++) {
    await page.getByTestId("start-job").dispatchEvent("click");
  }
  await expect(dialog).toBeVisible();
  await expect(noJob).toHaveCount(0);

  // The explicit confirm is the only way in, and it carries the intent the
  // user chose (overnight here) into the job row.
  await page.keyboard.press("Escape");
  await page.getByTestId("queue-overnight").click();
  await expect(dialog).toBeVisible();
  await page.getByTestId("red-confirm-run-anyway").click();
  await expect(page.getByTestId("job-modal")).not.toBeVisible();
  await expect(noJob).toBeVisible();
  await expect(noJob).toContainText(msg("job.overnightBadge"));
});

// F2: the 🔴 confirm is bound to the fit it was given for and spent by use.
function replayScenario(): Scenario {
  const badges: Record<string, "green" | "orange" | "red"> = {
    "kokoro-82m": "green",
    "gemma4-e4b-q4km": "orange",
  };
  return {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 120,
      outputPath: "/exports/fixture-book.mp3",
      createErrorOnce: "engine busy — try again",
    },
    modelScenario: {
      catalogModels: fixtureCatalogModels().map((model) =>
        badges[model.id]
          ? {
              ...model,
              badge: badges[model.id],
              ...(model.id === "gemma4-e4b-q4km" ? { vision_badge: "red" } : {}),
            }
          : model,
      ),
      hardware: fixtureHardware(),
      installed: [
        { modelId: "kokoro-82m", diskBytes: 355_000_000 },
        { modelId: "gemma4-e4b-q4km", diskBytes: 6_000_000_000 },
      ],
    },
  };
}

test("a red confirm is spent by the attempt: a failed start re-asks on Cast and on overnight", async ({
  page,
}) => {
  await installTauriMock(page, replayScenario());
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");
  await page.getByTestId("mode-teacher").click();
  const strip = page.getByTestId("fit-warning-strip");
  await expect(strip).toHaveAttribute("data-badge", "red");
  const dialog = page.getByTestId("red-confirm-dialog");
  const modal = page.getByTestId("job-modal");
  const noJob = page.getByTestId(/^job-row-/);

  await page.getByTestId("start-job").click();
  await expect(dialog).toBeVisible();
  await page.getByTestId("red-confirm-run-anyway").click();
  await expect(
    modal.getByRole("alert").filter({ hasText: "engine busy" }),
  ).toBeVisible();
  await expect(noJob).toHaveCount(0);

  // Retry with the keyboard on Cast: the spent confirm does not carry over.
  await page.getByTestId("start-job").focus();
  await page.keyboard.press("Enter");
  await expect(dialog).toBeVisible();
  await expect(noJob).toHaveCount(0);
  await page.keyboard.press("Escape");

  // Retry with the keyboard on Queue overnight: same gate.
  await castStep(page, "review");
  await page.getByTestId("queue-overnight").focus();
  await page.keyboard.press("Space");
  await expect(dialog).toBeVisible();
  await expect(noJob).toHaveCount(0);
  await page.keyboard.press("Escape");
});

test("a red confirm does not survive a fit round-trip (red → orange → red)", async ({
  page,
}) => {
  await installTauriMock(page, replayScenario());
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");
  await page.getByTestId("mode-teacher").click();
  const strip = page.getByTestId("fit-warning-strip");
  const dialog = page.getByTestId("red-confirm-dialog");
  const noJob = page.getByTestId(/^job-row-/);
  await expect(strip).toHaveAttribute("data-badge", "red");

  await page.getByTestId("start-job").click();
  await page.getByTestId("red-confirm-run-anyway").click();
  await expect(
    page
      .getByTestId("job-modal")
      .getByRole("alert")
      .filter({ hasText: "engine busy" }),
  ).toBeVisible();

  await page.getByTestId("cast-explain-figures").uncheck();
  await expect(strip).toHaveAttribute("data-badge", "orange");
  await page.getByTestId("cast-explain-figures").check();
  await expect(strip).toHaveAttribute("data-badge", "red");

  await page.getByTestId("start-job").click();
  await expect(dialog).toBeVisible();
  await expect(noJob).toHaveCount(0);

  // The explicit confirm is still the way in, and now the start succeeds.
  await page.getByTestId("red-confirm-run-anyway").click();
  await expect(page.getByTestId("job-modal")).not.toBeVisible();
  await expect(noJob).toBeVisible();
});

test("a deprecated LLM that is the only install still needs the red confirm", async ({
  page,
}) => {
  const scenario = warningScenario(tinyMachine);
  scenario.modelScenario!.catalogModels = fixtureCatalogModels().map((m) =>
    m.id === "qwen3.5-9b-instruct-q4km" ? { ...m, deprecated: true } : m,
  );
  await installTauriMock(page, scenario);
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await castStep(page, "sound");
  await page.getByTestId("mode-teacher").click();

  await expect(page.getByTestId("fit-warning-strip")).toHaveAttribute(
    "data-badge",
    "red",
  );
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("red-confirm-dialog")).toBeVisible();
  await expect(page.getByTestId("job-modal")).toBeVisible();
});
