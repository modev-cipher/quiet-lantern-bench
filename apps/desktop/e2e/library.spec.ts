import { expect, test } from "@playwright/test";

import { fragment, msg } from "./i18nText";
import { fixtureBook, fixtureChapters, installTauriMock } from "./tauriMock";

test("import fixture PDF shows a card with correct metadata", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    importScenario: {
      path: "/fixtures/Fixture Book.pdf",
      progress: [
        { type: "progress", page: 5, total: 12 },
        { type: "progress", page: 12, total: 12 },
      ],
      progressDelayMs: 250,
      book: fixtureBook(),
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await expect(page.getByText(msg("library.empty"))).toBeVisible();
  await page.getByTestId("library-import-book").click();
  await expect(
    page.getByRole("button", {
      name: new RegExp(`${fragment("library.readingPages", 0)} \\d+/12…`),
    }),
  ).toBeVisible();

  const card = page.getByTestId("book-card");
  await expect(card).toHaveCount(1);
  await expect(card.getByTestId("book-card-title")).toHaveText(
    "Fixture Book Title",
  );
  await expect(card.getByText("Fixture Author").first()).toBeVisible();
  await page.getByTestId("book-menu-book-fixture").click();
  await expect(page.getByTestId("book-menu-details-book-fixture")).toHaveText(msg("library.pageCount", { count: 12 }) + " · " + msg("library.chapterCount", { count: 3 }));
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("library-import-book")).toBeEnabled();
});

test("scanned import surfaces a dismissible OCR notice", async ({ page }) => {
  await installTauriMock(page, {
    books: [],
    importScenario: {
      path: "/fixtures/Scanned Book.pdf",
      book: fixtureBook({ id: "book-scanned", isScanned: true }),
      warnings: [
        "scanned_pdf_ocr: text extracted with OCR; accuracy may vary",
        "multi_column_upgraded: [2]",
      ],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await page.getByTestId("library-import-book").click();
  await expect(page.getByTestId("book-card")).toHaveCount(1);
  const notice = page.getByRole("status");
  await expect(notice).toContainText(msg("library.notice.scannedOcr"));
  await expect(notice).toContainText(msg("library.notice.multiColumn"));
  await notice.getByRole("button", { name: msg("library.dismissNotice") }).click();
  await expect(page.getByRole("status")).toHaveCount(0);
});

test("existing books render on launch with pseudo-chapter wording", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [
      fixtureBook(),
      fixtureBook({
        id: "book-pseudo",
        title: "Headingless Book",
        author: null,
        pageCount: 40,
        chapterCount: 4,
        hasPseudoChapters: true,
      }),
    ],
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await expect(page.getByTestId("book-card")).toHaveCount(2);
  await page.getByTestId("book-menu-book-pseudo").click();
  await expect(page.getByTestId("book-menu-details-book-pseudo")).toHaveText(msg("library.pageCount", { count: 40 }) + " · " + msg("library.sectionCount", { count: 4 }));
});

test("delete removes the card after confirmation", async ({ page }) => {
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  const card = page.getByTestId("book-card");
  await expect(card).toHaveCount(1);
  await page.getByTestId("book-menu-book-fixture").click();
  await page
    .getByRole("menuitem", { name: msg("library.deleteBookTitle") })
    .click();
  await card
    .getByRole("button", { name: msg("library.confirmDelete") })
    .click();
  await expect(card).toHaveCount(0);
  await expect(page.getByText(msg("library.empty"))).toBeVisible();
});

test("DRM import shows refusal copy without adding a card", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    importScenario: {
      path: "/fixtures/protected.pdf",
      error: "ERR_DRM: document is encrypted (DRM) and cannot be ingested",
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await page.getByTestId("library-import-book").click();
  const alert = page.getByRole("alert");
  await expect(alert).toContainText(msg("library.importError.drm"));
  await expect(page.getByTestId("book-card")).toHaveCount(0);
  await alert.getByRole("button", { name: msg("library.dismissError") }).click();
  await expect(alert).toHaveCount(0);
});

test("cancelling the file picker leaves the library untouched", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    importScenario: { path: null },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await page.getByTestId("library-import-book").click();
  await expect(page.getByText(msg("library.empty"))).toBeVisible();
  await expect(page.getByRole("alert")).toHaveCount(0);
});

test("book grid column count follows the container width, not a breakpoint", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: Array.from({ length: 8 }, (_, index) =>
      fixtureBook({ id: `book-${index}`, title: `Book ${index}` }),
    ),
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await expect(page.getByTestId("book-card")).toHaveCount(8);

  // Mirrors the shelf grid in index.css (166px column + 14px gap); loosen this
  // only by widening the track there first, never by raising the number here.
  const MIN_TRACK_PX = 180;

  const measureAt = async (width: number) => {
    await page.setViewportSize({ width, height: 900 });
    const grid = page.getByTestId("book-grid").locator(".shelf-grid").first();
    await expect(grid).toBeVisible();
    return grid.evaluate((el) => ({
      columns: getComputedStyle(el).gridTemplateColumns.trim().split(/\s+/)
        .length,
      width: el.clientWidth,
    }));
  };

  const narrow = await measureAt(700);
  const medium = await measureAt(1100);
  const wide = await measureAt(1600);

  expect(narrow.columns).toBeLessThan(medium.columns);
  expect(medium.columns).toBeLessThanOrEqual(wide.columns);
  expect(narrow.columns).toBeGreaterThanOrEqual(1);

  for (const [name, seen] of [
    ["narrow", narrow],
    ["medium", medium],
    ["wide", wide],
  ] as const) {
    expect(
      seen.columns,
      `${name}: the track count must follow the measured container, not a pinned number that rots the next time the shell is resized`,
    ).toBeLessThanOrEqual(Math.max(1, Math.floor(seen.width / MIN_TRACK_PX)));
  }
});

test("the grid column count follows the measured container as it grows", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: Array.from({ length: 8 }, (_, index) =>
      fixtureBook({ id: `book-${index}`, title: `Book ${index}` }),
    ),
  });
  await page.setViewportSize({ width: 1000, height: 900 });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await expect(page.getByTestId("book-card")).toHaveCount(8);

  const grid = page.getByTestId("book-grid").locator(".shelf-grid").first();
  const columns = () =>
    grid.evaluate(
      (el) =>
        getComputedStyle(el).gridTemplateColumns.trim().split(/\s+/).length,
    );

  const before = await columns();
  await page.setViewportSize({ width: 1400, height: 900 });
  await expect.poll(columns, { timeout: 3000 }).toBeGreaterThan(before);
});

test("finished audio survives deleting its job from the queue", async ({
  page,
}) => {
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

  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 10_000,
  });

  await page.getByTestId("delete-job-job-1").click();
  await expect(page.getByTestId("job-row-job-1")).not.toBeVisible();

  await page.getByTestId("nav-library").click();
  await page.getByTestId("nav-audio").click();
  await expect(page.getByTestId("audio-card-export-job-1")).toBeVisible();
});

test("deleting an export removes it from the shelf", async ({ page }) => {
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

  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 10_000,
  });

  await page.getByTestId("nav-library").click();
  await page.getByTestId("nav-audio").click();
  await expect(page.getByTestId("audio-card-export-job-1")).toBeVisible();
  await page.getByTestId("delete-audio-export-job-1").click();
  await page.getByTestId("confirm-delete-audio-export-job-1").click();
  await expect(page.getByTestId("audio-card-export-job-1")).not.toBeVisible();
});

test("delete all clears the audio shelf behind a confirm step", async ({
  page,
}) => {
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

  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 10_000,
  });

  await page.getByTestId("nav-library").click();
  await page.getByTestId("nav-audio").click();
  await expect(page.getByTestId("audio-card-export-job-1")).toBeVisible();

  await expect(page.getByTestId("confirm-delete-all-audio")).toHaveCount(0);
  await page.getByTestId("delete-all-audio").click();
  await expect(page.getByText(msg("library.deleteAllQuestion"))).toBeVisible();
  await expect(page.getByTestId("confirm-delete-all-audio")).toBeFocused();

  await page.getByTestId("confirm-delete-all-audio").click();
  await expect(page.getByTestId("audio-card-export-job-1")).not.toBeVisible();
  await expect(page.getByTestId("delete-all-audio")).toHaveCount(0);
});

test("cancelling delete all leaves the shelf intact", async ({ page }) => {
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

  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 10_000,
  });

  await page.getByTestId("nav-library").click();
  await page.getByTestId("nav-audio").click();
  await page.getByTestId("delete-all-audio").click();
  await page.getByRole("button", { name: msg("library.cancel") }).click();

  await expect(page.getByTestId("audio-card-export-job-1")).toBeVisible();
  await expect(page.getByTestId("delete-all-audio")).toBeVisible();
});

test("escape backs out of a delete confirmation and restores focus", async ({
  page,
}) => {
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

  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 10_000,
  });

  await page.getByTestId("nav-library").click();
  await page.getByTestId("nav-audio").click();

  await page.getByTestId("delete-all-audio").click();
  await expect(page.getByTestId("confirm-delete-all-audio")).toBeFocused();
  await page.keyboard.press("Escape");

  await expect(page.getByTestId("confirm-delete-all-audio")).toHaveCount(0);
  await expect(page.getByTestId("delete-all-audio")).toBeFocused();
  await expect(page.getByTestId("audio-card-export-job-1")).toBeVisible();
});

test("a card delete confirmation takes focus and escapes cleanly", async ({
  page,
}) => {
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

  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-output-job-1")).toBeVisible({
    timeout: 10_000,
  });

  await page.getByTestId("nav-library").click();
  await page.getByTestId("nav-audio").click();

  await page.getByTestId("delete-audio-export-job-1").click();
  await expect(
    page.getByTestId("confirm-delete-audio-export-job-1"),
  ).toBeFocused();
  await page.keyboard.press("Escape");

  await expect(page.getByTestId("delete-audio-export-job-1")).toBeFocused();
  await expect(page.getByTestId("audio-card-export-job-1")).toBeVisible();
});

test("a short book on a fast machine imports without asking anything", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    importScenario: {
      path: "/fixtures/Short.pdf",
      book: fixtureBook(),
      plan: {
        pageCount: 120,
        reflowed: false,
        layoutAvailable: true,
        layoutSeconds: 30,
        onGpu: true,
        silent: true,
        recommendLayout: true,
      },
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("library-import-book").click();

  await expect(page.getByTestId("book-card")).toHaveCount(1);
  await expect(page.getByTestId("layout-choice")).toHaveCount(0);
  expect(
    await page.evaluate(
      () => (window as never as Record<string, unknown>).__importedWithLayout,
    ),
  ).toBe(true);
});

test("a long book states the wait and still recommends analysing layout", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    importScenario: {
      path: "/fixtures/Long.pdf",
      book: fixtureBook(),
      plan: {
        pageCount: 613,
        reflowed: false,
        layoutAvailable: true,
        layoutSeconds: 153,
        onGpu: true,
        silent: false,
        recommendLayout: true,
      },
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("library-import-book").click();

  const panel = page.getByTestId("layout-choice");
  await expect(panel).toBeVisible();
  await expect(panel).toContainText(msg("library.eta.minutes", { n: 3 }));
  await expect(panel).toContainText(msg("library.layoutConsequence"));
  await expect(page.getByTestId("layout-choice-with")).toBeFocused();

  await page.getByTestId("layout-choice-with").click();
  await expect(page.getByTestId("book-card")).toHaveCount(1);
  expect(
    await page.evaluate(
      () => (window as never as Record<string, unknown>).__importedWithLayout,
    ),
  ).toBe(true);
});

test("on a CPU-only machine the hours-long option is not the default", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [],
    importScenario: {
      path: "/fixtures/Long.pdf",
      book: fixtureBook(),
      plan: {
        pageCount: 613,
        reflowed: false,
        layoutAvailable: true,
        layoutSeconds: 6982,
        onGpu: false,
        silent: false,
        recommendLayout: false,
      },
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("library-import-book").click();

  const panel = page.getByTestId("layout-choice");
  await expect(panel).toContainText(msg("library.eta.hours", { n: "1.9" }));
  await expect(page.getByTestId("layout-choice-without")).toBeFocused();

  await page.getByTestId("layout-choice-without").click();
  await expect(page.getByTestId("book-card")).toHaveCount(1);
  expect(
    await page.evaluate(
      () => (window as never as Record<string, unknown>).__importedWithLayout,
    ),
  ).toBe(false);
});

test("the book menu offers the improvement the import panel promised", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook({ id: "b-declined", layoutAnalysed: false })],
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await page.getByTestId("book-menu-b-declined").click();
  const item = page.getByTestId("book-menu-item-improve-b-declined");
  await expect(item).toBeVisible();
  await item.click();

  const panel = page.getByTestId("improve-choice");
  await expect(panel).toBeVisible();
  await expect(panel).toContainText(msg("library.eta.hours", { n: "1.9" }));
  await expect(page.getByTestId("improve-choice-cpu")).toBeFocused();

  await page.getByTestId("improve-choice-cpu").click();
  await expect(page.getByTestId("improve-progress")).toBeHidden();
  expect(
    await page.evaluate(
      () => (window as never as Record<string, unknown>).__improvedBookId,
    ),
  ).toBe("b-declined");
  expect(
    await page.evaluate(
      () => (window as never as Record<string, unknown>).__improvedWithLayout,
    ),
  ).toBe(false);
});

test("re-reading can elect the layout pass explicitly", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook({ id: "b-layout", layoutAnalysed: false })],
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await page.getByTestId("book-menu-b-layout").click();
  const item = page.getByTestId("book-menu-item-improve-b-layout");
  await expect(item).toBeVisible();
  await item.click();

  const panel = page.getByTestId("improve-choice");
  await expect(panel).toBeVisible();
  await page.getByTestId("improve-choice-layout").click();
  await expect(page.getByTestId("improve-progress")).toBeHidden();
  expect(
    await page.evaluate(
      () => (window as never as Record<string, unknown>).__improvedBookId,
    ),
  ).toBe("b-layout");
  expect(
    await page.evaluate(
      () => (window as never as Record<string, unknown>).__improvedWithLayout,
    ),
  ).toBe(true);
});

test("a book already read with the layout model is not offered the action", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook({ id: "b-done", layoutAnalysed: true })],
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await expect(page.getByTestId("book-card")).toHaveCount(1);
  await expect(page.getByTestId("book-menu-b-done")).toBeVisible();
  await page.getByTestId("book-menu-b-done").click();
  await expect(page.getByTestId("book-menu-item-improve-b-done")).toHaveCount(
    0,
  );
});

test("the action disappears once the pass has run", async ({ page }) => {
  await installTauriMock(page, {
    books: [fixtureBook({ id: "b-once", layoutAnalysed: false })],
    extractionScenario: {
      plan: {
        pageCount: 40,
        reflowed: false,
        layoutAvailable: true,
        layoutSeconds: 10,
        onGpu: true,
        silent: true,
        recommendLayout: true,
      },
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await page.getByTestId("book-menu-b-once").click();
  await page.getByTestId("book-menu-item-improve-b-once").click();

  await expect(page.getByTestId("improve-choice")).toHaveCount(0);
  await page.getByTestId("book-menu-b-once").click();
  await expect(page.getByTestId("book-menu-item-improve-b-once")).toHaveCount(0);
});

test("the menu is operable from the keyboard and returns focus on Escape", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook({ id: "b-keys", layoutAnalysed: false })],
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  const trigger = page.getByTestId("book-menu-b-keys");
  await trigger.focus();
  await expect(trigger).toHaveAttribute("aria-expanded", "false");

  await page.keyboard.press("ArrowDown");
  await expect(trigger).toHaveAttribute("aria-expanded", "true");
  await expect(page.getByTestId("book-menu-item-improve-b-keys")).toBeFocused();

  await page.keyboard.press("Escape");
  await expect(page.getByTestId("book-menu-items-b-keys")).toHaveCount(0);
  await expect(trigger).toBeFocused();
});

test("a blocked re-extraction explains itself without leaking the code", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook({ id: "b-busy", layoutAnalysed: false })],
    extractionScenario: {
      blocked:
        "ERR_JOB_ACTIVE: an audio job is queued or running for this book; re-extracting now would rewrite the text it reads. Let it finish, or cancel it",
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await page.getByTestId("book-menu-b-busy").click();
  await page.getByTestId("book-menu-item-improve-b-busy").click();

  const alert = page.getByRole("alert");
  await expect(alert).toContainText(msg("library.rereadError.jobActive"));
  await expect(alert).not.toContainText("ERR_");
  await expect(page.getByTestId("improve-choice")).toHaveCount(0);
});

test("progress names the page during a long re-extraction", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook({ id: "b-slow", layoutAnalysed: false })],
    extractionScenario: {
      plan: {
        pageCount: 613,
        reflowed: false,
        layoutAvailable: true,
        layoutSeconds: 20,
        onGpu: true,
        silent: true,
        recommendLayout: true,
      },
      progress: [
        { bookId: "b-slow", page: 25, pages: 613 },
        { bookId: "b-slow", page: 300, pages: 613 },
      ],
      progressDelayMs: 300,
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await page.getByTestId("book-menu-b-slow").click();
  await page.getByTestId("book-menu-item-improve-b-slow").click();

  await expect(page.getByTestId("improve-progress")).toContainText(
    new RegExp(`${fragment("library.rereadingPages", 0)} \\d+/613…`),
  );
});

test("a second re-extraction cannot start while one is running", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [
      fixtureBook({ id: "b-one", title: "First", layoutAnalysed: false }),
      fixtureBook({ id: "b-two", title: "Second", layoutAnalysed: false }),
    ],
    extractionScenario: {
      plan: {
        pageCount: 613,
        reflowed: false,
        layoutAvailable: true,
        layoutSeconds: 10,
        onGpu: true,
        silent: true,
        recommendLayout: true,
      },
      progress: [
        { bookId: "b-one", page: 100, pages: 613 },
        { bookId: "b-one", page: 400, pages: 613 },
      ],
      progressDelayMs: 700,
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await page.getByTestId("book-menu-b-one").click();
  await page.getByTestId("book-menu-item-improve-b-one").click();
  await expect(page.getByTestId("improve-progress")).toBeVisible();

  await page.getByTestId("book-menu-b-two").click();
  await page.getByTestId("book-menu-item-improve-b-two").click();

  expect(
    await page.evaluate(
      () => (window as never as Record<string, unknown>).__improvedBookId,
    ),
  ).toBe("b-one");
});

test("a partial pass keeps the retry instead of recording success", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook({ id: "b-partial", layoutAnalysed: false })],
    extractionScenario: {
      plan: {
        pageCount: 613,
        reflowed: false,
        layoutAvailable: true,
        layoutSeconds: 10,
        onGpu: true,
        silent: true,
        recommendLayout: true,
      },
      runWarnings: [
        "layout_pass_partial: analysed 500 of 613 page(s); the remainder falls back to text repetition",
      ],
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await page.getByTestId("book-menu-b-partial").click();
  await page.getByTestId("book-menu-item-improve-b-partial").click();

  await expect(page.getByText(msg("library.rereadOutcome.partial"))).toBeVisible();
  await expect(page.getByTestId("book-menu-b-partial")).toBeVisible();
  await expect(
    page.getByTestId("book-menu-item-improve-b-partial"),
  ).toHaveCount(0);
});

test("a reflowed book is never offered a pass that cannot help it", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [
      fixtureBook({ id: "b-epub", layoutAnalysed: false, reflowed: true }),
    ],
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await expect(page.getByTestId("book-card")).toHaveCount(1);
  await expect(page.getByTestId("book-menu-b-epub")).toBeVisible();
  await page.getByTestId("book-menu-b-epub").click();
  await expect(page.getByTestId("book-menu-item-improve-b-epub")).toHaveCount(0);
});

test("a long re-extraction can be stopped and the book keeps its text", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook({ id: "b-stop", layoutAnalysed: false })],
    extractionScenario: {
      plan: {
        pageCount: 613,
        reflowed: false,
        layoutAvailable: true,
        layoutSeconds: 10,
        onGpu: true,
        silent: true,
        recommendLayout: true,
      },
      hangUntilStopped: true,
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await page.getByTestId("book-menu-b-stop").click();
  await page.getByTestId("book-menu-item-improve-b-stop").click();
  await expect(page.getByTestId("improve-progress")).toBeVisible();

  await page.getByTestId("improve-stop").click();

  await expect(page.getByTestId("improve-progress")).toHaveCount(0);
  await expect(page.getByRole("alert")).toContainText(msg("library.rereadError.cancelled"));
  await expect(page.getByTestId("book-menu-b-stop")).toBeVisible();
});

test("a scanned book is refused before it can spend hours on a repeat OCR", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook({ id: "b-scan", layoutAnalysed: false })],
    extractionScenario: {
      blocked:
        "ERR_SCANNED: this book is scanned images, so re-reading would repeat the OCR and produce the same text",
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await page.getByTestId("book-menu-b-scan").click();
  await page.getByTestId("book-menu-item-improve-b-scan").click();

  const alert = page.getByRole("alert");
  await expect(alert).toContainText(msg("library.rereadError.scanned"));
  await expect(alert).not.toContainText("ERR_");
  expect(
    await page.evaluate(
      () => (window as never as Record<string, unknown>).__improvedBookId,
    ),
  ).toBeUndefined();
});

test("a pass running before the view mounted is restored with its Stop control", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook({ id: "b-running", layoutAnalysed: false })],
    extractionScenario: {
      activeOnMount: { bookId: "b-running", page: 240, pages: 613 },
      finishOnStop: true,
      plan: {
        pageCount: 613,
        reflowed: false,
        layoutAvailable: true,
        layoutSeconds: 10,
        onGpu: true,
        silent: true,
        recommendLayout: true,
      },
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await expect(page.getByTestId("improve-progress")).toContainText(
    msg("library.rereadingPages", { page: 240, pages: 613 }),
  );
  await expect(page.getByTestId("improve-stop")).toBeVisible();
  await page.getByTestId("book-menu-b-running").click();
  await expect(page.getByTestId("book-menu-item-improve-b-running")).toHaveCount(0);
  await page.keyboard.press("Escape");

  await page.getByTestId("improve-stop").click();
  await expect(page.getByTestId("improve-progress")).toHaveCount(0);
  await expect(page.getByTestId("book-menu-b-running")).toBeVisible();

  await page.getByTestId("book-menu-b-running").click();
  await page.getByTestId("book-menu-item-improve-b-running").click();
  await expect(page.getByTestId("improve-progress")).toBeHidden();
  expect(
    await page.evaluate(
      () => (window as never as Record<string, unknown>).__improvedBookId,
    ),
  ).toBe("b-running");
});

test("a scanned book never shows the action at all", async ({ page }) => {
  await installTauriMock(page, {
    books: [
      fixtureBook({ id: "b-ocr", layoutAnalysed: false, isScanned: true }),
    ],
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await expect(page.getByTestId("book-card")).toHaveCount(1);
  await expect(page.getByTestId("book-menu-b-ocr")).toBeVisible();
  await page.getByTestId("book-menu-b-ocr").click();
  await expect(page.getByTestId("book-menu-item-improve-b-ocr")).toHaveCount(0);
});

test("a refused delete says what to do about it, not that re-reading failed", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook({ id: "b-busy-del", layoutAnalysed: false })],
    extractionScenario: {
      deleteError:
        "ERR_EXTRACTION_ACTIVE: this book's text is being re-extracted; stop that first",
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await page.getByTestId("book-menu-b-busy-del").click();
  await page.getByTestId("delete-book-b-busy-del").click();
  await page.getByTestId("confirm-delete-book-b-busy-del").click();

  const alert = page.getByRole("alert");
  await expect(alert).toContainText(msg("library.deleteError.rereading"));
  await expect(alert).not.toContainText("Re-reading failed");
  await expect(alert).not.toContainText("ERR_");
  await expect(page.getByTestId("book-card")).toHaveCount(1);
});

test("a pass finishing before the restore resolves does not strand the row", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook({ id: "b-gap", layoutAnalysed: false })],
    extractionScenario: {
      activeOnMount: { bookId: "b-gap", page: 600, pages: 613 },
      finishBeforeRestore: true,
      plan: {
        pageCount: 613,
        reflowed: false,
        layoutAvailable: true,
        layoutSeconds: 10,
        onGpu: true,
        silent: true,
        recommendLayout: true,
      },
    },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await expect(page.getByTestId("improve-progress")).toHaveCount(0);
  await expect(page.getByTestId("book-menu-b-gap")).toBeVisible();

  await page.getByTestId("book-menu-b-gap").click();
  await page.getByTestId("book-menu-item-improve-b-gap").click();
  expect(
    await page.evaluate(
      () => (window as never as Record<string, unknown>).__improvedBookId,
    ),
  ).toBe("b-gap");
});

test("the delete entry announces what it deletes", async ({
  page,
}) => {
  await installTauriMock(page, { books: [fixtureBook({ id: "named" })] });
  await page.goto("/");
  await page.getByTestId("nav-library").click();

  await page.getByTestId("book-menu-named").click();
  const trigger = page.getByTestId("delete-book-named");
  await expect(trigger).toBeVisible();

  await expect(
    trigger,
    "the delete entry must say what it deletes: the book and its audio, not just Delete",
  ).toHaveAccessibleName(msg("library.deleteBookTitle"));

  await expect(
    trigger,
    "an empty string is a name a fallback chain built on ?? would happily pass through",
  ).not.toHaveAccessibleName("");
});

test("the first tab stop is a skip link that jumps past the chrome", async ({
  page,
}) => {
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.goto("/");
  await page.getByTestId("library").waitFor();

  await page.keyboard.press("Tab");
  const skip = page.getByTestId("skip-to-content");
  await expect(skip).toBeFocused();
  await expect(skip).toBeVisible();

  await page.keyboard.press("Enter");
  await expect(page.locator("#main-content")).toBeFocused();
});
