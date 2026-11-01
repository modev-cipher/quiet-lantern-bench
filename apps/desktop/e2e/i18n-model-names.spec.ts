import { expect, test, type Page } from "@playwright/test";
import { castStep } from "./castSteps";
import { msg } from "./i18nText";

import {
  fixtureBook,
  fixtureCatalogModels,
  fixtureChapters,
  fixtureHardware,
  installTauriMock,
} from "./tauriMock";

// Locale-agnostic on purpose (runs green under I18N_E2E_LOCALE=ar too).
// #31 public-copy law: catalog ids, vendor names and parameter-count tokens
// never reach the DOM. The catalog below carries the exact ids the site lane
// saw rendered raw at 4a3c234c; the pin walks the new-cast dialog, the wizard
// teaching step and the Models panel.
const RAW = /teaching-model|figure-reader|studio-voice|\b\d+b\b|Qwen|Gemma|Kokoro|XTTS|q4km/i;

function catalogWithRawIds() {
  const models = fixtureCatalogModels();
  const llm = models.find((m) => m.task === "llm")!;
  const tts = models.find((m) => m.task === "tts")!;
  return [
    ...models,
    { ...llm, id: "teaching-model-9b", display_name: "Teaching model 9B", quality_score: 99 },
    { ...tts, id: "studio-voice", display_name: "Studio voice", quality_score: 99 },
  ];
}

async function assertClean(page: Page, testId: string) {
  const text = (await page.getByTestId(testId).textContent()) ?? "";
  expect(text, `${testId} leaks a raw id / vendor / size token`).not.toMatch(RAW);
  expect(text.length).toBeGreaterThan(20);
}

test("the new-cast dialog names what runs each step by role and outcome, never by id", async ({ page }) => {
  const catalogModels = catalogWithRawIds();
  await installTauriMock(page, {
    books: [fixtureBook()],
    modelScenario: {
      catalogModels,
      installed: catalogModels.map((m) => ({ modelId: m.id, diskBytes: 1 })),
      hardware: fixtureHardware(),
      downloadTicks: 1,
      downloadDelayMs: 10,
    },
    jobScenario: { chapters: fixtureChapters() },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await expect(page.getByTestId("job-modal")).toBeVisible();
  await castStep(page, "review");
  await expect(page.getByTestId("phase-summary")).toBeVisible();
  await assertClean(page, "phase-summary");
  await assertClean(page, "job-modal");
  await expect(page.getByTestId("phase-summary")).toContainText(/Teaching|Explains the book|التعليم|يشرح الكتاب/);
});

test("the Models panel and the wizard teaching step use the same names", async ({ page }) => {
  const catalogModels = catalogWithRawIds();
  await installTauriMock(page, {
    books: [],
    onboardingComplete: false,
    modelScenario: { catalogModels, hardware: fixtureHardware(), downloadTicks: 1, downloadDelayMs: 10 },
  });
  await page.goto("/");
  await page.waitForSelector('[data-testid="wizard-scan-card"]');
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("wizard-teacher").locator("summary").click();
  await expect(page.getByTestId("wizard")).toContainText(/Teacher mode|وضع المعلّم/);
  await assertClean(page, "wizard");
});

// AT-2-2 (round-2 QA, S3): the Library's scanned-import notice and the
// scanned refusal repeated the engine's advice verbatim, model name included.
// The engine line below is the real one (engine/ingest/manifest.py); the DOM
// must name the destination and the model's role only.
const LIBRARY_RAW = /Docling|docling-layout|Kokoro|Whisper|pdfium/i;

test("the Library scanned-import notice never repeats the engine's model name", async ({ page }) => {
  await installTauriMock(page, {
    books: [],
    importScenario: {
      path: "/fixtures/scanned.pdf",
      book: fixtureBook({ id: "book-scanned", isScanned: true, pageCount: 3 }),
      warnings: [
        "scanned_pages_unread: 3 of 3 page(s) are images with no text layer and were not read; install the 'Docling layout suite' model (docling-layout) to read them with OCR",
      ],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("library-import-book").click();
  await expect(page.getByTestId("book-card")).toHaveCount(1);
  const notice = page.getByRole("status");
  const scope = msg("library.notice.pagesOf", { done: 3, total: 3 });
  await expect(notice).toContainText(scope);
  await expect(notice).toContainText(
    msg("library.notice.scannedSkippedInstall", { scope }),
  );
  const text = (await notice.textContent()) ?? "";
  expect(text, "Library import notice leaks a model / engine name").not.toMatch(LIBRARY_RAW);
  expect(text).not.toMatch(RAW);
});

test("the Library scanned-import refusal never names the model either", async ({ page }) => {
  await installTauriMock(page, {
    books: [],
    importScenario: {
      path: "/fixtures/scanned.pdf",
      error: "ERR_SCANNED: this book is scanned images and no OCR model is installed",
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("library-import-book").click();
  const alert = page.getByRole("alert");
  await expect(alert).toContainText(msg("library.importError.scanned"));
  await expect(alert).toContainText("Models");
  const text = (await alert.textContent()) ?? "";
  expect(text, "Library import refusal leaks a model / engine name").not.toMatch(LIBRARY_RAW);
  expect(text).not.toMatch(RAW);
});
