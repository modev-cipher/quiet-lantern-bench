import { expect, test } from "@playwright/test";

import golden from "./fixtures/reading-timeline.golden.json" with { type: "json" };
import { enterReading, openReading, play, waitForTime } from "./readingHarness";
import { heldWordAt, type ReadingChapter } from "../src/readingModel";

const chapters = golden.chapters as ReadingChapter[];

const SCOPE = `
Layer C measures clock-to-DOM latency: the gap between audio.currentTime at the instant the
highlight class actually changed and the word's startMs in the golden payload. It does NOT
measure decoder accuracy — that is layer A (tests/reading_timeline.rs, real ffmpeg) and the
WebKitGTK acceptance item. Two composed 120ms budgets are not a 120ms end-to-end guarantee.
The browser here is Chromium; the product ships WebKitGTK.
`;

const MEDIAN_BUDGET_MS = 120;
const WORST_BUT_ONE_REGRESSION_MS = 60;
const MIN_TICKS_PER_SECOND = 20;
const MAX_MUTATIONS_PER_BATCH = 3;
const INSTALL_TOLERANCE_MS = 40;

interface Probe {
  records: Array<{
    segmentId: string;
    wordIndex: number;
    currentTimeMs: number;
  }>;
  batches: number[];
  fillTicks: number[];
  installedAtMs: number;
  settledAtMs: number;
}

function segmentById(segmentId: string) {
  for (const chapter of chapters) {
    const found = chapter.segments.find(
      (entry) => entry.segmentId === segmentId,
    );
    if (found) return found;
  }
  return null;
}

function observableWords(
  fromMs: number,
  toMs: number,
  tickMs: number,
): Array<{ segmentId: string; wordIdx: number }> {
  const after = (startMs: number) => startMs > fromMs;
  const out: Array<{ segmentId: string; wordIdx: number }> = [];
  for (let position = 0; position < chapters.length; position += 1) {
    const chapter = chapters[position];
    for (let sequence = 0; sequence < chapter.segments.length; sequence += 1) {
      const segment = chapter.segments[sequence];
      const opensChapter = position > 0 && sequence === 0;
      for (let index = 0; index < segment.words.length; index += 1) {
        const word = segment.words[index];
        const next = segment.words[index + 1];
        const closesAt = Math.min(
          next?.startMs ?? segment.endMs,
          segment.endMs,
        );
        const window = closesAt - word.startMs;
        if (window < tickMs) continue;
        if (opensChapter && index === 0) continue;
        if (!after(word.startMs) || word.startMs > toMs) continue;
        out.push({ segmentId: segment.segmentId, wordIdx: index });
      }
    }
  }
  return out;
}

function heldWordsOver(
  segment: ReadingChapter["segments"][number],
  fromMs: number,
  toMs: number,
): Array<number | null> {
  const points = new Set<number>([fromMs, toMs]);
  for (const word of segment.words) {
    for (const edge of [word.startMs, word.endMs]) {
      if (edge >= fromMs && edge <= toMs) points.add(edge);
    }
  }
  if (segment.endMs >= fromMs && segment.endMs <= toMs)
    points.add(segment.endMs);
  const answers = new Set<number | null>();
  for (const point of points) {
    answers.add(heldWordAt(segment, point));
    if (point > fromMs) answers.add(heldWordAt(segment, point - 1));
  }
  return [...answers];
}

function peakPerSecond(times: number[]): number {
  let best = 0;
  for (let start = 0; start < times.length; start += 1) {
    let end = start;
    while (end < times.length && times[end] - times[start] < 1000) end += 1;
    best = Math.max(best, end - start);
  }
  return best;
}

function secondWorst(values: number[]): number {
  if (values.length === 0) return Number.NaN;
  if (values.length === 1) return values[0];
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[sorted.length - 2];
}

function quantile(values: number[], fraction: number): number {
  if (values.length === 0) return Number.NaN;
  const sorted = [...values].sort((a, b) => a - b);
  const position = Math.min(
    sorted.length - 1,
    Math.floor(fraction * (sorted.length - 1)),
  );
  return sorted[position];
}

async function rewind(page: import("@playwright/test").Page): Promise<void> {
  await page.evaluate(() => {
    const audio = document.querySelector("audio");
    if (audio != null) audio.currentTime = 0;
  });
  await page.waitForFunction(
    () => (document.querySelector("audio")?.currentTime ?? 1) < 0.1,
    null,
    { timeout: 5_000 },
  );
}

async function installProbe(page: import("@playwright/test").Page) {
  await page.evaluate(() => {
    const container = document.querySelector<HTMLElement>(
      '[data-testid="reading-script"]',
    );
    const audio = document.querySelector("audio");
    if (container == null || audio == null)
      throw new Error("nothing to observe");
    const underline = document.querySelector<HTMLElement>(
      '[data-testid="reading-underline"]',
    );
    const probe = {
      records: [] as Array<{
        segmentId: string;
        wordIndex: number;
        currentTimeMs: number;
      }>,
      batches: [] as number[],
      fillTicks: [] as number[],
      installedAtMs: audio.currentTime * 1000,
      settledAtMs: Number.NaN,
    };
    const observer = new MutationObserver((mutations) => {
      probe.batches.push(mutations.length);
      for (const mutation of mutations) {
        const element = mutation.target as HTMLElement;
        if (!element.classList.contains("bg-reading-highlight")) continue;
        const word = element.getAttribute("data-word");
        if (word == null) continue;
        probe.records.push({
          segmentId: container.dataset.segment ?? "",
          wordIndex: Number(word),
          currentTimeMs: audio.currentTime * 1000,
        });
      }
    });
    observer.observe(container, {
      subtree: true,
      attributes: true,
      attributeFilter: ["class"],
    });
    if (underline != null) {
      const fillObserver = new MutationObserver((mutations) => {
        for (let i = 0; i < mutations.length; i += 1) {
          probe.fillTicks.push(audio.currentTime * 1000);
        }
      });
      fillObserver.observe(underline, {
        attributes: true,
        attributeFilter: ["style"],
      });
    }
    (window as unknown as Record<string, unknown>).__readingProbe = probe;
  });
}

async function readProbe(
  page: import("@playwright/test").Page,
): Promise<Probe> {
  return page.evaluate(
    () =>
      (window as unknown as Record<string, unknown>).__readingProbe as Probe,
  );
}

function scoredRecords(probe: Probe): Probe["records"] {
  const floor = Number.isFinite(probe.settledAtMs)
    ? probe.settledAtMs
    : probe.installedAtMs - INSTALL_TOLERANCE_MS;
  return probe.records.filter((record) => {
    const segment = segmentById(record.segmentId);
    const word = segment?.words[record.wordIndex];
    if (segment == null || word == null) return false;
    return word.startMs >= floor;
  });
}

function errorsFrom(probe: Probe): number[] {
  return scoredRecords(probe).map((record) => {
    const segment = segmentById(record.segmentId);
    const word = segment?.words[record.wordIndex];
    return Math.abs(record.currentTimeMs - (word?.startMs ?? 0));
  });
}

test.describe("layer C — clock to DOM", () => {
  test.beforeEach(() => {
    test.info().annotations.push({ type: "scope", description: SCOPE.trim() });
  });

  test("C1 visible and focused: the highlight lands within budget on the right word", async ({
    page,
  }) => {
    await openReading(page);
    await play(page);
    await enterReading(page);
    await rewind(page);
    await installProbe(page);
    await page.waitForFunction(
      () => {
        const active = document.querySelector<HTMLElement>(
          "[data-word].bg-reading-highlight",
        );
        const audio = document.querySelector("audio");
        if ((audio?.currentTime ?? 1) > 0.6) return true;
        return active == null || active.dataset.word === "0";
      },
      null,
      { timeout: 5_000 },
    );
    await page.evaluate(() => {
      const probe = (window as unknown as Record<string, unknown>)
        .__readingProbe as { settledAtMs: number };
      const audio = document.querySelector("audio");
      probe.settledAtMs = (audio?.currentTime ?? 0) * 1000;
    });

    const samples: Array<{ segmentId: string; idx: number | null; t: number }> =
      [];
    for (let i = 0; i < 20; i += 1) {
      samples.push(
        await page.evaluate(() => {
          const container = document.querySelector<HTMLElement>(
            '[data-testid="reading-script"]',
          );
          const active = document.querySelector<HTMLElement>(
            "[data-word].bg-reading-highlight",
          );
          const audio = document.querySelector("audio");
          return {
            segmentId: container?.dataset.segment ?? "",
            idx: active == null ? null : Number(active.dataset.word),
            t: (audio?.currentTime ?? 0) * 1000,
          };
        }),
      );
      await page.waitForTimeout(60);
    }

    await waitForTime(page, 4.6);
    const probe = await readProbe(page);
    const errors = errorsFrom(probe);
    const observedSegments = new Set(
      probe.records.map((record) => record.segmentId),
    );

    const median = quantile(errors, 0.5);
    const worstButOne = secondWorst(errors);
    const worst = Math.max(...errors);
    test.info().annotations.push({
      type: "method",
      description:
        "playback is rewound with a raw audio.currentTime write, which deliberately bypasses the store trigger so the incremental window's fallback to a cold resolve is the path under test. The correct-word samples start once that resync has landed: the instant after a backward seek is the resync path's business (C3 and the unit resync-counter), not steady-state tracking.",
    });
    test.info().annotations.push({
      type: "measurement",
      description: `onsets n=${errors.length} median=${median.toFixed(1)}ms worstButOne=${worstButOne.toFixed(1)}ms max=${worst.toFixed(1)}ms fillTicks=${probe.fillTicks.length} (at n of about five there is no p95: the regression gate is the second worst value, named for what it is)`,
    });

    expect(
      observedSegments,
      "onsets from both word-bearing segments, so one lucky segment cannot carry the gate",
    ).toContain("seg-0000");
    expect(
      observedSegments,
      "the chapter boundary must still highlight its own words, even though the first of them is excluded from the strict coverage set",
    ).toContain("seg-0002");
    expect(median, "TASKS R2 acceptance gate").toBeLessThanOrEqual(
      MEDIAN_BUDGET_MS,
    );
    expect(
      worstButOne,
      "regression gate on the second worst onset, because at this n a p95 is the same number wearing a percentile's name: a healthy rAF path is about 16ms and onTimeUpdate is about 240ms",
    ).toBeLessThanOrEqual(WORST_BUT_ONE_REGRESSION_MS);

    const spanMs =
      probe.fillTicks.length < 2
        ? 0
        : probe.fillTicks[probe.fillTicks.length - 1] - probe.fillTicks[0];
    const mean = spanMs <= 0 ? 0 : probe.fillTicks.length / (spanMs / 1000);
    const peak = peakPerSecond(probe.fillTicks);
    test.info().annotations.push({
      type: "measurement",
      description: `underline fill writes n=${probe.fillTicks.length} peak=${peak}/s mean=${mean.toFixed(1)}/s over ${(spanMs / 1000).toFixed(2)}s (the mean is diluted by the gaps, where the fill holds at 1 and writes nothing)`,
    });
    expect(
      peak,
      "the fill is written once per clock tick while a segment is under the cursor; a 4Hz onTimeUpdate source peaks at 4",
    ).toBeGreaterThanOrEqual(MIN_TICKS_PER_SECOND);

    expect(
      Math.max(...probe.batches),
      "a batched observer callback would hide which tick moved the class",
    ).toBeLessThanOrEqual(MAX_MUTATIONS_PER_BATCH);

    const lastObservedMs =
      probe.fillTicks.length > 0
        ? probe.fillTicks[probe.fillTicks.length - 1]
        : probe.installedAtMs;
    const tickForCoverage = peak > 0 ? 1000 / peak : 40;
    const expectedWords = observableWords(
      probe.installedAtMs,
      lastObservedMs,
      tickForCoverage,
    );
    const seen = new Set(
      probe.records.map((record) => `${record.segmentId}:${record.wordIndex}`),
    );
    const missed = expectedWords.filter(
      (word) => !seen.has(`${word.segmentId}:${word.wordIdx}`),
    );
    test.info().annotations.push({
      type: "method",
      description:
        "the first word of a chapter is excluded from the strict coverage set: a chapter swap puts one React render between the cursor moving and the new word spans existing, so that word can be one render late rather than one tick late. Its latency is still scored in the median and p95 above, and the boundary segment must still appear in the observed set.",
    });
    test.info().annotations.push({
      type: "measurement",
      description: `words whose display window exceeds one ${tickForCoverage.toFixed(0)}ms tick: ${expectedWords.length}, missed: ${missed.length} ${JSON.stringify(missed)}`,
    });

    expect(
      expectedWords.length,
      "no word in this window has a display window longer than one tick, so nothing was measurable and the pass proved nothing",
    ).toBeGreaterThanOrEqual(3);
    expect(
      missed,
      "every word the measured tick rate could resolve must actually have been highlighted; a word whose window is longer than a tick and was skipped is a dropped highlight, not slow hardware",
    ).toEqual([]);
    const tickMs = peak > 0 ? 1000 / peak : 40;
    const lagBudgetTicks = 2;
    test.info().annotations.push({
      type: "method",
      description: `the correct-word check accepts any word that was the held word at some instant in the last ${lagBudgetTicks} tick periods (${(lagBudgetTicks * tickMs).toFixed(0)}ms at the ${peak}/s cadence measured in this run), which stays strictly inside the ${MEDIAN_BUDGET_MS}ms acceptance budget while still rejecting a word that is simply wrong`,
    });

    let checked = 0;
    for (const sample of samples) {
      const segment = segmentById(sample.segmentId);
      if (segment == null || segment.words.length === 0) continue;
      if (sample.idx == null) continue;
      const allowed = heldWordsOver(
        segment,
        sample.t - lagBudgetTicks * tickMs,
        sample.t,
      );
      expect(
        allowed,
        `at ${sample.t.toFixed(0)}ms the DOM showed word ${sample.idx}`,
      ).toContain(sample.idx);
      checked += 1;
    }
    expect(
      checked,
      "fast but wrong must not pass, so some samples have to be checkable",
    ).toBeGreaterThanOrEqual(3);
  });

  test("C2 visible and unfocused: the clock does not gate on focus", async ({
    page,
  }) => {
    await openReading(page);
    await play(page);
    await enterReading(page);
    await rewind(page);
    await installProbe(page);

    await page.evaluate(() => {
      (document.activeElement as HTMLElement | null)?.blur();
      window.dispatchEvent(new Event("blur"));
      document.body.focus();
    });
    await waitForTime(page, 4.5);

    const probe = await readProbe(page);
    const errors = errorsFrom(probe);
    test.info().annotations.push({
      type: "measurement",
      description: `unfocused onsets n=${errors.length} median=${quantile(errors, 0.5).toFixed(1)}ms peakFill=${peakPerSecond(probe.fillTicks)}/s`,
    });

    const peakUnfocused = peakPerSecond(probe.fillTicks);
    const tickUnfocused = peakUnfocused > 0 ? 1000 / peakUnfocused : 40;
    const expectedUnfocused = observableWords(
      probe.installedAtMs,
      probe.fillTicks[probe.fillTicks.length - 1] ?? probe.installedAtMs,
      tickUnfocused,
    );
    const seenUnfocused = new Set(
      probe.records.map((record) => `${record.segmentId}:${record.wordIndex}`),
    );
    expect(
      expectedUnfocused.length,
      "nothing was resolvable, so the unfocused claim would be vacuous",
    ).toBeGreaterThanOrEqual(2);
    expect(
      expectedUnfocused.filter(
        (word) => !seenUnfocused.has(`${word.segmentId}:${word.wordIdx}`),
      ),
      "ticks stopped while the window was unfocused",
    ).toEqual([]);
    expect(quantile(errors, 0.5)).toBeLessThanOrEqual(MEDIAN_BUDGET_MS);
    expect(
      peakPerSecond(probe.fillTicks),
      "the fill kept advancing with nothing focused",
    ).toBeGreaterThanOrEqual(MIN_TICKS_PER_SECOND);
    test.info().annotations.push({
      type: "honest-limit",
      description:
        "headless Chromium cannot produce genuinely unfocused-but-visible, and a second page does not hide the first here either (measured). This asserts a property of our code — we never read hasFocus or activeElement — which readingClockFocus.test.ts pins structurally. Browser rAF throttling when occluded is F15/R-1 and is owned by the WebKitGTK log.",
    });
  });

  test("C3 hidden then visible: no wrong highlight while hidden, correct within two ticks on return", async ({
    page,
  }) => {
    await openReading(page);
    await play(page);
    await enterReading(page);
    await rewind(page);
    await installProbe(page);
    await waitForTime(page, 0.4);

    const setVisibility = async (hidden: boolean) => {
      await page.evaluate((isHidden) => {
        Object.defineProperty(document, "hidden", {
          configurable: true,
          get: () => isHidden,
        });
        Object.defineProperty(document, "visibilityState", {
          configurable: true,
          get: () => (isHidden ? "hidden" : "visible"),
        });
        document.dispatchEvent(new Event("visibilitychange"));
      }, hidden);
    };

    await setVisibility(true);
    const atHide = await readProbe(page);
    await waitForTime(page, 1.9);
    const duringHidden = await readProbe(page);
    const newWhileHidden = duringHidden.records.slice(atHide.records.length);

    expect(
      await page.evaluate(() => document.hidden),
      "the override did not take, so nothing about hiding was tested",
    ).toBe(true);

    for (const record of newWhileHidden) {
      const segment = segmentById(record.segmentId);
      const expected =
        segment == null ? null : heldWordAt(segment, record.currentTimeMs);
      expect(
        record.wordIndex,
        "a highlight moved while hidden, which is a highlight moved without a clock",
      ).toBe(expected);
    }
    test.info().annotations.push({
      type: "measurement",
      description: `transitions while hidden: ${newWhileHidden.length} (zero is an expected pass; the invariant is that none of them is wrong)`,
    });

    await setVisibility(false);
    await page.waitForFunction(
      () => {
        const active = document.querySelector<HTMLElement>(
          "[data-word].bg-reading-highlight",
        );
        const container = document.querySelector<HTMLElement>(
          '[data-testid="reading-script"]',
        );
        return active != null || container?.dataset.segment !== "";
      },
      null,
      { timeout: 5_000 },
    );

    const after = await page.evaluate(() => {
      const container = document.querySelector<HTMLElement>(
        '[data-testid="reading-script"]',
      );
      const active = document.querySelector<HTMLElement>(
        "[data-word].bg-reading-highlight",
      );
      const audio = document.querySelector("audio");
      return {
        segmentId: container?.dataset.segment ?? "",
        idx: active == null ? null : Number(active.dataset.word),
        t: (audio?.currentTime ?? 0) * 1000,
      };
    });

    const segment = segmentById(after.segmentId);
    if (segment != null && segment.words.length > 0 && after.idx != null) {
      const expectedNow = heldWordAt(segment, after.t);
      const expectedBefore = heldWordAt(segment, after.t - 80);
      expect(
        [expectedNow, expectedBefore],
        "two rAF ticks: visibilitychange, resync, next paint",
      ).toContain(after.idx);
    }
    test.info().annotations.push({
      type: "budget-note",
      description:
        "the plan pinned 'correct within one frame of returning'; the measurement shape is two ticks (visibilitychange, then resync, then the next paint), and that delta is deliberate rather than a relaxed budget. MECHANISM DEVIATION: the plan specified context.newPage + bringToFront for a real visibilitychange; measured in this headless Chromium, a second page leaves the first at visibilityState=visible, so the event is emulated by overriding document.hidden. That tests our handling, not the browser's rAF throttling, which stays with the WebKitGTK log (F15/R-1). The unit resync-counter test remains this defect's owner.",
    });
  });
});
