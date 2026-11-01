// Talk entry routing (owner decision 2026-10-09, superseding the "prefer the
// cast" default of docs/f8-grounded-ux-design.md §5.3): a book-voice entry
// (Library Talk, JobModal tutor card) opens a fresh session with the BOOK
// ITSELF — its rendered pages — never the last cast the user listened to.
// A readable cast is only the fallback for books without page renders, and
// StudyView's book-tutor card only for books with nothing renderable.

import type { AudioShelfItem } from "./audioShelfModel";
import { bookDisplayTitle, type BookSummary } from "./libraryModel";
import { canFollowAlong } from "./readingOverlayModel";
import type { BookSurfaceItem } from "./viewportSyncModel";

export type TutorRoute =
  /** No page renders, but a readable cast exists: the grounded ReadingView
   * overlay (its rail mounts the voice panel with §11.1 spotlight/follow). */
  | { kind: "reader"; item: AudioShelfItem }
  /** Rendered pages exist: the job-independent BookSurface reader — a
   * BookDirect session beside real pages. */
  | { kind: "bookSurface"; surface: BookSurfaceItem }
  /** Nothing to show (no readable render): the Study card fallback. */
  | { kind: "study" };

export function tutorEntryRoute(
  book: BookSummary | null,
  shelf: AudioShelfItem[],
): TutorRoute {
  if (book == null) return { kind: "study" };
  if (book.pageCount != null && book.pageCount >= 1) {
    return {
      kind: "bookSurface",
      surface: {
        kind: "bookSurface",
        bookId: book.id,
        title: bookDisplayTitle(book),
        isScanned: book.isScanned,
        pageCount: book.pageCount,
      },
    };
  }
  const followable =
    book.id === ""
      ? null
      : (shelf
          .filter((item) => item.bookId === book.id && canFollowAlong(item))
          .sort((a, b) => b.createdAt - a.createdAt)[0] ?? null);
  if (followable != null) return { kind: "reader", item: followable };
  return { kind: "study" };
}
