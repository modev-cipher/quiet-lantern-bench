import { expect, test } from "@playwright/test";

import { msg } from "./i18nText";
import { wavDataUrl } from "./pcmFixture";
import { EXPORT_ID, audioState, openReading, play } from "./readingHarness";
import { fixtureBook, installTauriMock } from "./tauriMock";

const jobless = {
  id: "export-jobless",
  bookId: "book-fixture",
  jobId: null,
  path: "/exports/fixture-book.mp3",
  bytes: 5_000_000,
  format: "mp3",
  mode: "narrate",
  stale: false,
  createdAt: 1_755_000_000_000,
};

test("an audio card that can be followed opens its Book, not a separate player card", async ({
  page,
}) => {
  await openReading(page);
  await page.getByTestId(`audio-card-${EXPORT_ID}`).click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByTestId("now-playing-view")).toHaveCount(0);
});

test("the strip's cover opens the Book it is playing", async ({ page }) => {
  await openReading(page);
  await play(page);
  await page.getByTestId("mini-player-expand").click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
  await expect(page.getByTestId("now-playing-view")).toHaveCount(0);
});

test("audio that cannot be followed plays in the strip, which is the whole player", async ({
  page,
}) => {
  await installTauriMock(page, {
    books: [fixtureBook()],
    seededExports: [jobless],
    readingScenario: { mediaDataUrl: wavDataUrl() },
  });
  await page.goto("/");
  await page.getByTestId("nav-library").click();
  await page.getByTestId("nav-audio").click();
  await page.getByTestId("audio-card-export-jobless").click();

  await expect(page.getByTestId("mini-player")).toBeVisible();
  await expect(page.getByTestId("now-playing-view")).toHaveCount(0);
  await expect(page.getByTestId("reading-view")).toHaveCount(0);
  await expect
    .poll(async () => (await audioState(page)).paused, { timeout: 10_000 })
    .toBe(false);
  await expect(page.getByTestId("mini-player-expand")).toHaveCount(0);
  await expect(page.getByTestId("mini-player-follow-along")).toHaveCount(0);

  const strip = page.getByTestId("mini-player");
  for (const control of [
    "player-skip-back",
    "player-skip-fwd",
    "player-rate",
    "player-mute",
  ]) {
    await expect(
      strip.getByTestId(control),
      "the strip is this audio's whole player, so it keeps every control the retired card had",
    ).toBeVisible();
  }
  await strip.getByTestId("player-mute").click();
  await expect
    .poll(() => page.evaluate(() => document.querySelector("audio")?.muted))
    .toBe(true);
  await strip.getByTestId("player-mute").click();
  await expect
    .poll(() => page.evaluate(() => document.querySelector("audio")?.muted))
    .toBe(false);
});

test("the strip has no separate expand control", async ({ page }) => {
  await openReading(page);
  await play(page);
  await expect(page.getByTestId("mini-player-follow-along")).toBeVisible();
  await expect(
    page
      .getByTestId("mini-player")
      .getByRole("button", { name: msg("player.expand"), exact: true }),
  ).toHaveCount(0);
});
