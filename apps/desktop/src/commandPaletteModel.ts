import type { AudioShelfItem } from "./audioShelfModel";
import type { View } from "./views";

export const PALETTE_GROUP_LIMIT = 5;

export type PaletteGroup = "books" | "casts" | "chapters" | "go" | "actions";

export const PALETTE_GROUP_ORDER: readonly PaletteGroup[] = [
  "books",
  "casts",
  "chapters",
  "go",
  "actions",
];

export type PaletteTarget =
  | {
      kind: "book";
      bookId: string;
      hasPages: boolean;
      playable: AudioShelfItem | null;
    }
  | { kind: "cast"; item: AudioShelfItem }
  | { kind: "chapter"; startMs: number }
  | { kind: "view"; view: View }
  | { kind: "action"; action: "tutor" | "report" | "activity" };

export type PaletteEntry = {
  group: PaletteGroup;
  id: string;
  label: string;
  detail?: string;
  target: PaletteTarget;
};

export type RankedGroup = { group: PaletteGroup; entries: PaletteEntry[] };

const COMBINING_MARKS = /[̀-ͯ]/g;
const ARABIC_MARKS = /[ؐ-ًؚ-ٰٟۖ-ۭـ]/g;
const ALEF_FORMS = /[آأإٱ]/g;

export function normalizeForSearch(text: string): string {
  return text
    .normalize("NFKD")
    .replace(COMBINING_MARKS, "")
    .replace(ARABIC_MARKS, "")
    .replace(ALEF_FORMS, "ا")
    .replace(/ة/g, "ه")
    .replace(/ى/g, "ي")
    .toLowerCase()
    .replace(/\s+/g, " ")
    .trim();
}

function scoreText(query: string, text: string): number {
  if (text.length === 0) return 0;
  if (text.startsWith(query)) return 4;
  if (text.includes(` ${query}`)) return 3;
  if (text.includes(query)) return 2;
  let at = 0;
  for (const char of query) {
    const found = text.indexOf(char, at);
    if (found < 0) return 0;
    at = found + 1;
  }
  return 1;
}

export function matchScore(
  query: string,
  label: string,
  detail?: string,
): number {
  const q = normalizeForSearch(query);
  if (q.length === 0) return 1;
  const onLabel = scoreText(q, normalizeForSearch(label));
  if (onLabel > 0) return onLabel * 2;
  return detail == null ? 0 : scoreText(q, normalizeForSearch(detail));
}

export function rankPalette(
  entries: readonly PaletteEntry[],
  query: string,
): RankedGroup[] {
  const scored = entries
    .map((entry, order) => ({
      entry,
      order,
      score: matchScore(query, entry.label, entry.detail),
    }))
    .filter((row) => row.score > 0);
  return PALETTE_GROUP_ORDER.flatMap((group) => {
    const rows = scored
      .filter((row) => row.entry.group === group)
      .sort((a, b) => b.score - a.score || a.order - b.order)
      .slice(0, PALETTE_GROUP_LIMIT)
      .map((row) => row.entry);
    return rows.length > 0 ? [{ group, entries: rows }] : [];
  });
}

export function moveSelection(
  index: number,
  delta: number,
  count: number,
): number {
  if (count <= 0) return 0;
  return (((index + delta) % count) + count) % count;
}
