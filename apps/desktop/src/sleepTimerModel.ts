import { nextChapterTarget } from "./playerCapsuleModel";

export const FADE_MS = 10_000;
const NATURAL_STEP_MS = 3_000;

export type SleepChoice = "off" | 15 | 30 | 45 | "chapter";

export type SleepTimer =
  | { kind: "off" }
  | { kind: "minutes"; minutes: 15 | 30 | 45; remainingMs: number }
  | { kind: "chapterEnd"; untilMs: number };

export const SLEEP_OFF: SleepTimer = { kind: "off" };

export function startSleep(
  choice: SleepChoice,
  ctx: {
    currentMs: number;
    chapters: readonly { chapterId: string; startMs: number }[];
    durationMs: number;
  },
): SleepTimer {
  if (choice === "off") return SLEEP_OFF;
  if (choice === "chapter") {
    const next = nextChapterTarget(ctx.chapters, ctx.currentMs / 1000);
    return { kind: "chapterEnd", untilMs: next ?? ctx.durationMs };
  }
  return { kind: "minutes", minutes: choice, remainingMs: choice * 60_000 };
}

export function tickSleep(
  timer: SleepTimer,
  tick: {
    elapsedMs: number;
    playing: boolean;
    currentMs: number;
    prevMs: number;
  },
): { timer: SleepTimer; fade: number; expire: boolean } {
  if (timer.kind === "off") return { timer, fade: 1, expire: false };
  if (timer.kind === "minutes") {
    if (!tick.playing) {
      return { timer, fade: fadeFor(timer.remainingMs), expire: false };
    }
    const remainingMs = Math.max(0, timer.remainingMs - tick.elapsedMs);
    if (remainingMs === 0) return { timer: SLEEP_OFF, fade: 1, expire: true };
    return {
      timer: { ...timer, remainingMs },
      fade: fadeFor(remainingMs),
      expire: false,
    };
  }
  const remainingMs = timer.untilMs - tick.currentMs;
  if (remainingMs <= 0) {
    const natural = tick.currentMs - tick.prevMs <= NATURAL_STEP_MS;
    return { timer: SLEEP_OFF, fade: 1, expire: natural };
  }
  return { timer, fade: fadeFor(remainingMs), expire: false };
}

function fadeFor(remainingMs: number): number {
  return Math.min(1, Math.max(0, remainingMs / FADE_MS));
}
