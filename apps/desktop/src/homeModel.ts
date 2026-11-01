import type { AudioShelfItem } from "./audioShelfModel";
import { canPlay } from "./audioShelfModel";
import { canFollowAlong } from "./readingOverlayModel";
import type { ReadingIndex } from "./readingModel";
import { listenedOf, type ListenedMap } from "./listeningProgress";

export const RECENT_KEY = "txtcaster.listened.recent";

export const MAX_RECENT = 24;

export const JUMP_COUNT = 4;

export function parseRecent(raw: string | null): string[] {
  if (raw == null) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];
  const seen = new Set<string>();
  const ids: string[] = [];
  for (const value of parsed) {
    if (typeof value !== "string" || value === "" || seen.has(value)) continue;
    seen.add(value);
    ids.push(value);
  }
  return ids.slice(0, MAX_RECENT);
}

export function withRecent(ids: string[], id: string): string[] {
  if (id === "") return ids;
  if (ids[0] === id) return ids;
  return [id, ...ids.filter((known) => known !== id)].slice(0, MAX_RECENT);
}

export function loadRecent(): string[] {
  try {
    return parseRecent(localStorage.getItem(RECENT_KEY));
  } catch {
    return [];
  }
}

export function noteRecent(id: string): void {
  try {
    const current = loadRecent();
    const next = withRecent(current, id);
    if (next === current) return;
    localStorage.setItem(RECENT_KEY, JSON.stringify(next));
  } catch {
    return;
  }
}

export interface HomeSelection {
  hero: AudioShelfItem | null;
  heroListened: number | null;
  heroReadAlong: boolean;
  jump: AudioShelfItem[];
}

export function homeSelection(
  shelf: AudioShelfItem[],
  recent: string[],
  listened: ListenedMap,
): HomeSelection {
  const playable = shelf.filter(canPlay);
  const byId = new Map(playable.map((item) => [item.id, item]));
  const ordered: AudioShelfItem[] = [];
  for (const id of recent) {
    const item = byId.get(id);
    if (item) ordered.push(item);
  }
  for (const item of [...playable].sort((a, b) => b.createdAt - a.createdAt)) {
    if (!ordered.includes(item)) ordered.push(item);
  }
  const hero =
    ordered.find((item) => canFollowAlong(item)) ?? ordered[0] ?? null;
  const jump: AudioShelfItem[] = [];
  const seenBooks = new Set(hero?.bookId ? [hero.bookId] : []);
  for (const item of ordered.filter((entry) => entry !== hero)) {
    if (jump.length >= JUMP_COUNT) break;
    if (item.bookId !== "" && seenBooks.has(item.bookId)) continue;
    if (item.bookId !== "") seenBooks.add(item.bookId);
    jump.push(item);
  }
  return {
    hero,
    heroListened: hero ? listenedOf(listened, hero.id) : null,
    heroReadAlong: hero != null && canFollowAlong(hero),
    jump,
  };
}

export function formatPosition(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  const mm = hours > 0 ? String(minutes).padStart(2, "0") : String(minutes);
  const ss = String(seconds).padStart(2, "0");
  return hours > 0 ? `${hours}:${mm}:${ss}` : `${mm}:${ss}`;
}

export function positionLine(
  index: ReadingIndex,
  fraction: number | null,
): string | null {
  if (index.chapters.length === 0 || !(index.totalMs > 0)) return null;
  const at =
    fraction == null || fraction <= 0 || fraction >= 1
      ? 0
      : fraction * index.totalMs;
  const chapter =
    [...index.chapters]
      .reverse()
      .find((entry) => at >= entry.startMs && at < entry.endMs) ??
    index.chapters[0];
  if (chapter == null) return null;
  const name = chapter.title.trim();
  const label = `Chapter ${chapter.idx + 1}${name ? ` · ${name}` : ""}`;
  return at > 0 ? `${label} · ${formatPosition(at)}` : label;
}
