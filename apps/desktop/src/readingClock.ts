import {
  advanceCursor,
  OUTSIDE_CURSOR,
  resolveCursor,
  type ReadingChapter,
  type ReadingCursor,
} from "./readingModel";

export const SAFETY_TICK_MS = 250;

export interface ReadingClockScheduler {
  requestFrame(callback: () => void): number;
  cancelFrame(handle: number): void;
  startInterval(callback: () => void, ms: number): number;
  stopInterval(handle: number): void;
  onVisibilityChange(callback: () => void): () => void;
  isHidden(): boolean;
}

export interface ReadingClockOptions {
  scheduler: ReadingClockScheduler;
  getTimeMs: () => number;
  getChapter: () => ReadingChapter | null;
  onCursor: (cursor: ReadingCursor, stale: boolean, timeMs: number) => void;
  subscribeResyncTriggers: (listener: () => void) => () => void;
  flushMemo: () => void;
  safetyTickMs?: number;
}

export interface ReadingClockStats {
  ticks: number;
  resyncs: number;
  lastDeltaMs: number;
  deltaSumMs: number;
  deltaMaxMs: number;
  zeroDeltaTicks: number;
  lastWordOffsetMs: number;
  lastTimeMs: number;
}

export interface ReadingClock {
  start: () => void;
  stop: () => void;
  resyncCount: () => number;
  isStale: () => boolean;
  stats: () => ReadingClockStats;
  takeWindow: () => ReadingClockStats;
}

export function createReadingClock(
  options: ReadingClockOptions,
): ReadingClock {
  let frame: number | null = null;
  let interval: number | null = null;
  let unsubscribeVisibility: (() => void) | null = null;
  let unsubscribeTriggers: (() => void) | null = null;
  let cursor: ReadingCursor = OUTSIDE_CURSOR;
  let chapter: ReadingChapter | null = null;
  let stale = false;
  let resyncs = 0;
  let running = false;
  let lastTime = Number.NaN;
  let ticks = 0;
  let lastDelta = Number.NaN;
  let lastWordOffset = Number.NaN;
  let deltaSum = 0;
  let deltaMax = 0;
  let zeroDeltas = 0;
  let lastWordKey = "";

  function noteWordOffset(): void {
    if (chapter == null || cursor.segmentIdx == null || cursor.wordIdx == null) {
      return;
    }
    const key = `${chapter.chapterId}:${cursor.segmentIdx}:${cursor.wordIdx}`;
    if (key === lastWordKey) return;
    lastWordKey = key;
    const word =
      chapter.segments[cursor.segmentIdx]?.words[cursor.wordIdx] ?? null;
    if (word == null || !Number.isFinite(lastTime)) return;
    lastWordOffset = lastTime - word.startMs;
  }

  function emit(): void {
    noteWordOffset();
    options.onCursor(cursor, stale, lastTime);
  }

  function hold(): void {
    stale = true;
    emit();
  }

  function resolveAt(t: number): void {
    resyncs += 1;
    lastTime = t;
    cursor = chapter == null ? OUTSIDE_CURSOR : resolveCursor(chapter, t);
    stale = false;
    emit();
  }

  function resync(): void {
    const t = options.getTimeMs();
    if (!Number.isFinite(t)) {
      hold();
      return;
    }
    resolveAt(t);
  }

  function step(): void {
    if (!running) return;
    const current = options.getChapter();
    if (current !== chapter) {
      chapter = current;
      resync();
      return;
    }
    if (options.scheduler.isHidden()) {
      hold();
      return;
    }
    const t = options.getTimeMs();
    if (!Number.isFinite(t)) {
      hold();
      return;
    }
    if (chapter == null) {
      cursor = OUTSIDE_CURSOR;
      stale = false;
      emit();
      return;
    }
    const next = advanceCursor(cursor, chapter, t);
    if (next == null) {
      resolveAt(t);
      return;
    }
    lastDelta = Number.isFinite(lastTime) ? t - lastTime : Number.NaN;
    if (Number.isFinite(lastDelta)) {
      deltaSum += lastDelta;
      deltaMax = Math.max(deltaMax, lastDelta);
      if (lastDelta === 0) zeroDeltas += 1;
    }
    lastTime = t;
    ticks += 1;
    cursor = next;
    stale = false;
    emit();
  }

  function onFrame(): void {
    frame = null;
    step();
    if (running) {
      frame = options.scheduler.requestFrame(onFrame);
    }
  }

  function onVisibility(): void {
    if (options.scheduler.isHidden()) {
      hold();
      return;
    }
    resync();
  }

  return {
    start() {
      if (running) return;
      running = true;
      chapter = options.getChapter();
      resync();
      frame = options.scheduler.requestFrame(onFrame);
      interval = options.scheduler.startInterval(
        step,
        options.safetyTickMs ?? SAFETY_TICK_MS,
      );
      unsubscribeVisibility = options.scheduler.onVisibilityChange(onVisibility);
      unsubscribeTriggers = options.subscribeResyncTriggers(resync);
    },
    stop() {
      if (!running) return;
      running = false;
      if (frame != null) {
        options.scheduler.cancelFrame(frame);
        frame = null;
      }
      if (interval != null) {
        options.scheduler.stopInterval(interval);
        interval = null;
      }
      unsubscribeVisibility?.();
      unsubscribeVisibility = null;
      unsubscribeTriggers?.();
      unsubscribeTriggers = null;
      options.flushMemo();
    },
    resyncCount() {
      return resyncs;
    },
    isStale() {
      return stale;
    },
    takeWindow() {
      const window = {
        ticks,
        resyncs,
        lastDeltaMs: lastDelta,
        deltaSumMs: deltaSum,
        deltaMaxMs: deltaMax,
        zeroDeltaTicks: zeroDeltas,
        lastWordOffsetMs: lastWordOffset,
        lastTimeMs: lastTime,
      };
      deltaMax = 0;
      return window;
    },
    stats() {
      return {
        ticks,
        resyncs,
        lastDeltaMs: lastDelta,
        deltaSumMs: deltaSum,
        deltaMaxMs: deltaMax,
        zeroDeltaTicks: zeroDeltas,
        lastWordOffsetMs: lastWordOffset,
        lastTimeMs: lastTime,
      };
    },
  };
}
