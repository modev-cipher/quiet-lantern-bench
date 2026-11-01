import { expect, test } from "@playwright/test";

import { enterReading, openReading, play } from "./readingHarness";

async function pauseLineCount(
  page: import("@playwright/test").Page,
): Promise<number> {
  return page.evaluate(() => {
    const calls = (window as unknown as Record<string, unknown>)
      .__readingCalls as { clock: string[] };
    return calls.clock.filter((line) => line.includes("event=pause")).length;
  });
}

test("a scrubber drag never writes more than a bounded handful of pause lines", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);

  const rail = page.getByTestId("player-seek").first();
  const box = await rail.boundingBox();
  expect(box).not.toBeNull();
  if (box == null) return;

  const before = await pauseLineCount(page);
  const y = box.y + box.height / 2;
  await page.mouse.move(box.x + 4, y);
  await page.mouse.down();
  for (let step = 1; step <= 30; step += 1) {
    await page.mouse.move(box.x + 4 + (box.width - 8) * (step / 30), y);
  }
  await page.mouse.up();
  await page.waitForTimeout(300);

  const after = await pauseLineCount(page);
  expect(
    after - before,
    "a 30-step scrub gesture must not write one pause line per pointer move",
  ).toBeLessThanOrEqual(3);
});
