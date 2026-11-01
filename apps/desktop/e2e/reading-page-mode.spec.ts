import { expect, test, type Page } from "@playwright/test";

import golden from "./fixtures/reading-timeline.golden.json" with { type: "json" };
import { FIXTURE_DURATION_MS } from "./pcmFixture";
import { enterReading, openReading, play, waitForTime } from "./readingHarness";
import { msg } from "./i18nText";
import { readingCalls } from "./tauriMock";

interface OverlaySample {
  mediaMs: number;
  text: string;
  word: boolean;
  fallback: boolean;
  left: number;
  width: number;
}

async function sampleOverlay(page: Page): Promise<void> {
  await page.evaluate(() => {
    const samples: unknown[] = [];
    (window as unknown as Record<string, unknown>).__overlaySamples = samples;
    const percent = (raw: string) => Number.parseFloat(raw.replace("%", ""));
    const tick = () => {
      const audio = document.querySelector("audio");
      const word = document.querySelector<HTMLElement>(
        '[data-testid="reading-page-word"]',
      );
      const fallback = document.querySelector<HTMLElement>(
        '[data-testid="reading-page-fallback"]',
      );
      const live = document.querySelector('[data-testid="reading-page-live"]');
      if (word != null && fallback != null) {
        samples.push({
          mediaMs: (audio?.currentTime ?? 0) * 1000,
          text: live?.textContent ?? "",
          word: !word.hidden,
          fallback: !fallback.hidden,
          left: percent(word.style.left),
          width: percent(word.style.width),
        });
      }
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
}

async function overlaySamples(page: Page): Promise<OverlaySample[]> {
  return page.evaluate(
    () =>
      ((window as unknown as Record<string, unknown>).__overlaySamples ??
        []) as OverlaySample[],
  );
}

const FIRST_SENTENCE = "The first sentence reads aloud.";
const FIRST_SENTENCE_END_MS = golden.chapters[0].segments[0].endMs;
const ONE_SAMPLE_OF_SLACK_MS = 50;

async function waitForFallback(page: Page, visible: boolean): Promise<void> {
  await page.waitForFunction(
    (wanted) => {
      const element = document.querySelector<HTMLElement>(
        '[data-testid="reading-page-fallback"]',
      );
      return element != null && element.hidden !== wanted;
    },
    visible,
    { timeout: 5_000 },
  );
}

async function overlayHidden(
  page: Page,
): Promise<{ word: boolean; fallback: boolean }> {
  return page.evaluate(() => {
    const at = (testid: string) =>
      document.querySelector<HTMLElement>(`[data-testid="${testid}"]`);
    return {
      word: at("reading-page-word")?.hidden ?? true,
      fallback: at("reading-page-fallback")?.hidden ?? true,
    };
  });
}

async function playFrom(page: Page, ms: number): Promise<void> {
  await page.evaluate((target) => {
    const audio = document.querySelector("audio");
    if (audio != null) audio.currentTime = target / 1000;
  }, ms);
}

async function seekTo(page: Page, ms: number): Promise<void> {
  await page.evaluate((target) => {
    const audio = document.querySelector("audio");
    if (audio != null) {
      audio.pause();
      audio.currentTime = target / 1000;
    }
  }, ms);
}

async function enterPageMode(page: Page): Promise<void> {
  await expect(page.getByTestId("reading-surface")).toHaveAttribute(
    "data-mode",
    "script",
  );
  await page.getByTestId("reading-mode-toggle").click();
  await expect(page.getByTestId("reading-surface")).toHaveAttribute(
    "data-mode",
    "page",
  );
}

test("page mode is opt-in: the script surface is what opens", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);

  await expect(
    page.getByTestId("reading-surface"),
    "the match rate is measured on one design family, so the default flip is the user's decision and not this stage's",
  ).toHaveAttribute("data-mode", "script");
  await expect(page.getByTestId("reading-script")).toBeVisible();
  await expect(page.getByTestId("reading-page-overlay")).toHaveCount(0);
  const toggle = page.getByTestId("reading-mode-toggle");
  await expect(toggle).toHaveAttribute("aria-pressed", "false");
  await expect(
    toggle,
    "a toggle names the surface it controls and lets aria-pressed carry the state; naming the action instead makes the accessible name contradict the pressed state",
  ).toHaveAttribute("aria-label", msg("reader.page"));
  await expect(toggle).toHaveText(msg("reader.page"));
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-pressed", "true");
  await expect(
    toggle,
    "the name is constant across states; only the pressed state moves",
  ).toHaveAttribute("aria-label", msg("reader.page"));
  await expect(toggle).toHaveText(msg("reader.page"));
});

test("the toggle draws the spoken word on the printed page and keeps the text audible to a screen reader", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await seekTo(page, 250);
  await enterPageMode(page);

  const live = page.getByTestId("reading-page-live");
  await expect(
    live,
    "an img is not screen-readable, so page mode must keep the sentence in a live region",
  ).toHaveAttribute("aria-live", "polite");
  await expect(live).toHaveText("The first sentence reads aloud.");

  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "1",
  );
  await expect(page.getByTestId("reading-facsimile-image")).toBeVisible();

  await page.waitForFunction(
    () => {
      const element = document.querySelector<HTMLElement>(
        '[data-testid="reading-page-word"]',
      );
      return element != null && !element.hidden;
    },
    null,
    { timeout: 5_000 },
  );
  expect(
    (await overlayHidden(page)).fallback,
    "the fallback container is a zero-height box whose children are absolutely positioned, so a Playwright visibility assertion on it can never fail; the property is what carries the state",
  ).toBe(true);
});

test("a spoken word the walk could not place holds the last placed word, never the block envelope", async ({
  page,
}) => {
  const words = golden.chapters[0].segments[0].words;
  const unplacedIdx = words.findIndex((word) => word.rect == null);
  const unplaced = words[unplacedIdx];
  const held = words[unplacedIdx - 1];
  expect(
    held?.rect,
    "the fixture must put a located word right before the one the walk could not place",
  ).not.toBeNull();
  const leftOf = (pt: number) => (pt / 612) * 100;
  const heldLeft = leftOf(held.rect!.x0);

  await openReading(page);
  await play(page);
  await enterReading(page);
  await enterPageMode(page);
  await sampleOverlay(page);
  await playFrom(page, 0);
  await waitForTime(page, 1.0);

  const observed = (await overlaySamples(page)).filter(
    (sample) => sample.text === FIRST_SENTENCE,
  );
  const samples = observed.filter(
    (sample) => sample.mediaMs < FIRST_SENTENCE_END_MS,
  );
  for (const sample of observed) {
    if (sample.mediaMs < FIRST_SENTENCE_END_MS + ONE_SAMPLE_OF_SLACK_MS)
      continue;
    expect(
      sample.word || sample.fallback,
      `the overlay was still drawn at ${sample.mediaMs} ms, well after the sentence ended; between sentences nothing is being spoken, so nothing is highlighted (the first sample past the end can still carry the previous frame, because the clock is read after that frame was drawn)`,
    ).toBe(false);
  }
  const onWord = samples.filter((sample) => sample.word);
  const duringUnplaced = samples.filter(
    (sample) =>
      sample.mediaMs >= unplaced.startMs && sample.mediaMs < unplaced.endMs,
  );

  test.info().annotations.push({
    type: "measurement",
    description: `samples ${samples.length}, word ${onWord.length}, during the unplaced token ${duringUnplaced.length}`,
  });

  expect(samples.length, "the sentence was never observed").toBeGreaterThan(0);
  for (const sample of samples) {
    expect(
      sample.fallback,
      `the block envelope was drawn at ${sample.mediaMs} ms; this sentence is word-anchored, so a token the walk could not place holds the previous word instead of flashing the whole block`,
    ).toBe(false);
  }
  expect(
    onWord.length,
    "three of this sentence's four tokens are on the page, so the word box must have been used",
  ).toBeGreaterThan(0);
  expect(
    duringUnplaced.length,
    `'${unplaced.w}' was never observed being spoken; a run with no sample inside it proves nothing about how an unplaced token is shown`,
  ).toBeGreaterThan(0);
  for (const sample of duringUnplaced) {
    expect(
      sample.word,
      `the highlight blanked at ${sample.mediaMs} ms while '${unplaced.w}' was spoken; an unplaced token holds the last placed word, never nothing`,
    ).toBe(true);
    expect(
      Math.abs(sample.left - heldLeft),
      `at ${sample.mediaMs} ms the word box sat at ${sample.left}%, not on '${held.w}', the last word the walk placed`,
    ).toBeLessThan(0.001);
  }

  const LOCATED_LEFTS = words
    .filter((word) => word.rect != null)
    .map((word) => leftOf(word.rect!.x0));
  for (const sample of onWord) {
    expect(
      LOCATED_LEFTS.some((left) => Math.abs(left - sample.left) < 0.001),
      `the word box sat at ${sample.left}%, which is none of the three page words this sentence locates`,
    ).toBe(true);
  }
});

test("a framed figure zooms the page and takes the overlay with it", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await enterPageMode(page);
  await seekTo(page, 4900);

  const facsimile = page.getByTestId("reading-facsimile");
  await expect(
    facsimile,
    "seg-0003 sits on page 3 and frames figure-9, which lives on page 2",
  ).toHaveAttribute("data-page", "2", { timeout: 5_000 });
  await expect(facsimile).toHaveAttribute("data-zoomed", "yes");
  await expect(
    facsimile,
    "the component that owns the zoom transform owns the decision to drop the overlay; an overlay left behind would sit on an image that has been scaled and translated out from under it",
  ).toHaveAttribute("data-overlay", "zoomed-out");
  await expect(
    page.getByTestId("reading-page-overlay"),
    "the page-identity gate is the second guard and is pinned in readingPageModel.test.ts against this same segment; here the overlay never mounts at all",
  ).toHaveCount(0);
});

test("the overlay comes back alive after a same-page figure zoom unmounts it", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await enterPageMode(page);

  const facsimile = page.getByTestId("reading-facsimile");

  await seekTo(page, 6_200);
  await expect(
    facsimile,
    "this segment frames a figure on its own page, so the frame zooms and the component that owns the transform drops the overlay while the plan is still a segment plan",
  ).toHaveAttribute("data-page", "4", { timeout: 5_000 });
  await expect(facsimile).toHaveAttribute("data-zoomed", "yes");
  await expect(page.getByTestId("reading-page-overlay")).toHaveCount(0);

  await seekTo(page, 7_500);
  await expect(facsimile).toHaveAttribute("data-page", "4", { timeout: 5_000 });
  await expect(
    facsimile,
    "the next segment on the same page frames nothing, so the overlay mounts again",
  ).toHaveAttribute("data-zoomed", "no");
  await expect(page.getByTestId("reading-page-overlay")).toHaveCount(1);
  await expect(page.getByTestId("reading-page-fallback-box")).toHaveCount(1);

  await waitForFallback(page, true).catch(() => {
    throw new Error(
      "the remounted overlay stayed hidden. Both segments are block rung inside one chapter, so the plan signature never leaves 'segment' across the unmount; React remounts with hidden hard-coded and never changes that prop again, so an applier cached on the signature alone skips the write and the highlight is blank from here on",
    );
  });
});

test("the page mode line reports the rungs the payload delivered, once per change", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await enterPageMode(page);
  await seekTo(page, 400);

  await page.waitForFunction(
    () => {
      const calls = (window as unknown as Record<string, unknown>)
        .__readingCalls as { pageMode: string[] };
      return calls.pageMode.some((line) => line.includes("mode=page"));
    },
    null,
    { timeout: 5_000 },
  );

  const before = (await readingCalls(page)).pageMode;
  test.info().annotations.push({
    type: "measurement",
    description: `page mode lines ${JSON.stringify(before)}`,
  });
  expect(before[before.length - 1]).toContain(
    "mode=page reason=user-preference chapter=chap-0 anchorGeometry=ensured segments=2 word=1 line=0 block=1 page=0 anchored=2",
  );

  await page.waitForTimeout(1_500);
  expect(
    (await readingCalls(page)).pageMode.length,
    `playback is pinned inside chapter 0, so nothing about the surface changed; this line is sampled on every chapter render and a second one here would mean it is not change-gated. Without the pin the fixture runs ${FIXTURE_DURATION_MS} ms, which spans both chapters, so the wait could legitimately cross into chapter 1`,
  ).toBe(before.length);
});

test("a chapter whose geometry was unavailable is re-fetched once when it is opened again", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await enterPageMode(page);

  await page.waitForFunction(
    () => {
      const calls = (window as unknown as Record<string, unknown>)
        .__readingCalls as { chapter: string[] };
      return calls.chapter.includes("chap-1");
    },
    null,
    { timeout: 5_000 },
  );
  await page.waitForTimeout(1_000);
  expect(
    (await readingCalls(page)).chapter.filter((id) => id === "chap-1").length,
    "chap-1 arrives here through the next-chapter prefetch, which runs on every cursor tick; spending the single retry there would both burn it before the reader ever opens the chapter and put a re-fetch on a per-tick path",
  ).toBe(1);

  await seekTo(page, 3600);
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "4",
    { timeout: 5_000 },
  );
  await seekTo(page, 250);
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "1",
    { timeout: 5_000 },
  );
  await seekTo(page, 3600);
  await expect(page.getByTestId("reading-facsimile")).toHaveAttribute(
    "data-page",
    "4",
    { timeout: 5_000 },
  );
  await page.waitForTimeout(1_000);

  const fetched = (await readingCalls(page)).chapter;
  test.info().annotations.push({
    type: "measurement",
    description: `chapter fetches ${JSON.stringify(fetched)}`,
  });
  const degraded = fetched.filter((id) => id === "chap-1").length;
  const ensured = fetched.filter((id) => id === "chap-0").length;

  expect(
    degraded,
    "chap-1 reports anchorGeometry=unavailable, so the frozen payload gets exactly one more chance and never an unbounded loop",
  ).toBe(2);
  expect(
    ensured,
    "chap-0 reports ensured, so re-entering it must serve the cache rather than the engine",
  ).toBe(1);
});

test("both surfaces are the same shell, so switching modes never collapses the window", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);

  const view = page.getByTestId("reading-view");
  const panel = view.getByRole("dialog");
  const viewport = page.viewportSize();
  if (viewport == null) throw new Error("the spec needs a pinned viewport");

  const measure = async () => {
    const box = await panel.boundingBox();
    if (box == null) throw new Error("the reading panel has no box");
    return box;
  };

  await expect(view).toHaveAttribute("data-surface", "script");
  const script = await measure();

  await page.getByTestId("reading-mode-toggle").click();
  await expect(view).toHaveAttribute("data-surface", "page");
  const printed = await measure();

  for (const [name, box] of [
    ["script", script],
    ["page", printed],
  ] as const) {
    expect(
      box.width,
      `the ${name} surface gives back ${Math.round(viewport.width - box.width)}px of the window; one shell means both modes commit to it`,
    ).toBeGreaterThanOrEqual(viewport.width - 1);
    expect(box.height).toBeGreaterThanOrEqual(viewport.height - 1);
  }

  expect(
    Math.round(Math.abs(script.width - printed.width)),
    "the shell must not resize when only its contents change",
  ).toBe(0);
});

test("the no-text-layer banner speaks only for pages that truly cannot highlight", async ({
  page,
}) => {
  await openReading(page, {}, {}, { isScanned: true });
  await play(page);
  await enterReading(page);

  const advisory = page.getByTestId("reading-page-advisory");

  await expect(page.getByTestId("reading-surface")).toHaveAttribute(
    "data-mode",
    "script",
  );
  await playFrom(page, 0);
  await page.waitForFunction(
    () => document.querySelector("[data-word].bg-reading-highlight") != null,
    null,
    { timeout: 5_000 },
  );
  await expect(
    advisory,
    "the reflowed view highlights its own text, so no banner may contradict it",
  ).toHaveCount(0);

  await enterPageMode(page);
  // Restart the audio: word rects only cover the opening of seg-0000, and on
  // a slow host that window may already have played out during the script
  // phase above.
  await playFrom(page, 0);
  await page.waitForFunction(
    () => {
      const element = document.querySelector<HTMLElement>(
        '[data-testid="reading-page-word"]',
      );
      return element != null && !element.hidden;
    },
    null,
    { timeout: 5_000 },
  );
  await expect(
    advisory,
    "this chapter's geometry is ensured and the word overlay is moving, so the scanned flag alone must not claim the pages cannot highlight",
  ).toHaveCount(0);

  await seekTo(page, 4852);
  await expect(
    page.getByTestId("reading-page-live"),
    "the seek must land in the chapter whose geometry is unavailable",
  ).toHaveText("The lecture resumes after it.", { timeout: 5_000 });
  await expect(
    advisory,
    "a chapter that genuinely cannot anchor keeps the banner's own truth",
  ).toContainText(msg("reader.noTextLayerAdvisory"));
});
