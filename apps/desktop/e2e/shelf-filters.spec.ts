import { expect, test, type Page } from "@playwright/test";

import { msg } from "./i18nText";
import { wavDataUrl } from "./pcmFixture";
import { fixtureBook, fixtureChapters, installTauriMock } from "./tauriMock";

const owned = {
  id: "export-owned",
  bookId: "book-fixture",
  jobId: "job-1",
  path: "/exports/fixture-book.mp3",
  bytes: 5_000_000,
  format: "mp3",
  mode: "narrate",
  stale: false,
  createdAt: 1_755_000_000_000,
};

const orphan = {
  ...owned,
  id: "export-orphan",
  bookId: null,
  jobId: null,
  path: "/exports/lost-book.mp3",
  createdAt: 1_754_000_000_000,
};

async function bootWithAudio(page: Page) {
  await installTauriMock(page, {
    books: [fixtureBook()],
    seededExports: [owned, orphan],
    readingScenario: { mediaDataUrl: wavDataUrl() },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
}

test("the Library opens on the Books tab with All and Casting chips", async ({
  page,
}) => {
  await bootWithAudio(page);
  const tabs = page.getByRole("tablist", { name: msg("library.title") });
  await expect(tabs.getByRole("tab")).toHaveCount(2);
  await expect(page.getByTestId("zone-books")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  const group = page.getByRole("group", { name: msg("library.filters") });
  await expect(group.getByRole("button")).toHaveText([
    msg("library.filter.all"),
    msg("nav.casting"),
  ]);
  await expect(page.getByTestId("filter-all-books")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(page.getByTestId("book-grid")).toBeVisible();
});

test("With audio shows the book's casts and keeps Send to phone and Delete all", async ({
  page,
}) => {
  await bootWithAudio(page);
  await page.getByTestId("nav-audio").click();
  await expect(page.getByTestId("nav-audio")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.getByTestId("filter-all-audio")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(page.getByTestId("audio-card-export-owned")).toBeVisible();
  await expect(page.getByTestId("audio-card-export-orphan")).toHaveCount(0);
  await expect(page.getByTestId("send-to-phone")).toBeEnabled();
  await expect(page.getByTestId("book-grid")).toHaveCount(0);
});

test("Unattached audio shows only casts whose book is gone", async ({
  page,
}) => {
  await bootWithAudio(page);
  await page.getByTestId("nav-audio").click();
  await page.getByTestId("filter-unattached").click();
  await expect(page.getByTestId("audio-card-export-orphan")).toBeVisible();
  await expect(page.getByTestId("audio-card-export-owned")).toHaveCount(0);
  await expect(page.getByTestId("library")).toContainText(
    msg("library.unattachedHint"),
  );
});

test("Unattached audio says so when there is none", async ({ page }) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    seededExports: [owned],
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("nav-audio").click();
  await page.getByTestId("filter-unattached").click();
  await expect(page.getByTestId("unattached-empty")).toHaveText(
    msg("library.unattachedEmpty"),
  );
});

test("Casting shows only books with a cast in progress, and says so when there is none", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [
      fixtureBook(),
      fixtureBook({ id: "book-other", title: "Another Book" }),
    ],
    jobScenario: { chapters: fixtureChapters(), stageDelayMs: 20_000 },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("filter-casting").click();
  await expect(page.getByTestId("casting-empty")).toHaveText(
    msg("library.castingEmpty"),
  );

  await page.getByTestId("zone-books").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("start-job").click();
  await expect(page.getByTestId("job-modal")).not.toBeVisible();

  await page.getByTestId("nav-library").click();
  await page.getByTestId("filter-casting").click();
  await expect(page.getByTestId("book-card")).toHaveCount(1);
  await expect(page.getByTestId("book-grid")).toContainText(
    fixtureBook().title,
  );
});

test("Continue sits on All only", async ({ page }) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    seededExports: [owned],
    readingScenario: { mediaDataUrl: wavDataUrl() },
  });
  await page.addInitScript(() => {
    localStorage.setItem(
      "txtcaster.listened",
      JSON.stringify({ "export-owned": 0.3 }),
    );
    localStorage.setItem(
      "txtcaster.listened.recent",
      JSON.stringify(["export-owned"]),
    );
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await expect(page.getByTestId("shelf-continue")).toBeVisible();
  await page.getByTestId("filter-casting").click();
  await expect(page.getByTestId("shelf-continue")).toHaveCount(0);
  await page.getByTestId("zone-books").click();
  await expect(page.getByTestId("shelf-continue")).toBeVisible();
});

test("an explicit request for the audio shelf shows With audio, even from Unattached", async ({
  page,
}) => {
  await bootWithAudio(page);
  await page.getByTestId("nav-audio").click();
  await page.getByTestId("filter-unattached").click();
  await expect(page.getByTestId("audio-card-export-orphan")).toBeVisible();

  await page.evaluate(() =>
    window.dispatchEvent(
      new CustomEvent("txtcaster:go-view", { detail: "audio" }),
    ),
  );
  await expect(page.getByTestId("nav-audio")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.getByTestId("filter-all-audio")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(page.getByTestId("audio-card-export-owned")).toBeVisible();
});
