import { formatTime } from "./audioPlayerModel";

export const CAPSULE_RATES = [0.75, 1, 1.25, 1.5, 1.75, 2] as const;
export const RESTART_GRACE_S = 3;

type ChapterStart = { chapterId: string; startMs: number };

export function chapterTicks(
  chapters: readonly ChapterStart[],
  durationS: number,
): { chapterId: string; fraction: number }[] {
  if (!Number.isFinite(durationS) || durationS <= 0) return [];
  const totalMs = durationS * 1000;
  return chapters
    .slice(1)
    .filter((chapter) => chapter.startMs > 0 && chapter.startMs < totalMs)
    .map((chapter) => ({
      chapterId: chapter.chapterId,
      fraction: chapter.startMs / totalMs,
    }));
}

export function chapterAt<T extends ChapterStart>(
  chapters: readonly T[],
  timeS: number,
): T | null {
  const ms = timeS * 1000;
  let found: T | null = null;
  for (const chapter of chapters) {
    if (chapter.startMs <= ms) found = chapter;
    else break;
  }
  return found ?? chapters[0] ?? null;
}

export function prevChapterTarget(
  chapters: readonly ChapterStart[],
  timeS: number,
): number | null {
  const current = chapterAt(chapters, timeS);
  if (current == null) return null;
  if (timeS * 1000 - current.startMs > RESTART_GRACE_S * 1000) {
    return current.startMs;
  }
  const at = chapters.indexOf(current);
  return chapters[Math.max(0, at - 1)].startMs;
}

export function nextChapterTarget(
  chapters: readonly ChapterStart[],
  timeS: number,
): number | null {
  const current = chapterAt(chapters, timeS);
  if (current == null) return null;
  return chapters[chapters.indexOf(current) + 1]?.startMs ?? null;
}

export function remainingLabel(currentS: number, durationS: number): string {
  if (!Number.isFinite(durationS) || durationS <= 0) return "";
  return `−${formatTime(Math.max(0, durationS - currentS))}`;
}

export function chapterSpan(
  chapters: readonly ChapterStart[],
  durationS: number,
  timeS: number,
): { startS: number; endS: number } | null {
  if (!Number.isFinite(durationS) || durationS <= 0) return null;
  const current = chapterAt(chapters, timeS);
  if (current == null) return null;
  const following = chapters[chapters.indexOf(current) + 1];
  return {
    startS: current.startMs / 1000,
    endS:
      following != null
        ? Math.min(durationS, following.startMs / 1000)
        : durationS,
  };
}

export function chapterLengths(
  chapters: readonly ChapterStart[],
  durationS: number,
): number[] {
  if (!Number.isFinite(durationS) || durationS <= 0) return [];
  return chapters.map((chapter, at) => {
    const endMs = chapters[at + 1]?.startMs ?? durationS * 1000;
    return Math.max(0, (endMs - chapter.startMs) / 1000);
  });
}

export type PlayerScope = "chapter" | "book";

export const PLAYER_SCOPE_KEY = "txtcaster.player.scope";

export function loadPlayerScope(): PlayerScope {
  try {
    return localStorage.getItem(PLAYER_SCOPE_KEY) === "book"
      ? "book"
      : "chapter";
  } catch {
    return "chapter";
  }
}

export function storePlayerScope(scope: PlayerScope): void {
  try {
    localStorage.setItem(PLAYER_SCOPE_KEY, scope);
  } catch {
    return;
  }
}
