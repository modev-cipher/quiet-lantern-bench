export const SHELF_BOOK_HEIGHT = 182;

const TRIM_HEIGHT_IN = 9;
const PAGES_PER_INCH = 444;
const BOARDS_IN = 0.25;
const ORDINARY_PAGES = 240;
const MIN_SPINE = 10;
const MAX_SPINE = 44;
const FINISHED_AT = 0.98;

export function spineWidth(
  pageCount: number | null | undefined,
  height: number = SHELF_BOOK_HEIGHT,
): number {
  const pages =
    pageCount != null && Number.isFinite(pageCount) && pageCount > 0
      ? pageCount
      : ORDINARY_PAGES;
  const scale = height / SHELF_BOOK_HEIGHT;
  const px = ((pages / PAGES_PER_INCH + BOARDS_IN) / TRIM_HEIGHT_IN) * height;
  return Math.round(
    Math.min(MAX_SPINE * scale, Math.max(MIN_SPINE * scale, px)),
  );
}

export type ShelfStatus =
  | { kind: "improving" }
  | { kind: "casting" }
  | { kind: "listen"; progress: number; leftMs: number | null }
  | { kind: "finished" }
  | { kind: "audio" }
  | { kind: "stale" }
  | { kind: "ready" };

export function shelfStatus(input: {
  improving: boolean;
  casting: boolean;
  playable: boolean;
  stale: boolean;
  listened: number | null;
  durationMs: number | null | undefined;
}): ShelfStatus {
  if (input.improving) return { kind: "improving" };
  if (input.casting) return { kind: "casting" };
  if (input.playable) {
    const listened = input.listened;
    if (listened == null || !Number.isFinite(listened) || listened <= 0) {
      return { kind: "audio" };
    }
    if (listened >= FINISHED_AT) return { kind: "finished" };
    const duration = input.durationMs;
    return {
      kind: "listen",
      progress: listened,
      leftMs:
        duration != null && Number.isFinite(duration) && duration > 0
          ? Math.round(duration * (1 - listened))
          : null,
    };
  }
  if (input.stale) return { kind: "stale" };
  return { kind: "ready" };
}

export function partitionShelf<T>(
  items: readonly T[],
  statusOf: (item: T) => ShelfStatus,
): { inProgress: T[]; onShelf: T[] } {
  const inProgress: T[] = [];
  const onShelf: T[] = [];
  for (const item of items) {
    const kind = statusOf(item).kind;
    (kind === "listen" || kind === "casting" ? inProgress : onShelf).push(item);
  }
  return { inProgress, onShelf };
}

export function ribbonLength(
  progress: number | null,
  height: number = SHELF_BOOK_HEIGHT,
): number {
  if (progress == null || !Number.isFinite(progress) || progress <= 0) return 0;
  return Math.round(18 + Math.min(1, progress) * height * 0.72);
}

export const CLOTH_TONES = 8;

export function clothTone(seed: string): number {
  let hash = 2166136261;
  for (let index = 0; index < seed.length; index += 1) {
    hash = Math.imul(hash ^ seed.charCodeAt(index), 16777619);
  }
  return (hash >>> 0) % CLOTH_TONES;
}

const RTL_LETTER =
  /[\p{Script=Arabic}\p{Script=Hebrew}\p{Script=Syriac}\p{Script=Thaana}\p{Script=Nko}]/u;
const LTR_LETTER = /\p{L}/u;

export function bookDirection(title: string): "ltr" | "rtl" {
  for (const char of title) {
    if (RTL_LETTER.test(char)) return "rtl";
    if (LTR_LETTER.test(char)) return "ltr";
  }
  return "ltr";
}
