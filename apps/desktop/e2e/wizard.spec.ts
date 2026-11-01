import { expect, test } from "@playwright/test";

import { msg } from "./i18nText";

import {
  fixtureBook,
  fixtureCatalogModels,
  fixtureDevice,
  fixtureDevices,
  fixtureHardware,
  installTauriMock,
  NO_DEVICES,
  type MockHardwareProfile,
  type Scenario,
} from "./tauriMock";

function wizardScenario(
  hardware: MockHardwareProfile,
  overrides: Partial<Scenario> = {},
): Scenario {
  return {
    books: [],
    onboardingComplete: false,
    modelScenario: {
      catalogModels: fixtureCatalogModels(),
      hardware,
      downloadTicks: 2,
      downloadDelayMs: 60,
    },
    ...overrides,
  };
}

const noGpu = fixtureHardware(NO_DEVICES);
const fourGb = fixtureHardware({
  ...fixtureDevices([
    fixtureDevice({
      name: "RTX 3050",
      memoryTotalMb: 4_096,
      memoryFreeMb: 3_800,
    }),
  ]),
  ram: { totalMb: 16_384, freeMb: 8_192 },
});
const twelveGb = fixtureHardware();

test("12 GB profile recommends the best green models with reason lines", async ({
  page,
}) => {
  await installTauriMock(page, wizardScenario(twelveGb));
  await page.goto("/");

  const scan = page.getByTestId("wizard-scan-card");
  await expect(scan).toContainText("Fixture GPU · 12 GB VRAM");
  await expect(scan).toContainText(
    msg("wizard.ramCpuLine", { gb: 32, cores: 8 }),
  );
  await expect(scan).toContainText(msg("wizard.computeScore"));
  await page.getByTestId("wizard-next").click();

  await expect(page.getByTestId("wizard-recommended-kokoro-82m")).toBeVisible();
  await expect(page.getByTestId("wizard-reason-kokoro-82m")).toContainText(
    msg("wizard.reason.vram", { need: "1.2", have: "12" }),
  );
  await page.getByTestId("wizard-teacher").locator("summary").click();

  await expect(
    page.getByTestId("wizard-recommended-qwen3.5-9b-instruct-q4km"),
  ).toBeVisible();
});

test("4 GB profile shifts recommendations and keeps red models selectable", async ({
  page,
}) => {
  await installTauriMock(page, wizardScenario(fourGb));
  await page.goto("/");
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("wizard-teacher").locator("summary").click();

  await expect(
    page.getByTestId("wizard-recommended-qwen3.5-9b-instruct-q4km"),
  ).toBeVisible();
  const heavy = page.getByTestId("wizard-option-qwen3.5-32b-class-q4km");
  await expect(heavy.locator("[data-badge]")).toHaveAttribute(
    "data-badge",
    "red",
  );
  await heavy.click();
  await expect(heavy).toHaveAttribute("aria-pressed", "true");
});

test("no-GPU profile uses CPU copy and still recommends", async ({ page }) => {
  await installTauriMock(page, wizardScenario(noGpu));
  await page.goto("/");

  await expect(page.getByTestId("scan-gpu")).toContainText(msg("wizard.noGpu"));
  await page.getByTestId("wizard-next").click();

  await expect(page.getByTestId("wizard-recommended-kokoro-82m")).toBeVisible();
  await expect(page.getByTestId("wizard-reason-kokoro-82m")).toContainText(
    msg("wizard.reason.fallback"),
  );
  const option = page.getByTestId("wizard-option-kokoro-82m");
  await expect(option).toContainText(msg("fit.orange.detail"));
});

test("selections queue downloads, set defaults, and land in the library", async ({
  page,
}) => {
  await installTauriMock(page, wizardScenario(twelveGb));
  await page.goto("/");
  await page.getByTestId("wizard-next").click();

  await page.getByTestId("wizard-option-kokoro-82m").click();
  await page.getByTestId("wizard-teacher").locator("summary").click();
  await page.getByTestId("wizard-option-qwen3.5-9b-instruct-q4km").click();
  await page.getByTestId("wizard-next").click();

  const queue = page.getByTestId("wizard-download-queue");
  await expect(queue).toContainText(
    msg("wizard.queueSummary", { count: 2, size: "6.1 GB" }),
  );
  await expect(queue.getByTestId("wizard-queue-kokoro-82m")).toContainText(
    msg("wizard.installed"),
    { timeout: 10_000 },
  );

  await page.getByTestId("wizard-finish").click();
  await expect(page.getByTestId("library")).toBeVisible();

  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-models").click();
  await expect(
    page
      .getByTestId("installed-models")
      .getByTestId("model-default-kokoro-82m"),
  ).toBeVisible();
});

test("skip-all path completes without downloads", async ({ page }) => {
  await installTauriMock(page, wizardScenario(twelveGb));
  await page.goto("/");

  await page.getByTestId("wizard-skip-all").click();
  await expect(page.getByTestId("library")).toBeVisible();

  await page.reload();
  await expect(page.getByTestId("library")).toBeVisible();
  await expect(page.getByTestId("wizard")).not.toBeVisible();
});

test("failed download while offline shows the offline banner and wizard still completes", async ({
  page,
  context,
}) => {
  await installTauriMock(
    page,
    wizardScenario(twelveGb, {
      books: [],
      onboardingComplete: false,
      modelScenario: {
        catalogModels: fixtureCatalogModels(),
        hardware: twelveGb,
        downloadTicks: 1,
        downloadDelayMs: 30,
        downloadError: "connection refused",
      },
    }),
  );
  await page.goto("/");
  await context.setOffline(true);
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("wizard-option-kokoro-82m").click();
  await page.getByTestId("wizard-teacher").locator("summary").click();
  await page.getByTestId("wizard-next").click();

  await expect(page.getByTestId("wizard-offline-banner")).toContainText(
    msg("wizard.offline"),
  );
  await context.setOffline(false);
  await page.getByTestId("wizard-finish").click();
  await expect(page.getByTestId("library")).toBeVisible();
});

test("failed download while online shows a generic failure line, not the offline banner", async ({
  page,
}) => {
  await installTauriMock(
    page,
    wizardScenario(twelveGb, {
      books: [],
      onboardingComplete: false,
      modelScenario: {
        catalogModels: fixtureCatalogModels(),
        hardware: twelveGb,
        downloadTicks: 1,
        downloadDelayMs: 30,
        downloadError: "sha256 mismatch",
      },
    }),
  );
  await page.goto("/");
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("wizard-option-kokoro-82m").click();
  await page.getByTestId("wizard-teacher").locator("summary").click();
  await page.getByTestId("wizard-next").click();

  await expect(page.getByTestId("wizard-download-failures")).toContainText(
    msg("wizard.downloadsFailed"),
  );
  await expect(page.getByTestId("wizard-offline-banner")).not.toBeVisible();
  await page.getByTestId("wizard-finish").click();
  await expect(page.getByTestId("library")).toBeVisible();
});

test("combined downloads exceeding disk show the exact-numbers block", async ({
  page,
}) => {
  await installTauriMock(
    page,
    wizardScenario(fixtureHardware({ disk: { modelsFreeGb: 4.1 } })),
  );
  await page.goto("/");
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("wizard-teacher").locator("summary").click();
  await page.getByTestId("wizard-option-qwen3.5-9b-instruct-q4km").click();
  await page.getByTestId("wizard-next").click();

  await expect(page.getByTestId("wizard-disk-block")).toContainText(
    msg("models.diskBlock", { need: "5.7 GB", have: "4.1 GB" }),
  );

  await page.getByTestId("wizard-finish").click();
  await expect(page.getByTestId("library")).toBeVisible();
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-models").click();
  await expect(
    page.getByTestId("model-default-qwen3.5-9b-instruct-q4km"),
  ).not.toBeVisible();
});

test("Teacher mode is an option on the voice step, with no figure step or vision download", async ({
  page,
}) => {
  await installTauriMock(page, wizardScenario(twelveGb));
  await page.goto("/");
  await expect(page.getByTestId("wizard-steps").locator("li")).toHaveCount(3);
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("wizard-teacher").locator("summary").click();

  await page.getByTestId("wizard-option-gemma4-e4b-q4km").click();
  await page.getByTestId("wizard-next").click();

  await expect(page.getByTestId("wizard-download-queue")).toBeVisible();
  await expect(page.getByTestId("wizard-queue-gemma4-e4b-q4km")).toBeVisible();
  await expect(page.getByTestId("wizard-download-queue")).not.toContainText(
    /figure specialist/i,
  );
});

test("wizard is skipped entirely when onboarding is already complete", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    onboardingComplete: true,
    modelScenario: {
      catalogModels: fixtureCatalogModels(),
      hardware: twelveGb,
    },
  });
  await page.goto("/");
  await expect(page.getByTestId("library")).toBeVisible();
  await expect(page.getByTestId("wizard")).not.toBeVisible();
});

test("first-run setup carries the same brand mark as the app's bar", async ({
  page,
}) => {
  await installTauriMock(page, wizardScenario(twelveGb));
  await page.goto("/");
  const wizard = page.getByTestId("wizard");
  await expect(wizard).toBeVisible();
  const mark = wizard.getByTestId("brand-mark");
  await expect(mark).toBeVisible();
  await expect(mark.getByRole("img", { name: "txtCaster" })).toBeVisible();
});

test("the welcome says what the app does and where it runs", async ({
  page,
}) => {
  await installTauriMock(page, wizardScenario(twelveGb));
  await page.goto("/");
  const wizard = page.getByTestId("wizard");
  await expect(wizard.getByRole("heading", { level: 2 })).toHaveText(
    msg("wizard.welcome.title"),
  );
  await expect(page.getByTestId("wizard-welcome")).toContainText(
    msg("wizard.welcome.offline"),
  );
  await expect(page.getByTestId("wizard-scan-card")).toBeVisible();
});

test("choosing a book on the shelf step finishes setup and puts the book on the shelf", async ({
  page,
}) => {
  await installTauriMock(
    page,
    wizardScenario(twelveGb, {
      importScenario: {
        path: "/fixtures/First Book.pdf",
        book: fixtureBook({ id: "book-first", title: "First Book" }),
      },
    }),
  );
  await page.goto("/");
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("wizard-next").click();
  await expect(page.getByTestId("wizard-drop")).toBeVisible();
  await page.getByTestId("wizard-drop").click();
  await expect(page.getByTestId("wizard")).toHaveCount(0);
  await expect(page.getByTestId("library")).toBeVisible();
  await expect(page.getByTestId("open-book-book-first")).toBeVisible();
});

test("a file picker that fails says so on the book step and leaves setup usable", async ({
  page,
}) => {
  const pageErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));
  await installTauriMock(
    page,
    wizardScenario(twelveGb, {
      importScenario: { pickerError: "File picker unavailable" },
    }),
  );
  await page.goto("/");
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("wizard-drop").click();
  await expect(page.getByTestId("wizard-picker-error")).toContainText(
    msg("wizard.book.pickerFailed"),
  );
  await expect(page.getByTestId("wizard-book")).toBeVisible();
  expect(pageErrors, "the failure is handled, not thrown").toEqual([]);
  await page.getByTestId("wizard-finish").click();
  await expect(page.getByTestId("library")).toBeVisible();
});

test("cancelling the file picker on the book step changes nothing", async ({
  page,
}) => {
  await installTauriMock(page, wizardScenario(twelveGb));
  await page.goto("/");
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("wizard-drop").click();
  await expect(page.getByTestId("wizard-book")).toBeVisible();
  await expect(page.getByTestId("wizard-picker-error")).toHaveCount(0);
});
