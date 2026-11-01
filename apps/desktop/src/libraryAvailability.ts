import { canPlay, type AudioShelfItem } from "./audioShelfModel";

export interface BookAvailability {
  line: string | null;
  tone: "ready" | "stale" | null;
  playable: AudioShelfItem | null;
}

export const NO_AUDIO: BookAvailability = {
  line: null,
  tone: null,
  playable: null,
};

export function bookAvailability(
  bookId: string,
  shelf: AudioShelfItem[],
): BookAvailability {
  const owned = shelf.filter((item) => item.bookId === bookId);
  if (owned.length === 0) return NO_AUDIO;
  const playable = owned
    .filter(canPlay)
    .sort((a, b) => b.createdAt - a.createdAt);
  const stale = owned.filter((item) => item.stale).length;
  if (playable.length === 0) {
    return {
      line:
        stale > 0
          ? "Audio stale — re-render available"
          : "Audio not playable yet",
      tone: "stale",
      playable: null,
    };
  }
  const counts =
    playable.length === 1 ? "Audio ready" : `${playable.length} casts ready`;
  return {
    line: stale > 0 ? `${counts} · ${stale} stale` : counts,
    tone: "ready",
    playable: playable[0] ?? null,
  };
}

export function partitionOrphans(shelf: AudioShelfItem[]): {
  owned: AudioShelfItem[];
  orphans: AudioShelfItem[];
} {
  const owned: AudioShelfItem[] = [];
  const orphans: AudioShelfItem[] = [];
  for (const item of shelf) {
    (item.bookId === "" ? orphans : owned).push(item);
  }
  return { owned, orphans };
}
