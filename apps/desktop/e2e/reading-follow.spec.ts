import { expect, test, type Page } from "@playwright/test";

import { audioState, enterReading, openReading, play } from "./readingHarness";
import { readingCalls } from "./tauriMock";

const FIRST_SENTENCE = "The first sentence reads aloud.";
const SECOND_SENTENCE = "The second sentence follows it.";
const PAGE_WIDTH_PT = 612;
const PAGE_HEIGHT_PT = 792;

async function seekTo(page: Page, ms: number): Promise<void> {
  await page.evaluate((target) => {
    const audio = document.querySelector("audio");
    if (audio != null) {
      audio.pause();
      audio.currentTime = target / 1000;
    }
  }, ms);
}

async function playFrom(page: Page, ms: number): Promise<void> {
  await page.evaluate((target) => {
    const audio = document.querySelector("audio");
    if (audio != null) audio.currentTime = target / 1000;
  }, ms);
}

async function enterPageMode(page: Page): Promise<void> {
  await page.getByTestId("reading-mode-toggle").click();
  await expect(page.getByTestId("reading-surface")).toHaveAttribute(
    "data-mode",
    "page",
  );
}

async function tapAtPagePoint(
  page: Page,
  xPt: number,
  yPt: number,
): Promise<void> {
  const layer = page.getByTestId("reading-tap-layer");
  const box = await layer.boundingBox();
  expect(box, "the tap layer is mounted and has a size").not.toBeNull();
  const { width, height } = box as { width: number; height: number };
  await layer.click({
    position: {
      x: (xPt / PAGE_WIDTH_PT) * width,
      y: (yPt / PAGE_HEIGHT_PT) * height,
    },
  });
}

function expectTapLine(
  line: string,
  xPt: number,
  yPt: number,
  outcome: string,
): void {
  const match = /^([0-9.]+),([0-9.]+) (.+)$/.exec(line);
  expect(match, `'${line}' is not a tap line`).not.toBeNull();
  const [, x, y, rest] = match as RegExpExecArray;
  expect(
    Math.abs(Number.parseFloat(x) - xPt),
    `the tap landed at ${x}pt, not near ${xPt}pt — a click position rounds to a pixel, so the tolerance is a couple of points, never zero`,
  ).toBeLessThan(3);
  expect(Math.abs(Number.parseFloat(y) - yPt)).toBeLessThan(3);
  expect(rest).toBe(outcome);
}

test("a manual flip browses without seeking, and only a flip shows the way back", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await seekTo(page, 250);
  await enterPageMode(page);

  const facsimile = page.getByTestId("reading-facsimile");
  const controls = page.getByTestId("reading-page-controls");
  await expect(facsimile).toHaveAttribute("data-page", "1");
  await expect(controls).toHaveAttribute("data-browsing", "no");
  await expect(page.getByTestId("reading-return")).toHaveCount(0);
  const before = await audioState(page);

  await page.getByTestId("reading-flip-forward").click();

  await expect(facsimile).toHaveAttribute("data-page", "2");
  await expect(controls).toHaveAttribute("data-browsing", "yes");
  await expect(page.getByTestId("reading-return")).toBeVisible();
  const after = await audioState(page);
  expect(
    after.currentTime,
    "a flip must never seek the audio; an accidental page turn destroying the playback position is the defect this state machine exists to prevent",
  ).toBe(before.currentTime);

  const calls = await readingCalls(page);
  expect(calls.followState).toEqual([
    "state=browsing trigger=flip from=following to=browsing:2 seek=no",
  ]);

  await page.getByTestId("reading-return").click();
  await expect(facsimile).toHaveAttribute("data-page", "1");
  await expect(controls).toHaveAttribute("data-browsing", "no");
  await expect(page.getByTestId("reading-return")).toHaveCount(0);
  expect((await audioState(page)).currentTime).toBe(before.currentTime);
  expect((await readingCalls(page)).followState).toEqual([
    "state=browsing trigger=flip from=following to=browsing:2 seek=no",
    "state=following trigger=returnToNarration from=browsing:2 to=following seek=no",
  ]);
});

test("a clamped flip at the edge transitions nothing and logs nothing", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await seekTo(page, 250);
  await enterPageMode(page);

  const back = page.getByTestId("reading-flip-back");
  await back.click();
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "1",
  );
  await expect(
    page.getByTestId("reading-page-controls"),
    "flipping back from page 1 clamps onto page 1, and a browse onto the page you were already shown is still a deliberate browse",
  ).toHaveAttribute("data-browsing", "yes");
  await back.click();
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "1",
  );
  const calls = await readingCalls(page);
  expect(
    calls.followState,
    "the second flip moved nothing, so the transitions-only rule says it emits nothing",
  ).toEqual([
    "state=browsing trigger=flip from=following to=browsing:1 seek=no",
  ]);
});

test("the browse state machine serves the script layout too", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await seekTo(page, 250);

  await expect(page.getByTestId("reading-surface")).toHaveAttribute(
    "data-mode",
    "script",
  );
  const facsimile = page.getByTestId("reading-facsimile");
  await expect(facsimile).toHaveAttribute("data-page", "1");

  await page.getByTestId("reading-flip-forward").click();
  await expect(facsimile).toHaveAttribute("data-page", "2");
  await expect(page.getByTestId("reading-return")).toBeVisible();

  await page.getByTestId("reading-mode-toggle").click();
  await expect(page.getByTestId("reading-surface")).toHaveAttribute(
    "data-mode",
    "page",
  );
  await expect(
    page.getByTestId("reading-facsimile"),
    "one state machine serves both surfaces: a mode flip stays inside browsing on the same page and never re-aims",
  ).toHaveAttribute("data-page", "2");
  await expect(page.getByTestId("reading-return")).toBeVisible();
  const calls = await readingCalls(page);
  expect(
    calls.followState,
    "the mode flip changed no follow state, so it logged nothing",
  ).toEqual([
    "state=browsing trigger=flip from=following to=browsing:2 seek=no",
  ]);
});

test("a chapter change holds the browsed page rather than silently re-aiming it", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await seekTo(page, 250);
  await enterPageMode(page);

  await page.getByTestId("reading-flip-forward").click();
  const facsimile = page.getByTestId("reading-facsimile");
  await expect(facsimile).toHaveAttribute("data-page", "2");

  await seekTo(page, 3600);
  await page.waitForFunction(
    () => {
      const calls = (window as unknown as Record<string, unknown>)
        .__readingCalls as { chapter: string[] };
      return calls.chapter.includes("chap-1");
    },
    null,
    { timeout: 5_000 },
  );

  await expect(
    facsimile,
    "the narration crossed into chapter 2, and the browsed page held: clamping it into the new chapter's range would be the silent re-aim a flip-that-seeks is",
  ).toHaveAttribute("data-page", "2");
  await expect(page.getByTestId("reading-page-controls")).toHaveAttribute(
    "data-browsing",
    "yes",
  );
  const calls = await readingCalls(page);
  expect(
    calls.followState,
    "the chapter change transitioned nothing, so it logged nothing",
  ).toEqual([
    "state=browsing trigger=flip from=following to=browsing:2 seek=no",
  ]);

  await page.getByTestId("reading-return").click();
  await expect(
    facsimile,
    "returning lands on the narration's page, which chapter 2's cue frames on the figure's own page",
  ).toHaveAttribute("data-page", "4", { timeout: 5_000 });
});

test("a scrubber seek is a deliberate re-aim and ends browsing", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await seekTo(page, 250);
  await enterPageMode(page);

  await page.getByTestId("reading-flip-forward").click();
  await expect(page.getByTestId("reading-page-controls")).toHaveAttribute(
    "data-browsing",
    "yes",
  );

  await page
    .getByTestId("reading-view")
    .getByTestId("player-seek")
    .click({ position: { x: 5, y: 2 } });

  await expect(page.getByTestId("reading-page-controls")).toHaveAttribute(
    "data-browsing",
    "no",
  );
  await expect(page.getByTestId("reading-return")).toHaveCount(0);
  const calls = await readingCalls(page);
  expect(calls.followState).toEqual([
    "state=browsing trigger=flip from=following to=browsing:2 seek=no",
    "state=following trigger=seeked from=browsing:2 to=following seek=yes",
  ]);
});

test("tapping a printed word seeks the narration there and re-establishes following", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await seekTo(page, 250);
  await enterPageMode(page);

  await page.getByTestId("reading-flip-forward").click();
  await page.getByTestId("reading-flip-back").click();
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "1",
  );
  await expect(page.getByTestId("reading-page-controls")).toHaveAttribute(
    "data-browsing",
    "yes",
  );

  await tapAtPagePoint(page, 214, 106);

  await page.waitForFunction(
    () => {
      const audio = document.querySelector("audio");
      return audio != null && Math.abs(audio.currentTime - 0.89) < 0.05;
    },
    null,
    { timeout: 5_000 },
  );
  await expect(
    page.getByTestId("reading-page-controls"),
    "a tap is the user aiming the narration, so following resumes",
  ).toHaveAttribute("data-browsing", "no");
  const calls = await readingCalls(page);
  expect(calls.tap).toHaveLength(1);
  expectTapLine(calls.tap[0], 214, 106, "page=1 hit=word seekMs=890");
  expect(calls.followState[calls.followState.length - 1]).toBe(
    "state=following trigger=seeked from=browsing:1 to=following seek=yes",
  );
});

test("a tap in the per-token gap is a dead tap: no seek, no snap, one debug line", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await seekTo(page, 250);
  await enterPageMode(page);
  const before = await audioState(page);

  await tapAtPagePoint(page, 160, 106);

  await expect(page.getByTestId("reading-tap-layer")).toBeVisible();
  const calls = await readingCalls(page);
  expect(
    calls.tap,
    "the golden's unlocated token is printed between 134pt and 188pt; snapping the tap to a neighbour would seek somewhere the user did not aim",
  ).toHaveLength(1);
  expectTapLine(calls.tap[0], 160, 106, "page=1 hit=none");
  const after = await audioState(page);
  expect(after.currentTime).toBe(before.currentTime);
  expect(calls.followState).toEqual([]);
});

test("the tap layer leaves with the zoom, exactly like the overlay", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await enterPageMode(page);
  await seekTo(page, 250);
  await expect(page.getByTestId("reading-tap-layer")).toHaveCount(1);

  await seekTo(page, 4900);
  const facsimile = page.getByTestId("reading-facsimile");
  await expect(facsimile).toHaveAttribute("data-zoomed", "yes", {
    timeout: 5_000,
  });
  await expect(
    page.getByTestId("reading-tap-layer"),
    "a zoomed page is scaled and translated out from under the hit layer, so a tap would resolve against coordinates the reader is not looking at — the same policy that drops the overlay",
  ).toHaveCount(0);
});

test("the page turns at the segment boundary, with the next page warm and no wrong-page overlay", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await enterPageMode(page);

  await page.evaluate(() => {
    const samples: unknown[] = [];
    (window as unknown as Record<string, unknown>).__turnSamples = samples;
    const tick = () => {
      const audio = document.querySelector("audio");
      const facsimile = document.querySelector<HTMLElement>(
        '[data-testid="reading-facsimile"]',
      );
      const live = document.querySelector('[data-testid="reading-page-live"]');
      const word = document.querySelector<HTMLElement>(
        '[data-testid="reading-page-word"]',
      );
      const fallback = document.querySelector<HTMLElement>(
        '[data-testid="reading-page-fallback"]',
      );
      const box = document.querySelector<HTMLElement>(
        '[data-testid="reading-page-fallback-box"]',
      );
      if (facsimile != null) {
        samples.push({
          mediaMs: (audio?.currentTime ?? 0) * 1000,
          page: facsimile.dataset.page ?? "",
          text: live?.textContent ?? "",
          word: word != null && !word.hidden,
          fallback: fallback != null && !fallback.hidden,
          boxTop:
            box == null
              ? null
              : Number.parseFloat(box.style.top.replace("%", "")),
        });
      }
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });

  await playFrom(page, 700);
  await page.waitForFunction(
    () => {
      const audio = document.querySelector("audio");
      return (audio?.currentTime ?? 0) >= 1.9;
    },
    null,
    { timeout: 20_000 },
  );

  const samples = await page.evaluate(
    () =>
      ((window as unknown as Record<string, unknown>).__turnSamples ??
        []) as Array<{
        mediaMs: number;
        page: string;
        text: string;
        word: boolean;
        fallback: boolean;
        boxTop: number | null;
      }>,
  );
  const windowed = samples.filter(
    (sample) => sample.mediaMs >= 750 && sample.mediaMs <= 1_900,
  );
  const onOne = windowed.filter((sample) => sample.page === "1");
  const onTwo = windowed.filter((sample) => sample.page === "2");

  test.info().annotations.push({
    type: "measurement",
    description: `samples ${windowed.length}, page1 ${onOne.length}, page2 ${onTwo.length}`,
  });

  expect(
    onOne.length,
    "the boundary was never observed from its left side",
  ).toBeGreaterThan(0);
  expect(onTwo.length, "the turn never happened").toBeGreaterThan(0);
  for (const sample of windowed) {
    expect(
      sample.page === "1" || sample.page === "2",
      `the facsimile showed page ${sample.page} at ${sample.mediaMs} ms, which neither segment owns`,
    ).toBe(true);
    if (sample.page === "2" && sample.text !== "") {
      expect(
        sample.text,
        `page 2 was shown at ${sample.mediaMs} ms while the live region still carried another segment — the turn left the boundary`,
      ).toBe(SECOND_SENTENCE);
    }
    if (sample.text === FIRST_SENTENCE) {
      expect(
        sample.page,
        `the first sentence's page turned away at ${sample.mediaMs} ms before the segment ended`,
      ).toBe("1");
    }
    if (sample.page === "2") {
      expect(
        sample.word,
        "seg-0001 locates no words, so a word box on page 2 is a wrong-page overlay",
      ).toBe(false);
      if (sample.fallback && sample.boxTop != null) {
        expect(
          Math.abs(sample.boxTop - (126 / PAGE_HEIGHT_PT) * 100),
          `the fallback box on page 2 sat at ${sample.boxTop}%, which is not seg-0001's own rect`,
        ).toBeLessThan(0.1);
      }
    }
  }

  const calls = await readingCalls(page);
  expect(
    calls.ensure[0],
    "the following window [N, N+1] had already asked for page 2 while page 1 was showing, so the incoming page was warm before the turn",
  ).toEqual([1, 2]);
  expect(calls.rendered).toContain(2);
  await expect(page.getByTestId("reading-facsimile-image")).toBeVisible();
});
