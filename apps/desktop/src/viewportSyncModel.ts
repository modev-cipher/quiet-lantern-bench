// F8 D2/D4 (docs/f8-grounded-ux-design.md §2.2/§2.4): pure decisions for the
// reader-as-the-voice-surface campaign. The runtime surfaces successful
// navigation as SessionUiEvent::Navigation (voiceSessionModel `navigation`);
// this module owns the §11.1 follow law for applying it to the shown page,
// and the shape of the job-independent reader mount.

import type { VoiceNavigation } from "./voiceSessionModel";

/**
 * The job-independent reader mount (F8 §2.2, BookDirect): ReadingView renders
 * the book's cached page images with the narration/word-overlay machinery
 * disabled and the voice panel mounted exactly as in the grounded overlay.
 * No jobId exists — the session is jobless by definition.
 */
export interface BookSurfaceItem {
  kind: "bookSurface";
  bookId: string;
  title: string;
  isScanned: boolean;
  pageCount: number | null;
}

export function isBookSurface(item: object): item is BookSurfaceItem {
  return (item as { kind?: unknown }).kind === "bookSurface";
}

export function displayedPageReport(
  shownPage: number | null,
  narrationPage: number | null,
): number | null {
  return shownPage === narrationPage ? null : shownPage;
}

/** Only an explicit view hold blocks a fresh destination; scrolling and
 * browsing are independent viewport gestures. The consumer retains held
 * destinations separately for the return-to-tutor action. */
export function navigationPageToApply(
  navigation: VoiceNavigation | null,
  appliedNonce: number,
  viewHeld: boolean,
): number | null {
  if (navigation == null || navigation.nonce <= appliedNonce) return null;
  if (navigation.pageNo == null) return null;
  if (viewHeld) return null;
  return navigation.pageNo;
}
