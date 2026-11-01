import { describe, expect, it } from "vitest";

import {
  FADE_MS,
  SLEEP_OFF,
  startSleep,
  tickSleep,
  type SleepTimer,
} from "./sleepTimerModel";

const CHAPTERS = [
  { chapterId: "a", startMs: 0 },
  { chapterId: "b", startMs: 60_000 },
];

describe("startSleep", () => {
  it("counts a minutes timer in milliseconds of listening", () => {
    expect(
      startSleep(15, { currentMs: 0, chapters: CHAPTERS, durationMs: 1 }),
    ).toEqual({
      kind: "minutes",
      minutes: 15,
      remainingMs: 15 * 60_000,
    });
  });

  it("ends a chapter timer at the next chapter's start", () => {
    expect(
      startSleep("chapter", {
        currentMs: 10_000,
        chapters: CHAPTERS,
        durationMs: 90_000,
      }),
    ).toEqual({ kind: "chapterEnd", untilMs: 60_000 });
  });

  it("ends the last chapter's timer at the end of the cast", () => {
    expect(
      startSleep("chapter", {
        currentMs: 70_000,
        chapters: CHAPTERS,
        durationMs: 90_000,
      }),
    ).toEqual({ kind: "chapterEnd", untilMs: 90_000 });
  });

  it("turns off", () => {
    expect(
      startSleep("off", { currentMs: 0, chapters: CHAPTERS, durationMs: 1 }),
    ).toEqual(SLEEP_OFF);
  });
});

const minutes = (remainingMs: number): SleepTimer => ({
  kind: "minutes",
  minutes: 15,
  remainingMs,
});

describe("tickSleep: minutes", () => {
  it("only spends time while playing", () => {
    const paused = tickSleep(minutes(60_000), {
      elapsedMs: 500,
      playing: false,
      currentMs: 0,
      prevMs: 0,
    });
    expect(paused.timer).toEqual(minutes(60_000));
    const playing = tickSleep(minutes(60_000), {
      elapsedMs: 500,
      playing: true,
      currentMs: 0,
      prevMs: 0,
    });
    expect(playing.timer).toEqual(minutes(59_500));
    expect(playing.fade).toBe(1);
    expect(playing.expire).toBe(false);
  });

  it("fades over the last ten seconds", () => {
    const step = tickSleep(minutes(FADE_MS / 2 + 250), {
      elapsedMs: 250,
      playing: true,
      currentMs: 0,
      prevMs: 0,
    });
    expect(step.fade).toBeCloseTo(0.5, 5);
  });

  it("expires at zero, turning itself off", () => {
    const step = tickSleep(minutes(200), {
      elapsedMs: 250,
      playing: true,
      currentMs: 0,
      prevMs: 0,
    });
    expect(step.expire).toBe(true);
    expect(step.timer).toEqual(SLEEP_OFF);
    expect(step.fade).toBe(1);
  });
});

describe("tickSleep: end of chapter", () => {
  const timer: SleepTimer = { kind: "chapterEnd", untilMs: 60_000 };

  it("fades as narration approaches the chapter's end", () => {
    const step = tickSleep(timer, {
      elapsedMs: 250,
      playing: true,
      currentMs: 55_000,
      prevMs: 54_750,
    });
    expect(step.fade).toBeCloseTo(0.5, 5);
    expect(step.expire).toBe(false);
  });

  it("expires when playback runs across the end", () => {
    const step = tickSleep(timer, {
      elapsedMs: 250,
      playing: true,
      currentMs: 60_100,
      prevMs: 59_850,
    });
    expect(step.expire).toBe(true);
    expect(step.timer).toEqual(SLEEP_OFF);
  });

  it("cancels quietly when the listener seeks past the end", () => {
    const step = tickSleep(timer, {
      elapsedMs: 250,
      playing: true,
      currentMs: 75_000,
      prevMs: 20_000,
    });
    expect(step.expire).toBe(false);
    expect(step.timer).toEqual(SLEEP_OFF);
    expect(step.fade).toBe(1);
  });
});

describe("tickSleep: off", () => {
  it("never fades or expires", () => {
    expect(
      tickSleep(SLEEP_OFF, {
        elapsedMs: 250,
        playing: true,
        currentMs: 0,
        prevMs: 0,
      }),
    ).toEqual({
      timer: SLEEP_OFF,
      fade: 1,
      expire: false,
    });
  });
});
