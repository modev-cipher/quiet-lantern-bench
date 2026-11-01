import { expect, test } from "@playwright/test";

import { enterReading, openReading, play } from "./readingHarness";

// The observed window is a script over reading-timeline.golden.json, not a
// wall-clock wait: the audio is paused and its currentTime is stepped just
// past each word or segment start, waiting for the DOM to reflect the step
// before taking the next one. The reading clock samples audio.currentTime
// per animation frame, so a paused element holds each cursor state until the
// test has seen it — a slow host makes the test take longer, never stricter.
// Seeks stay forward and within one segment, which keeps the clock on the
// same advanceCursor path as real playback. The scripted positions are the
// golden's boundaries: seg-0000 words at 0/200/450/890 ms, the seg-0001 turn
// at 1300 ms (the real page fetch), and seg-0002 words at 3452/3592 ms.
const CLOCK_SCRIPT: Array<{
  seekS: number;
  segment: string;
  word: string | null;
}> = [
  { seekS: 0.25, segment: "seg-0000", word: "1" },
  { seekS: 0.5, segment: "seg-0000", word: "2" },
  { seekS: 0.893, segment: "seg-0000", word: "3" },
  { seekS: 1.35, segment: "seg-0001", word: null },
  { seekS: 3.5, segment: "seg-0002", word: "0" },
  { seekS: 3.65, segment: "seg-0002", word: "1" },
];

// Every scripted step whose word differs from the one before it, counting the
// held word 0 the probe is anchored on before the first step. A constant of
// the script above, not a measurement of the host.
const WORD_ADVANCES = 5;

test("word ticks mutate the DOM instead of rendering React", async ({
  page,
}) => {
  await openReading(page);
  await play(page);
  await enterReading(page);
  await page.evaluate(() => {
    const audio = document.querySelector("audio");
    if (audio == null) throw new Error("no audio element to drive");
    audio.pause();
    audio.currentTime = 0;
  });

  // Settle on the scripted clock's origin before installing the probe, so the
  // baseline excludes the renders of pausing and rewinding.
  await page.waitForFunction(
    () =>
      document.querySelector<HTMLElement>("[data-word].bg-reading-highlight")
        ?.dataset.word === "0" &&
      document.querySelector<HTMLElement>('[data-testid="reading-script"]')
        ?.dataset.segment === "seg-0000",
    undefined,
    { timeout: 20_000 },
  );

  await page.evaluate(() => {
    const container = document.querySelector<HTMLElement>(
      '[data-testid="reading-script"]',
    );
    if (container == null) throw new Error("nothing to observe");
    const calls = (window as unknown as Record<string, unknown>)
      .__readingCalls as
      { ensure: number[][]; figures: string[][] } | undefined;
    const probe = {
      words: 0,
      segments: new Set<string>(),
      firstRenderCount: Number(container.dataset.renderCount ?? "0"),
      firstEnsureCount: calls?.ensure.length ?? 0,
      firstFigureCount: calls?.figures.length ?? 0,
    };
    if (container.dataset.segment != null && container.dataset.segment !== "") {
      probe.segments.add(container.dataset.segment);
    }
    let lastWord: string | null =
      document.querySelector<HTMLElement>("[data-word].bg-reading-highlight")
        ?.dataset.word ?? null;
    new MutationObserver(() => {
      const active = document.querySelector<HTMLElement>(
        "[data-word].bg-reading-highlight",
      );
      const word = active?.dataset.word ?? null;
      if (word != null && word !== lastWord) probe.words += 1;
      lastWord = word;
      const segment = container.dataset.segment;
      if (segment != null && segment !== "") probe.segments.add(segment);
    }).observe(container, {
      subtree: true,
      attributes: true,
      attributeFilter: ["class", "data-segment"],
    });
    (window as unknown as Record<string, unknown>).__renderProbe = probe;
  });

  for (const step of CLOCK_SCRIPT) {
    await page.evaluate((seekS) => {
      const audio = document.querySelector("audio");
      if (audio == null) throw new Error("no audio element to drive");
      audio.currentTime = seekS;
    }, step.seekS);
    await page.waitForFunction(
      (expected) => {
        const active = document.querySelector<HTMLElement>(
          "[data-word].bg-reading-highlight",
        );
        const container = document.querySelector<HTMLElement>(
          '[data-testid="reading-script"]',
        );
        return (
          container?.dataset.segment === expected.segment &&
          (active?.dataset.word ?? null) === expected.word
        );
      },
      { segment: step.segment, word: step.word },
      { timeout: 20_000 },
    );
  }

  const result = await page.evaluate(() => {
    const container = document.querySelector<HTMLElement>(
      '[data-testid="reading-script"]',
    );
    const probe = (window as unknown as Record<string, unknown>)
      .__renderProbe as {
      words: number;
      segments: Set<string>;
      firstRenderCount: number;
      firstEnsureCount: number;
      firstFigureCount: number;
    };
    const calls = (window as unknown as Record<string, unknown>)
      .__readingCalls as
      { ensure: number[][]; figures: string[][] } | undefined;
    return {
      words: probe.words,
      segments: probe.segments.size,
      ensures: (calls?.ensure.length ?? 0) - probe.firstEnsureCount,
      figureFetches: (calls?.figures.length ?? 0) - probe.firstFigureCount,
      renderDelta:
        Number(container?.dataset.renderCount ?? "0") - probe.firstRenderCount,
    };
  });

  test.info().annotations.push({
    type: "measurement",
    description: `word advances ${result.words}, segments seen ${result.segments}, ensure calls ${result.ensures}, figure fetches ${result.figureFetches}, render count delta ${result.renderDelta}`,
  });

  expect(
    result.words,
    "each scripted advance was awaited in the DOM before the next seek, so the observer must have counted exactly one advance per scripted word change — more means the highlight flickered, fewer means a tick bypassed the DOM",
  ).toBe(WORD_ADVANCES);
  expect(
    result.renderDelta,
    "data-render-count comes from a counter incremented in the render body, so this is renders and not segment transitions: a render per word would cost one per advance, while the budget is one render per segment plus one, doubled to allow StrictMode, plus one per resolved page fetch — the golden's segment-boundary turn at 1300 ms puts a real page fetch inside this window — and one more, doubled the same way, per figure fetch: figures arrive after the cursor has already picked a page, so the page they move the narration to needs its own render or the reader stays on the wrong page. Upper bound, never an equality",
  ).toBeLessThanOrEqual(
    2 * (result.segments + 1) + result.ensures + 2 * result.figureFetches,
  );
  expect(
    result.renderDelta,
    "the words advanced more often than React rendered, which is the whole point of mutating two spans; page and figure fetches are the only non-segment render sources and are budgeted by count, so a render per word still reddens this. WORD_ADVANCES is a constant of the clock script, not an observation of the host, so load cannot shrink this bound",
  ).toBeLessThan(
    WORD_ADVANCES + result.segments + result.ensures + 2 * result.figureFetches,
  );
});
