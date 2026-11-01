import { expect, test } from "@playwright/test";

import { enterReading, openReading, play } from "./readingHarness";
import { readingCalls } from "./tauriMock";

async function seekTo(page: import("@playwright/test").Page, ms: number) {
  await page.evaluate((target) => {
    const audio = document.querySelector("audio");
    if (audio != null) audio.currentTime = target / 1000;
  }, ms);
}

test("the facsimile follows the page and prefetches the next one exactly once", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await seekTo(page, 250);

  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "1",
  );

  await page.waitForFunction(
    () => {
      const calls = (window as unknown as Record<string, unknown>)
        .__readingCalls as { ensure: number[][] };
      return calls.ensure.flat().includes(2);
    },
    null,
    { timeout: 5_000 },
  );

  await seekTo(page, 3600);
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "4",
    { timeout: 5_000 },
  );

  const calls = await readingCalls(page);
  const requested = calls.ensure.flat();
  const counts = new Map<number, number>();
  for (const number of requested) {
    counts.set(number, (counts.get(number) ?? 0) + 1);
  }

  test.info().annotations.push({
    type: "measurement",
    description: `ensure_reading_pages calls ${JSON.stringify(calls.ensure)}`,
  });

  for (const [number, count] of counts) {
    expect(count, `page ${number} was fetched more than once`).toBe(1);
  }
  expect(requested, "the page after the current one is prefetched").toContain(
    2,
  );
  expect(requested).toContain(1);
});

test("a page the engine cannot render shows a placeholder that already knows its shape", async ({
  page,
}) => {
  await openReading(page, { engineDown: true });
  await play(page);
  await enterReading(page);
  await seekTo(page, 4900);

  const facsimile = page.getByTestId("reading-facsimile");
  await expect(facsimile).toHaveAttribute("data-page", "2", { timeout: 5_000 });
  await expect(page.getByTestId("reading-facsimile-placeholder")).toBeVisible();

  const box = await facsimile.boundingBox();
  expect(box).not.toBeNull();
  const ratio =
    (box as { width: number; height: number }).width /
    (box as { width: number; height: number }).height;

  expect(
    ratio,
    "the placeholder is sized from the page row, so the pane cannot jump when an image arrives",
  ).toBeCloseTo(612 / 792, 1);
});

test("a page image that cannot load is retried exactly once", async ({
  page,
}) => {
  const assetRequests: string[] = [];
  await openReading(page, { brokenPages: [1] });
  page.on("request", (request) => {
    if (request.url().includes("asset.localhost"))
      assetRequests.push(request.url());
  });
  await play(page);
  await enterReading(page);

  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-attempt",
    "2",
    { timeout: 10_000 },
  );
  await expect(page.getByTestId("reading-facsimile-placeholder")).toBeVisible();

  test.info().annotations.push({
    type: "measurement",
    description: `asset requests ${assetRequests.length}`,
  });
  expect(
    assetRequests.length,
    "one initial load and exactly one retry; a retry loop would keep counting",
  ).toBeLessThanOrEqual(2);
});

test("a page that was never rendered is rendered on demand rather than left as a placeholder", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await seekTo(page, 4900);

  const facsimile = page.getByTestId("reading-facsimile");
  await expect(facsimile).toHaveAttribute("data-page", "2", { timeout: 5_000 });
  await expect(
    page.getByTestId("reading-facsimile-image"),
    "page 2 has no render hash in the fixture, so only an on-demand render can produce an image",
  ).toBeVisible({ timeout: 10_000 });
  await expect(page.getByTestId("reading-facsimile-placeholder")).toHaveCount(
    0,
  );

  const calls = await readingCalls(page);
  expect(
    calls.rendered.filter((number) => number === 2),
    "the page was rendered exactly once, not once per frame",
  ).toEqual([2]);
  expect(
    new Set(calls.rendered),
    "only the shown page and its prefetched neighbour are rendered",
  ).toEqual(new Set([2, 3]));
});

test("a page already in the cache is never re-rendered", async ({ page }) => {
  await openReading(page);
  await play(page);
  await enterReading(page);

  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "1",
  );
  await expect(page.getByTestId("reading-facsimile-image")).toBeVisible();

  const calls = await readingCalls(page);
  expect(
    calls.rendered,
    "page 1 arrives with a render hash, so an ensure that re-rendered it would cost every page turn",
  ).not.toContain(1);
});

test("the reading view refuses a page request that is not a reading workload", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);

  const refusal = await page.evaluate(async () => {
    const internals = (window as unknown as Record<string, unknown>)
      .__TAURI_INTERNALS__ as {
      invoke: (cmd: string, args: Record<string, unknown>) => Promise<unknown>;
    };
    try {
      await internals.invoke("ensure_reading_pages", {
        bookId: "book-1",
        pages: Array.from({ length: 65 }, (_value, index) => index + 1),
      });
      return "accepted";
    } catch (error) {
      return String(error);
    }
  });

  expect(refusal).toContain("caller defect rather than a workload");
});

test("a page the engine could not render is retried once per visit and heals when it recovers", async ({
  page,
}) => {
  await openReading(page, { engineDown: true });
  await play(page);
  await enterReading(page);
  await seekTo(page, 4900);

  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "2",
    { timeout: 5_000 },
  );
  await expect(page.getByTestId("reading-facsimile-placeholder")).toBeVisible();

  const asksForTwo = async () =>
    (await readingCalls(page)).ensure.flat().filter((number) => number === 2)
      .length;
  const settled = await asksForTwo();
  expect(
    settled,
    "a page that answered without an image was never asked for at all, so the retry cannot exist",
  ).toBeGreaterThanOrEqual(1);
  await page.waitForTimeout(2_000);
  expect(
    await asksForTwo(),
    "the page is still showing and nothing changed, so any further ask is a render loop. Note the failure mode when the guard is removed: the loop saturates the webview and this spec dies by timeout or a closed target rather than by this assertion, so read a timeout here as a probable loop and not as flake",
  ).toBe(settled);
  expect((await readingCalls(page)).rendered).toEqual([]);

  await page.evaluate(() => {
    (
      window as unknown as { __mockSetEngineDown: (value: boolean) => void }
    ).__mockSetEngineDown(false);
  });
  await seekTo(page, 3600);
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "4",
    { timeout: 5_000 },
  );
  await seekTo(page, 4900);
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "2",
    { timeout: 5_000 },
  );

  await expect(
    page.getByTestId("reading-facsimile-image"),
    "returning to the page after the engine recovered must heal it, or a transient outage is permanent",
  ).toBeVisible({ timeout: 10_000 });
});
