import type { AudioShelfItem } from "./audioShelfModel";
import { bookDisplayTitle, type BookSummary } from "./libraryModel";
import { canFollowAlong } from "./readingOverlayModel";
import { isBookSurface, type BookSurfaceItem } from "./viewportSyncModel";

export const LAST_PLACE_KEY = "txtcaster.lastPlace";

export type LastPlace =
  | { kind: "cast"; bookId: string; exportId: string }
  | { kind: "pages"; bookId: string };

export type ResolvedPlace =
  | { kind: "cast"; item: AudioShelfItem }
  | { kind: "pages"; surface: BookSurfaceItem };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isId(value: unknown): value is string {
  return typeof value === "string" && value !== "";
}

export function parseLastPlace(raw: string | null): LastPlace | null {
  if (raw == null || raw === "") return null;
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!isRecord(value) || !isId(value.bookId)) return null;
  if (value.kind === "pages") return { kind: "pages", bookId: value.bookId };
  if (value.kind === "cast" && isId(value.exportId)) {
    return { kind: "cast", bookId: value.bookId, exportId: value.exportId };
  }
  return null;
}

export function serialiseLastPlace(place: LastPlace): string {
  return JSON.stringify(place);
}

export function placeOf(item: AudioShelfItem | BookSurfaceItem): LastPlace {
  if (isBookSurface(item)) return { kind: "pages", bookId: item.bookId };
  return { kind: "cast", bookId: item.bookId, exportId: item.id };
}

function pagesOf(book: BookSummary): ResolvedPlace | null {
  if (book.pageCount == null || book.pageCount < 1) return null;
  return {
    kind: "pages",
    surface: {
      kind: "bookSurface",
      bookId: book.id,
      title: bookDisplayTitle(book),
      isScanned: book.isScanned,
      pageCount: book.pageCount,
    },
  };
}

export function resolveLastPlace(
  place: LastPlace | null,
  books: BookSummary[],
  shelf: AudioShelfItem[],
): ResolvedPlace | null {
  if (place == null) return null;
  const book = books.find((entry) => entry.id === place.bookId) ?? null;
  if (book == null) return null;
  if (place.kind === "cast") {
    const item =
      shelf.find(
        (entry) => entry.id === place.exportId && entry.bookId === book.id,
      ) ?? null;
    if (item != null && canFollowAlong(item)) return { kind: "cast", item };
  }
  return pagesOf(book);
}

export function loadLastPlace(): LastPlace | null {
  try {
    return parseLastPlace(localStorage.getItem(LAST_PLACE_KEY));
  } catch {
    return null;
  }
}

export function saveLastPlace(place: LastPlace | null): void {
  try {
    if (place == null) localStorage.removeItem(LAST_PLACE_KEY);
    else localStorage.setItem(LAST_PLACE_KEY, serialiseLastPlace(place));
  } catch {
    return;
  }
}
