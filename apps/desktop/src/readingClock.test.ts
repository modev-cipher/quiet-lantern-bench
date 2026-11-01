import { describe, expect, it } from "vitest";

import golden from "../e2e/fixtures/reading-timeline.golden.json";
import {
  createReadingClock,
  SAFETY_TICK_MS,
  type ReadingClockScheduler,
} from "./readingClock";
import {
  resolveCursor,
  type ReadingChapter,
  type ReadingCursor,
} from "./readingModel";

const chapter = (golden.chapters as ReadingChapter[])[0];

function harness() {
  const counts = {
    frames: 0,
    cancelledFrames: 0,
    intervals: 0,
    stoppedIntervals: 0,
    visibilityRemovals: 0,
    triggerRemovals: 0,
    memoFlushes: 0,
  };
  const state = { hidden: false, time: 0 };
  const cursors: Array<{ cursor: ReadingCursor; stale: boolean }> = [];
  let frameCallback: (() => void) | null = null;
  let intervalCallback: (() => void) | null = null;
  let visibilityCallback: (() => void) | null = null;
  let triggerCallback: (() => void) | null = null;
  let intervalMs: number | null = null;

  const scheduler: ReadingClockScheduler = {
    requestFrame(callback) {
      counts.frames += 1;
      frameCallback = callback;
      return counts.frames;
    },
    cancelFrame() {
      counts.cancelledFrames += 1;
      frameCallback = null;
    },
    startInterval(callback, ms) {
      counts.intervals += 1;
      intervalCallback = callback;
      intervalMs = ms;
      return counts.intervals;
    },
    stopInterval() {
      counts.stoppedIntervals += 1;
      intervalCallback = null;
    },
    onVisibilityChange(callback) {
      visibilityCallback = callback;
      return () => {
        counts.visibilityRemovals += 1;
        visibilityCallback = null;
      };
    },
    isHidden() {
      return state.hidden;
    },
  };

  const clock = createReadingClock({
    scheduler,
    getTimeMs: () => state.time,
    getChapter: () => chapter,
    onCursor: (cursor, stale) => cursors.push({ cursor, stale }),
    subscribeResyncTriggers: (listener) => {
      triggerCallback = listener;
      return () => {
        counts.triggerRemovals += 1;
        triggerCallback = null;
      };
    },
    flushMemo: () => {
      counts.memoFlushes += 1;
    },
  });

  return {
    clock,
    counts,
    state,
    cursors,
    intervalMs: () => intervalMs,
    runFrame: () => frameCallback?.(),
    runSafetyTick: () => intervalCallback?.(),
    fireVisibility: () => visibilityCallback?.(),
    fireTrigger: () => triggerCallback?.(),
    hasFrame: () => frameCallback != null,
    hasInterval: () => intervalCallback != null,
    hasVisibility: () => visibilityCallback != null,
    frameCallback: () => frameCallback,
  };
}

describe("the reading clock ticks once per frame", () => {
  it("emits one cursor per frame and reschedules itself", () => {
    const h = harness();
    h.clock.start();
    const afterStart = h.cursors.length;

    h.state.time = 200;
    h.runFrame();
    h.state.time = 400;
    h.runFrame();

    expect(h.cursors.length - afterStart).toBe(2);
    expect(h.counts.frames).toBe(3);
  });

  it("registers the safety tick beside the frame loop at 250ms", () => {
    const h = harness();
    h.clock.start();

    expect(h.counts.intervals).toBe(1);
    expect(h.intervalMs()).toBe(SAFETY_TICK_MS);
    expect(SAFETY_TICK_MS).toBe(250);
  });

  it("advances through the same entry point when only the safety tick fires", () => {
    const h = harness();
    h.clock.start();
    const baseline = h.clock.resyncCount();

    h.state.time = 200;
    h.runSafetyTick();
    const first = h.cursors[h.cursors.length - 1];
    h.state.time = 450;
    h.runSafetyTick();
    const second = h.cursors[h.cursors.length - 1];

    expect(first.cursor.wordIdx).not.toBeNull();
    expect(second.cursor.wordIdx).not.toBe(first.cursor.wordIdx);
    expect(
      h.clock.resyncCount(),
      "a forward tick is incremental, so it must not resync",
    ).toBe(baseline);
  });
});

describe("the reading clock resyncs exactly once per trigger", () => {
  it("counts one resync for the initial resolve and nothing for forward ticks", () => {
    const h = harness();
    h.clock.start();

    expect(h.clock.resyncCount()).toBe(1);

    h.state.time = 100;
    h.runFrame();
    h.state.time = 200;
    h.runFrame();

    expect(h.clock.resyncCount()).toBe(1);
  });

  it("counts one resync per store trigger", () => {
    const h = harness();
    h.clock.start();
    const baseline = h.clock.resyncCount();

    h.state.time = 1500;
    h.fireTrigger();

    expect(h.clock.resyncCount()).toBe(baseline + 1);

    h.fireTrigger();

    expect(h.clock.resyncCount()).toBe(baseline + 2);
  });

  it("counts one resync when the page comes back into view", () => {
    const h = harness();
    h.clock.start();
    const baseline = h.clock.resyncCount();

    h.state.hidden = true;
    h.fireVisibility();

    expect(
      h.clock.resyncCount(),
      "going hidden resolves nothing, so it cannot claim a resync",
    ).toBe(baseline);

    h.state.hidden = false;
    h.state.time = 1500;
    h.fireVisibility();

    expect(h.clock.resyncCount()).toBe(baseline + 1);
  });

  it("counts one resync when the chapter under it changes", () => {
    let current = (golden.chapters as ReadingChapter[])[0];
    const scheduler: ReadingClockScheduler = {
      requestFrame: () => 1,
      cancelFrame: () => {},
      startInterval: () => 1,
      stopInterval: () => {},
      onVisibilityChange: () => () => {},
      isHidden: () => false,
    };
    const holder: { fire: (() => void) | null } = { fire: null };
    const clock = createReadingClock({
      scheduler: {
        ...scheduler,
        requestFrame: (callback) => {
          holder.fire = callback;
          return 1;
        },
      },
      getTimeMs: () => 100,
      getChapter: () => current,
      onCursor: () => {},
      subscribeResyncTriggers: () => () => {},
      flushMemo: () => {},
    });
    clock.start();
    const baseline = clock.resyncCount();

    holder.fire?.();

    expect(clock.resyncCount()).toBe(baseline);

    current = (golden.chapters as ReadingChapter[])[1];
    holder.fire?.();

    expect(clock.resyncCount()).toBe(baseline + 1);
  });

  it("does not claim a resync it could not perform", () => {
    const h = harness();
    h.clock.start();
    const baseline = h.clock.resyncCount();

    h.state.time = Number.NaN;
    h.fireTrigger();

    expect(h.clock.resyncCount()).toBe(baseline);
    expect(h.clock.isStale()).toBe(true);
  });
});

describe("the reading clock holds rather than guesses", () => {
  it("marks the cursor stale while hidden instead of moving it", () => {
    const h = harness();
    h.clock.start();
    h.state.time = 200;
    h.runFrame();
    const before = h.cursors[h.cursors.length - 1].cursor;

    h.state.hidden = true;
    h.state.time = 1800;
    h.runFrame();
    const during = h.cursors[h.cursors.length - 1];

    expect(during.stale).toBe(true);
    expect(during.cursor).toEqual(before);
  });

  it("restores the correct cursor on return without an intermediate wrong one", () => {
    const h = harness();
    h.clock.start();
    h.state.time = 200;
    h.runFrame();
    h.state.hidden = true;
    h.state.time = 1800;
    h.runFrame();
    h.runSafetyTick();

    const wrongWhileHidden = h.cursors
      .slice(2)
      .filter((entry) => !entry.stale);
    h.state.hidden = false;
    h.fireVisibility();
    const after = h.cursors[h.cursors.length - 1];

    expect(
      wrongWhileHidden,
      "every cursor emitted while hidden must carry the stale flag",
    ).toHaveLength(0);
    expect(after.stale).toBe(false);
    expect(
      after.cursor,
      "the resync resolves from the audio clock, not from the held cursor",
    ).toEqual(resolveCursor(chapter, 1800));
  });

  it("treats a missing audio element exactly as it treats a hidden page", () => {
    const h = harness();
    h.clock.start();
    h.state.time = 200;
    h.runFrame();
    const before = h.cursors[h.cursors.length - 1].cursor;

    h.state.time = Number.NaN;
    h.runFrame();
    const after = h.cursors[h.cursors.length - 1];

    expect(after.stale).toBe(true);
    expect(after.cursor).toEqual(before);
  });
});

describe("the reading clock reports its own numbers", () => {
  it("counts a tick per advance and the gap between two clock reads", () => {
    const h = harness();
    h.clock.start();
    const base = h.clock.stats();

    h.state.time = 200;
    h.runFrame();
    h.state.time = 240;
    h.runFrame();
    const stats = h.clock.stats();

    expect(stats.ticks - base.ticks).toBe(2);
    expect(stats.lastDeltaMs).toBe(40);
    expect(stats.resyncs).toBe(h.clock.resyncCount());
  });

  it("counts no tick for a frame that could not read the clock", () => {
    const h = harness();
    h.clock.start();
    h.state.time = 200;
    h.runFrame();
    const before = h.clock.stats().ticks;

    h.state.time = Number.NaN;
    h.runFrame();
    h.state.hidden = true;
    h.runFrame();

    expect(
      h.clock.stats().ticks,
      "a held cursor is not a tick; counting it would inflate the reported tick rate",
    ).toBe(before);
  });

  it("reports the offset between the clock and the word it just highlighted", () => {
    const h = harness();
    h.clock.start();

    h.state.time = 235;
    h.runFrame();

    expect(
      h.clock.stats().lastWordOffsetMs,
      "word 1 of the golden's first segment starts at 200ms",
    ).toBeCloseTo(35, 6);

    h.state.time = 470;
    h.runFrame();

    expect(h.clock.stats().lastWordOffsetMs).toBeCloseTo(20, 6);
  });

  it("keeps the offset of the last word rather than recomputing it every tick", () => {
    const h = harness();
    h.clock.start();
    h.state.time = 235;
    h.runFrame();
    const first = h.clock.stats().lastWordOffsetMs;

    h.state.time = 300;
    h.runFrame();

    expect(
      h.clock.stats().lastWordOffsetMs,
      "the offset belongs to the transition, not to the tick that happened to follow it",
    ).toBe(first);
  });

  it("has no delta to report before two reads", () => {
    const h = harness();
    h.clock.start();

    expect(Number.isNaN(h.clock.stats().lastDeltaMs)).toBe(true);
  });
});

describe("the reading clock leaves nothing running", () => {
  it("releases all five subscriptions on stop", () => {
    const h = harness();
    h.clock.start();
    h.clock.stop();

    expect(h.counts.cancelledFrames).toBe(1);
    expect(h.counts.stoppedIntervals).toBe(1);
    expect(h.counts.visibilityRemovals).toBe(1);
    expect(h.counts.triggerRemovals).toBe(1);
    expect(h.counts.memoFlushes).toBe(1);
    expect(h.hasFrame()).toBe(false);
    expect(h.hasInterval()).toBe(false);
    expect(h.hasVisibility()).toBe(false);
  });

  it("is idempotent, because StrictMode double-invokes effects", () => {
    const h = harness();
    h.clock.start();
    h.clock.stop();
    h.clock.stop();
    h.clock.stop();

    expect(h.counts.cancelledFrames).toBe(1);
    expect(h.counts.stoppedIntervals).toBe(1);
    expect(h.counts.visibilityRemovals).toBe(1);
    expect(h.counts.triggerRemovals).toBe(1);
    expect(h.counts.memoFlushes).toBe(1);
  });

  it("ignores a second start, because two loops would double every tick", () => {
    const h = harness();
    h.clock.start();
    h.clock.start();

    expect(h.counts.frames).toBe(1);
    expect(h.counts.intervals).toBe(1);
  });

  it("stops ticking after stop even if a queued frame still fires", () => {
    const h = harness();
    h.clock.start();
    const queued = h.frameCallback();
    const before = h.cursors.length;
    h.clock.stop();
    h.state.time = 1500;
    queued?.();

    expect(
      h.cursors.length,
      "a frame the scheduler had already queued must not resurrect the loop",
    ).toBe(before);
    expect(h.counts.frames).toBe(1);
  });
});
