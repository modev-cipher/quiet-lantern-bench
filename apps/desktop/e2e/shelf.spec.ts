import { expect, test, type Page } from "@playwright/test";

import { msg } from "./i18nText";
import { wavDataUrl } from "./pcmFixture";
import { fixtureBook, installTauriMock } from "./tauriMock";
import { emitVoice } from "./voiceEnvelope";

const seeded = {
  id: "export-seeded",
  bookId: "book-fixture",
  jobId: "job-1",
  path: "/exports/fixture-book.mp3",
  bytes: 5_000_000,
  format: "mp3",
  mode: "narrate",
  stale: false,
  createdAt: 1_755_000_000_000,
};

async function bootWithSession(page: Page) {
  await installTauriMock(page, {
    books: [fixtureBook()],
    seededExports: [seeded],
    readingScenario: { mediaDataUrl: wavDataUrl() },
    bookAgentProbe: "notInBuild",
  });
  await page.addInitScript(() => {
    localStorage.setItem(
      "txtcaster.listened",
      JSON.stringify({ "export-seeded": 0.3 }),
    );
    localStorage.setItem(
      "txtcaster.listened.recent",
      JSON.stringify(["export-seeded"]),
    );
  });
  await page.goto("/");
}

test("the app opens on the Shelf, the one lit destination", async ({
  page,
}) => {
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.goto("/");
  await expect(page.getByTestId("library")).toBeVisible();
  await expect(page.getByTestId("nav-library")).toHaveAttribute(
    "aria-current",
    "page",
  );
  await expect(page.getByTestId("nav-study")).toHaveCount(0);
  await expect(page.getByTestId("study-hero")).toHaveCount(0);
});

test("a book in progress leads the Shelf and resumes into the reader", async ({
  page,
}) => {
  await bootWithSession(page);
  await expect(page.getByTestId("study-hero")).toBeVisible();
  await expect(page.getByTestId("book-card").first()).toBeVisible();
  await page.getByTestId("study-resume").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
});

test("Talk to the tutor opens the standalone tutor from the bar and Escape hands focus back", async ({
  page,
}) => {
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.goto("/");
  const entry = page.getByTestId("tutor-launcher");
  await expect(entry).toHaveAttribute("aria-expanded", "false");
  await entry.click();
  await expect(page.getByTestId("tutor-panel")).toBeFocused();
  await expect(page.getByTestId("study-standalone-tutor")).toBeVisible();
  await expect(page.getByTestId("study-book-tutor")).toHaveCount(0);
  await expect(entry).toHaveAttribute("aria-expanded", "true");

  await page.keyboard.press("Escape");
  await expect(page.getByTestId("tutor-panel")).toHaveCount(0);
  await expect(entry).toBeFocused();
});

test("the tutor is reachable from Settings too, not only from the Shelf", async ({
  page,
}) => {
  await installTauriMock(page, { books: [] });
  await page.goto("/");
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("tutor-launcher").click();
  await expect(page.getByTestId("study-standalone-tutor")).toBeVisible();
  await page.getByTestId("tutor-panel-close").click();
  await expect(page.getByTestId("tutor-panel")).toHaveCount(0);
});

test("opening the reader closes the shell tutor: one tutor surface at a time", async ({
  page,
}) => {
  await bootWithSession(page);
  await page.getByTestId("tutor-launcher").click();
  await expect(page.getByTestId("tutor-panel")).toBeVisible();
  await page.getByTestId("study-resume").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByTestId("tutor-panel")).toHaveCount(0);
});

test("Talk on a book with nothing to render opens the tutor panel with the book attached", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook({ pageCount: 0 })],
  });
  await page.goto("/");
  await page.getByTestId("talk-book-book-fixture").click();
  const section = page.getByTestId("study-book-tutor");
  await expect(section).toBeVisible();
  await expect(page.getByTestId("study-book-tutor-no-cast")).toBeVisible();
  await expect(page.getByTestId("study-standalone-tutor")).toHaveCount(0);
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
});

const activeKey = {
  entries: [
    {
      baseUrl: "https://generativelanguage.googleapis.com",
      provider: "gemini",
      last4: "abcd",
      state: "active" as const,
      keyPresent: true,
      custom: false,
    },
  ],
};

test("the shell tutor stays closed after a reader round trip", async ({
  page,
}) => {
  await bootWithSession(page);
  await page.getByTestId("tutor-launcher").click();
  await expect(page.getByTestId("tutor-panel")).toBeVisible();
  await page.getByTestId("study-resume").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await page.getByTestId("reading-close").click();
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await expect(page.getByTestId("library")).toBeVisible();
  await expect(page.getByTestId("tutor-panel")).toHaveCount(0);
  await expect(page.getByTestId("tutor-launcher")).toHaveAttribute(
    "aria-expanded",
    "false",
  );
});

test("a live book tutor survives leaving the reader when the shell tutor was open before it", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    bookAgentProbe: "notInBuild",
    voiceKeysScenario: activeKey,
  });
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");
  await page.getByTestId("tutor-launcher").click();
  await expect(page.getByTestId("tutor-panel")).toBeVisible();
  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await emitVoice(page, { kind: "lifecycle", state: "connected" });
  await emitVoice(page, { kind: "mode", mode: "text", model: "text-model" });
  await expect(page.getByTestId("voice-session-stop")).toBeVisible();

  await page.getByTestId("reading-close").click();
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await expect(page.getByTestId("library")).toBeVisible();
  await expect(
    page.getByTestId("tutor-panel"),
    "a standalone panel remounting here would rebind the voice store to no book and blank the live session",
  ).toHaveCount(0);

  await page.getByTestId("talk-book-book-fixture").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(
    page.getByTestId("voice-session-stop"),
    "the running session is still the reader's to end",
  ).toBeVisible();
  const stops = await page.evaluate(
    () =>
      (
        ((window as unknown as Record<string, unknown>).__invokedCommands ??
          []) as string[]
      ).filter((command) => command === "voice_session_stop").length,
  );
  expect(stops, "nothing ended the session along the way").toBe(0);
});

test("deleting the book in progress removes it from Continue at once", async ({
  page,
}) => {
  await bootWithSession(page);
  await expect(page.getByTestId("study-hero")).toBeVisible();
  const card = page.getByTestId("book-card");
  await page.getByTestId("book-menu-book-fixture").click();
  await page
    .getByRole("menuitem", { name: msg("library.deleteBookTitle") })
    .click();
  await card
    .getByRole("button", { name: msg("library.confirmDelete") })
    .click();
  await expect(card).toHaveCount(0);
  await expect(page.getByTestId("study-hero")).toHaveCount(0);
  await expect(page.getByTestId("study-resume")).toHaveCount(0);
});

test("Escape in a dialog above the tutor closes the dialog, not the tutor", async ({
  page,
}) => {
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.goto("/");
  await page.getByTestId("tutor-launcher").click();
  await expect(page.getByTestId("tutor-panel")).toBeVisible();
  const feedback = page.getByTestId("nav-report-issue");
  await feedback.click();
  await expect(
    page.getByTestId("report-issue-modal").getByRole("dialog"),
  ).toBeFocused();

  await page.keyboard.press("Escape");
  await expect(page.getByTestId("report-issue-modal")).toHaveCount(0);
  await expect(page.getByTestId("tutor-panel")).toBeVisible();
  await expect(feedback).toBeFocused();
});

test("at the default window a book's primary action is on screen without scrolling", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1100, height: 720 });
  await installTauriMock(page, { books: [fixtureBook()] });
  await page.goto("/");
  const primary = page.getByTestId("book-card-primary").first();
  await expect(primary).toBeVisible();
  const box = await primary.boundingBox();
  expect(
    box!.y + box!.height,
    "the card's Study or Cast button ends above the fold",
  ).toBeLessThanOrEqual(720);
  await expect(
    page.getByTestId("book-card-title").first(),
    "the title stays a heading for assistive technology even when the cover carries it visually",
  ).toHaveText(fixtureBook().title);
});

test("a tutor that needs a provider offers the way there, not only a sentence about it", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    voiceKeysScenario: { entries: [] },
  });
  await page.goto("/");
  await page.getByTestId("tutor-launcher").click();
  await expect(page.getByTestId("voice-session-key-hint")).toBeVisible();
  await page.getByTestId("voice-session-key-action").click();
  await expect(page.getByTestId("nav-providers")).toHaveAttribute(
    "aria-current",
    "page",
  );
  await expect(
    page.getByTestId("tutor-panel"),
    "the tutor stays open beside the page it sent you to",
  ).toBeVisible();
});
