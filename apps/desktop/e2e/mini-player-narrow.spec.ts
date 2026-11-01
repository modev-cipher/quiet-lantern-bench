import { expect, test } from "@playwright/test";

import { fixtureBook, fixtureChapters, installTauriMock } from "./tauriMock";
import { wavDataUrl } from "./pcmFixture";

/*
 * #197: below ~900px the single mini-player row collapsed — the seek strip
 * slid under the action cluster, so a tap on "Follow along" scrubbed the
 * audio instead. The row now wraps the seek strip onto its own line below
 * the md breakpoint; a hit-test at the button's centre must resolve the
 * button, and the two rects must not intersect.
 */
test("at 480px the follow-along button owns its own pixels", async ({
  page,
}) => {
  await page.setViewportSize({ width: 480, height: 860 });
  await installTauriMock(page, {
    books: [fixtureBook()],
    readingScenario: { mediaDataUrl: wavDataUrl() },
    jobScenario: {
      chapters: fixtureChapters(),
      stageDelayMs: 10,
      outputPath: "/exports/fixture-book.mp3",
    },
  });
  await page.goto("/");

  await page.getByTestId("nav-library").click();
  await page.getByTestId("create-audio-book-fixture").click();
  await page.getByTestId("start-job").click();
  await page.getByTestId("job-output-job-1").waitFor({ timeout: 20000 });
  await page.getByTestId("nav-library").first().click();
  await page.getByTestId("nav-audio").click();
  await page.getByTestId("cover-quick-toggle-export-job-1").click();
  await page.getByTestId("mini-player").waitFor();

  const follow = page.getByTestId("mini-player-follow-along");
  await expect(follow).toBeVisible();

  const hit = await page.evaluate(() => {
    const button = document.querySelector<HTMLElement>(
      '[data-testid="mini-player-follow-along"]',
    );
    const seek = document.querySelector<HTMLElement>(
      '[data-testid="player-seek"]',
    );
    if (button == null || seek == null) return null;
    const b = button.getBoundingClientRect();
    const s = seek.getBoundingClientRect();
    const overlap =
      Math.max(0, Math.min(b.right, s.right) - Math.max(b.left, s.left)) *
      Math.max(0, Math.min(b.bottom, s.bottom) - Math.max(b.top, s.top));
    const centre = document.elementFromPoint(
      b.left + b.width / 2,
      b.top + b.height / 2,
    );
    return {
      overlap,
      centreHitsButton: button.contains(centre),
      seekWidth: s.width,
    };
  });

  expect(hit).not.toBeNull();
  expect(hit?.overlap).toBe(0);
  expect(hit?.centreHitsButton).toBe(true);
  // The wrapped seek strip is a usable scrub target, not the 48px stub the
  // collapsed row produced (worst case: 480px window minus the sidebar).
  expect(hit?.seekWidth).toBeGreaterThan(100);

  await follow.click();
  await expect(page.getByTestId("reading-view")).toBeVisible();
});
