import { test, type Page } from "@playwright/test";

import {
  fixtureBook,
  fixtureChapters,
  fixtureCatalogModels,
  fixtureHardware,
  installTauriMock,
  type Scenario,
} from "./tauriMock";

test.setTimeout(120_000);

test.skip(
  process.env.AUDIT_SHOTS !== "1",
  "baseline capture rig — the before-state shots are a committed record; set AUDIT_SHOTS=1 to re-run deliberately",
);

import { openReading, play, enterReading } from "./readingHarness";

const MODELS = {
  catalogModels: fixtureCatalogModels(),
  hardware: fixtureHardware(),
  downloadTicks: 2,
  downloadDelayMs: 60,
};

const OUT = "../../docs/design/audit-shots/before";
const WIDTHS = [
  { name: "1440", width: 1440, height: 900 },
  { name: "1100", width: 1100, height: 800 },
  { name: "800", width: 800, height: 720 },
];

async function shot(page: Page, name: string, widths = WIDTHS) {
  for (const w of widths) {
    await page.setViewportSize({ width: w.width, height: w.height });
    await page.waitForTimeout(250);
    await page.screenshot({ path: `${OUT}/${name}-${w.name}.png` });
  }
  await page.setViewportSize({ width: 1440, height: 900 });
}

async function dark(page: Page) {
  await page.evaluate(() => {
    document.documentElement.setAttribute("data-theme", "dark");
  });
  await page.waitForTimeout(150);
}

const BOOKS = [
  fixtureBook(),
  fixtureBook({
    id: "book-2",
    title: "A Second, Much Longer Book Title That Wraps",
    author: "Another Author",
    pageCount: 340,
    chapterCount: 18,
    createdAt: 2,
  }),
  fixtureBook({
    id: "book-3",
    title: "Arabic Grammar Primer",
    author: "M. Haroun",
    language: "ar",
    pageCount: 96,
    chapterCount: 7,
    createdAt: 3,
    isScanned: true,
  }),
];

const EXPORTS = [
  {
    id: "export-lan-1",
    bookId: "book-fixture",
    jobId: "job-lan-1",
    path: "/exports/Fixture Book - full - narrate.mp3",
    bytes: 5_000_000,
    format: "mp3",
    mode: "narrate",
    stale: false,
    createdAt: 1_755_000_000_000,
  },
  {
    id: "export-lan-2",
    bookId: "book-2",
    jobId: "job-lan-2",
    path: "/exports/Second Book - ch1-4 - narrate.mp3",
    bytes: 22_000_000,
    format: "mp3",
    mode: "narrate",
    stale: true,
    createdAt: 1_755_100_000_000,
  },
];

test("wizard onboarding steps", async ({ page }) => {
  await installTauriMock(page, {
    books: [],
    onboardingComplete: false,
    modelScenario: MODELS,
  });
  await page.goto("/");
  await page.waitForSelector('[data-testid="wizard-scan-card"]');
  await shot(page, "wizard-step1");
  await dark(page);
  await shot(page, "wizard-step1-dark", [WIDTHS[0]]);
  await page.getByTestId("wizard-next").click();
  await page.waitForTimeout(300);
  await shot(page, "wizard-step2-tts", [WIDTHS[0], WIDTHS[2]]);
  await page.getByTestId("wizard-teacher").locator("summary").click();
  await page.waitForTimeout(300);
  await shot(page, "wizard-step3-llm", [WIDTHS[0]]);
});

test("empty library", async ({ page }) => {
  await installTauriMock(page, { books: [] });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.waitForSelector("text=Your library is empty.");
  await shot(page, "library-empty");
  await dark(page);
  await shot(page, "library-empty-dark", [WIDTHS[0]]);
});

test("populated library and audios shelf", async ({ page }) => {
  await installTauriMock(page, {
    books: BOOKS,
    seededExports: EXPORTS,
    jobScenario: { chapters: fixtureChapters() },
  } as Scenario);
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.waitForSelector('[data-testid="book-card"]');
  await shot(page, "library-books");
  await dark(page);
  await shot(page, "library-books-dark", [WIDTHS[0]]);
  await page.evaluate(() =>
    document.documentElement.setAttribute("data-theme", "light"),
  );
  await page.getByTestId("nav-audio").click();
  await page.waitForTimeout(300);
  await shot(page, "library-audios");
  await page.getByTestId("send-to-phone").click();
  await page.waitForSelector('[data-testid="send-to-phone-modal"]');
  await shot(page, "send-to-phone", [WIDTHS[0], WIDTHS[2]]);
});

test("job modal states", async ({ page }) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 1500,
      outputPath: "/exports/fixture-book.mp3",
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.waitForSelector('[data-testid="job-modal"]');
  await shot(page, "job-modal", [WIDTHS[0], WIDTHS[2]]);
  await dark(page);
  await shot(page, "job-modal-dark", [WIDTHS[0]]);
  await page.evaluate(() =>
    document.documentElement.setAttribute("data-theme", "light"),
  );
  await page.getByTestId("start-job").click();
  await page.waitForTimeout(600);
  await shot(page, "jobs-running", [WIDTHS[0], WIDTHS[2]]);
  await page.waitForSelector('[data-testid="job-output-job-1"]', {
    timeout: 30_000,
  });
  await shot(page, "jobs-done", [WIDTHS[0]]);
});

test("models panel and settings", async ({ page }) => {
  await installTauriMock(page, { books: [], modelScenario: MODELS });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("nav-models").click();
  await page.waitForTimeout(500);
  await shot(page, "models");
  await dark(page);
  await shot(page, "models-dark", [WIDTHS[0]]);
  await page.evaluate(() =>
    document.documentElement.setAttribute("data-theme", "light"),
  );
  await page.getByTestId("nav-settings").click();
  await page.waitForTimeout(500);
  await shot(page, "settings");
  await dark(page);
  await shot(page, "settings-dark", [WIDTHS[0]]);
});

test("playback, mini player, reading, voice panel", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await shot(page, "library-mini-player");
  await enterReading(page);
  await page.waitForTimeout(600);
  await shot(page, "reading-view");
  await dark(page);
  await shot(page, "reading-view-dark", [WIDTHS[0]]);
});
