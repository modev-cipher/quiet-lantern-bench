import { expect, test } from "@playwright/test";

import { EXPORT_ID, openReading, play } from "./readingHarness";

const RULE = `[data-testid="audio-card-${EXPORT_ID}"] [role="progressbar"]`;

async function listened(page: import("@playwright/test").Page) {
  const raw = await page.locator(RULE).first().getAttribute("aria-valuenow");
  return raw == null ? null : Number(raw) / 100;
}

test("the card's progress bar is where the voice is, and it is still there when you come back", async ({
  page,
}) => {
  const storedFraction = async () =>
    page.evaluate((id) => {
      const raw = localStorage.getItem("txtcaster.listened");
      if (raw == null) return null;
      try {
        const parsed = JSON.parse(raw) as Record<string, unknown>;
        const value = parsed[id];
        return typeof value === "number" ? value : null;
      } catch {
        return null;
      }
    }, EXPORT_ID);

  await openReading(page);

  await page.getByTestId("nav-audio").click();
  await expect(
    page.locator(RULE),
    "a cast nobody has played reads as not started",
  ).toHaveAttribute("aria-valuenow", "0");

  await play(page);
  await page.waitForFunction(() => {
    const audio = document.querySelector("audio");
    return audio != null && audio.currentTime > 0 && audio.duration > 0;
  });

  await expect
    .poll(() => listened(page), {
      message: "the rule must appear once the voice has actually moved",
    })
    .toBeGreaterThan(0);

  await expect
    .poll(storedFraction, {
      message:
        "a session that is killed mid-play must still have committed a position; the pause path cannot be the only writer",
    })
    .toBeGreaterThan(0);

  await page.evaluate(() => {
    const audio = document.querySelector("audio");
    if (audio != null) audio.currentTime = audio.duration * 0.4;
  });

  await expect
    .poll(storedFraction, {
      message: "the throttled writer should have committed the 40% mark",
    })
    .toBeGreaterThan(0.39);

  await page.evaluate(() => {
    const audio = document.querySelector("audio");
    if (audio == null) return;
    audio.currentTime = audio.duration * 0.402;
    audio.pause();
  });

  await expect
    .poll(() => listened(page), {
      message: "a pause must commit the position the reader stopped at",
    })
    .toBeGreaterThanOrEqual(0.4);

  await expect
    .poll(storedFraction, {
      message:
        "the reader stopped 0.2% past the last throttled write, which is inside PERSIST_STEP, so only the pause writer can record where they actually are — without it the shelf loses up to a whole step of the book",
    })
    .toBeGreaterThan(0.401);
});
