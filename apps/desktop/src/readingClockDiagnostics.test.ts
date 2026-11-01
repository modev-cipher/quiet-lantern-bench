describe("readingClockSample", () => {
  const window = {
    chapterIdx: 2,
    mediaMs: 9600,
    ticks: 50,
    resyncCount: 3,
    deltaSumMs: 800,
    deltaMaxMs: 34,
    zeroDeltaTicks: 5,
    elapsedMs: 2000,
    offsetMs: null,
  };

  it("turns tick counts into a rate over the interval that produced them", () => {
    const result = readingClockSample(FOCUSED, window);

    expect(result.tickHz).toBeCloseTo(25, 6);
    expect(result.currentTimeDeltaMs).toBeCloseTo(16, 6);
    expect(result.deltaMaxMs).toBe(34);
    expect(result.zeroDeltaPct).toBeCloseTo(10, 6);
    expect(result.resyncCount).toBe(3);
    expect(result.offsetMs).toBeNull();
  });

  it("reports no rate rather than dividing by a zero interval", () => {
    const result = readingClockSample(FOCUSED, {
      ...window,
      ticks: 0,
      elapsedMs: 0,
    });

    expect(Number.isNaN(result.tickHz)).toBe(true);
    expect(Number.isNaN(result.currentTimeDeltaMs)).toBe(true);
    expect(formatReadingClockSample(result)).toContain("tickHz=n/a");
    expect(formatReadingClockSample(result)).toContain("currentTimeDeltaMs=n/a");
  });

  it("reports the share of ticks the media clock did not move at all", () => {
    const stalled = readingClockSample(FOCUSED, {
      ...window,
      zeroDeltaTicks: 45,
    });

    expect(
      stalled.zeroDeltaPct,
      "a media clock that only advances a few times a second cannot meet the budget however fast rAF runs",
    ).toBeCloseTo(90, 6);
  });
});

import { describe, expect, it } from "vitest";

import {
  decideEmission,
  formatReadingClockSample,
  formatReadingClockScope,
  conditionFor,
  INITIAL_SUPPRESSION,
  readingClockSample,
  formatPlaybackEvent,
  SUPPRESSED_HEARTBEAT_EVERY,
  type ReadingClockSample,
  type ReadingClockScope,
} from "./readingClockDiagnostics";

function sample(overrides: Partial<ReadingClockSample> = {}): ReadingClockSample {
  return {
    condition: "focused",
    chapterIdx: 2,
    mediaMs: 9600,
    tickHz: 59.94,
    currentTimeDeltaMs: 16,
    deltaMaxMs: 34,
    zeroDeltaPct: 0,
    resyncCount: 1,
    offsetMs: -38,
    visibilityState: "visible",
    focused: true,
    ...overrides,
  };
}

function scope(overrides: Partial<ReadingClockScope> = {}): ReadingClockScope {
  return {
    userAgent:
      "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0",
    webkitVersion: "605.1.15",
    platform: "Linux x86_64",
    audioBackend: "pulseaudio 17.0",
    fixtureChapters: 6,
    fixtureWords: 132,
    fixtureShape: "synthetic tones, linear gate",
    ...overrides,
  };
}

describe("formatReadingClockSample", () => {
  it("publishes every field the acceptance item names", () => {
    const line = formatReadingClockSample(sample());

    expect(line).toContain("condition=focused");
    expect(line).toContain("chapterIdx=2");
    expect(line).toContain("mediaMs=9600");
    expect(line).toContain("tickHz=59.9");
    expect(line).toContain("currentTimeDeltaMs=16.0");
    expect(line).toContain("deltaMaxMs=34.0");
    expect(line).toContain("zeroDeltaPct=0");
    expect(line).toContain("resyncCount=1");
    expect(line).toContain("offsetMs=-38");
    expect(line).toContain("visibilityState=visible");
    expect(line).toContain("focused=yes");
  });

  it("carries the chapter and media position so a slope can be recomputed from the log", () => {
    const line = formatReadingClockSample(sample({ chapterIdx: 5, mediaMs: 23760 }));

    expect(line).toContain("chapterIdx=5");
    expect(line).toContain("mediaMs=23760");
  });

  it("admits an unknown chapter rather than implying chapter zero", () => {
    expect(formatReadingClockSample(sample({ chapterIdx: null }))).toContain(
      "chapterIdx=n/a",
    );
  });

  it("keeps a stable field order so two runs diff line by line", () => {
    expect(formatReadingClockSample(sample(), Date.parse("2026-08-12T01:02:03.004Z"))).toBe(
      "at=2026-08-12T01:02:03.004Z condition=focused chapterIdx=2 mediaMs=9600 tickHz=59.9 currentTimeDeltaMs=16.0 deltaMaxMs=34.0 zeroDeltaPct=0 resyncCount=1 offsetMs=-38 visibilityState=visible focused=yes",
    );
  });

  it("says n/a instead of undefined or NaN when a figure was not obtained", () => {
    const line = formatReadingClockSample(
      sample({ offsetMs: null, tickHz: Number.NaN }),
    );

    expect(line).toContain("offsetMs=n/a");
    expect(line).toContain("tickHz=n/a");
    expect(line).not.toContain("undefined");
    expect(line).not.toContain("NaN");
  });

  it("reports a stopped clock as zero rather than as a missing figure", () => {
    const line = formatReadingClockSample(
      sample({ tickHz: 0, currentTimeDeltaMs: 0, resyncCount: 0 }),
    );

    expect(line).toContain("tickHz=0.0");
    expect(line).toContain("resyncCount=0");
    expect(line).not.toContain("n/a");
  });

  it("stays one line and one field per space", () => {
    const line = formatReadingClockSample(
      sample({ visibilityState: "visible\nhidden" }),
    );

    expect(line).not.toContain("\n");
    expect(line.split(" ")).toHaveLength(12);
  });

  it("does not lose an unfocused reading to a truthy default", () => {
    expect(formatReadingClockSample(sample({ focused: false }))).toContain(
      "focused=no",
    );
  });
});

describe("formatReadingClockScope", () => {
  it("records the scope that makes a number reproducible", () => {
    const line = formatReadingClockScope(scope());

    expect(line).toContain("platform=Linux_x86_64");
    expect(line).toContain("webkit=605.1.15");
    expect(line).toContain("audioBackend=pulseaudio_17.0");
    expect(line).toContain("fixtureChapters=6");
    expect(line).toContain("fixtureWords=132");
  });

  it("collapses the spaces inside a user agent so fields stay parseable", () => {
    const line = formatReadingClockScope(scope());

    expect(line).not.toContain("(KHTML, like Gecko)");
    expect(line).toContain("AppleWebKit/605.1.15_(KHTML,_like_Gecko)");
    expect(line.split(" ")).toHaveLength(7);
  });

  it("admits an unknown backend rather than implying a measured one", () => {
    const line = formatReadingClockScope(
      scope({ audioBackend: null, webkitVersion: null }),
    );

    expect(line).toContain("audioBackend=n/a");
    expect(line).toContain("webkit=n/a");
  });

  it("treats an empty string as unknown", () => {
    expect(formatReadingClockScope(scope({ platform: "   " }))).toContain(
      "platform=n/a",
    );
  });
});

const FOCUSED = { hidden: false, visibilityState: "visible", focused: true };

describe("conditionFor", () => {
  it("names the three conditions the acceptance item asks for", () => {
    expect(conditionFor(FOCUSED)).toBe("focused");
    expect(conditionFor({ ...FOCUSED, focused: false })).toBe(
      "visible-unfocused",
    );
    expect(
      conditionFor({ hidden: true, visibilityState: "hidden", focused: false }),
    ).toBe("hidden");
  });

  it("calls a hidden window hidden even if the platform still claims focus", () => {
    expect(
      conditionFor({ hidden: true, visibilityState: "hidden", focused: true }),
      "visibility decides first, because a focused but hidden window is the throttling case",
    ).toBe("hidden");
  });
});


describe("idle suppression", () => {
  const frozen = (mediaMs: number, zeroDeltaPct: number): ReadingClockSample => ({
    condition: "focused",
    chapterIdx: 0,
    mediaMs,
    tickHz: 4,
    currentTimeDeltaMs: 0,
    deltaMaxMs: 0,
    zeroDeltaPct,
    resyncCount: 1,
    offsetMs: null,
    visibilityState: "visible",
    focused: true,
  });

  it("emits the first window and then goes quiet while the media clock is frozen", () => {
    let state = INITIAL_SUPPRESSION;
    const emitted: number[] = [];
    for (let index = 0; index < 10; index += 1) {
      const decision = decideEmission(state, frozen(23016, 100));
      state = decision.next;
      if (decision.emit) emitted.push(index);
    }
    expect(emitted).toEqual([0]);
  });

  it("keeps a heartbeat so silence is distinguishable from a dead reporter", () => {
    let state = INITIAL_SUPPRESSION;
    let heartbeats = 0;
    let run = 0;
    for (let index = 0; index < SUPPRESSED_HEARTBEAT_EVERY * 2 + 1; index += 1) {
      const decision = decideEmission(state, frozen(23016, 100));
      state = decision.next;
      if (decision.heartbeat) {
        heartbeats += 1;
        run = decision.suppressedRun;
      }
    }
    expect(heartbeats).toBe(2);
    expect(run).toBe(SUPPRESSED_HEARTBEAT_EVERY * 2);
  });

  it("emits immediately once the media clock advances again", () => {
    let state = decideEmission(INITIAL_SUPPRESSION, frozen(23016, 100)).next;
    state = decideEmission(state, frozen(23016, 100)).next;
    const resumed = decideEmission(state, frozen(25038, 0));
    expect(resumed.emit).toBe(true);
    expect(resumed.heartbeat).toBe(false);
    expect(resumed.next.suppressed).toBe(0);
  });

  it("never suppresses a window that ticked, even at an unchanged media position", () => {
    const state = decideEmission(INITIAL_SUPPRESSION, frozen(23016, 100)).next;
    expect(decideEmission(state, frozen(23016, 40)).emit).toBe(true);
  });

  it("would have removed the idle share the retro-pass measured without losing a moving sample", () => {
    const total = 491;
    const idle = 151;
    let state = INITIAL_SUPPRESSION;
    let lines = 0;
    for (let index = 0; index < total; index += 1) {
      const sample =
        index < idle ? frozen(23016, 100) : frozen(23016 + index * 2000, 0);
      const decision = decideEmission(state, sample);
      state = decision.next;
      if (decision.emit) lines += 1;
    }
    const moving = total - idle;
    expect(lines).toBeLessThan(total);
    expect(lines).toBeGreaterThanOrEqual(moving);
    expect(idle / total).toBeCloseTo(0.308, 3);
  });

  it("stamps every line with a sortable timestamp", () => {
    expect(formatReadingClockSample(frozen(1, 0))).toMatch(
      /^at=\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z /,
    );
  });
});

describe("idle suppression, hidden view", () => {
  const hidden = (mediaMs: number): ReadingClockSample => ({
    condition: "hidden",
    chapterIdx: 0,
    mediaMs,
    tickHz: 0,
    currentTimeDeltaMs: Number.NaN,
    deltaMaxMs: 0,
    zeroDeltaPct: Number.NaN,
    resyncCount: 1,
    offsetMs: null,
    visibilityState: "hidden",
    focused: false,
  });

  it("stays quiet when rAF stopped entirely, which is the shape the flood actually had", () => {
    let state = decideEmission(INITIAL_SUPPRESSION, hidden(23016)).next;
    const emitted: number[] = [];
    for (let index = 0; index < 10; index += 1) {
      const decision = decideEmission(state, hidden(23016));
      state = decision.next;
      if (decision.emit) emitted.push(index);
    }
    expect(
      emitted.filter((index) => (index + 1) % SUPPRESSED_HEARTBEAT_EVERY !== 0),
      "a hidden window reports no ticks at all, so a predicate that only looks at zeroDeltaPct sees NaN and calls it movement",
    ).toEqual([]);
  });

  it("emits one line for a fourteen second pause at the two second reporter interval", () => {
    const windows = Math.floor(14_000 / 2_000);
    let state = INITIAL_SUPPRESSION;
    let lines = 0;
    for (let index = 0; index < windows; index += 1) {
      const decision = decideEmission(state, hidden(23016));
      state = decision.next;
      if (decision.emit) lines += 1;
    }
    expect(windows).toBe(7);
    expect(lines, "the pause is reported once, not once per window").toBe(1);
  });
});

describe("playback event lines", () => {
  it("keeps one greppable shape for every event", () => {
    expect(
      formatPlaybackEvent("pause", 12.3456, Date.parse("2026-08-12T01:02:03.004Z")),
    ).toBe("media at=2026-08-12T01:02:03.004Z event=pause mediaMs=12346");
  });

  it("says n/a rather than NaN when the clock never produced a position", () => {
    const line = formatPlaybackEvent("error", Number.NaN, 0);
    expect(line).toContain("mediaMs=n/a");
    expect(line).not.toContain("NaN");
    expect(formatPlaybackEvent("exit", null, 0)).toContain("mediaMs=n/a");
  });

  it("reads as one line under the host prefix, not a doubled one", () => {
    expect(`reading clock: ${formatPlaybackEvent("exit", 1, 0)}`).not.toContain(
      "reading clock: reading",
    );
  });

  it("shares the media prefix so one grep finds the whole channel", () => {
    for (const event of ["pause", "error", "exit"] as const) {
      expect(formatPlaybackEvent(event, 1, 0)).toMatch(
        /^media at=\S+ event=\w+ mediaMs=\d+$/,
      );
    }
  });
});
