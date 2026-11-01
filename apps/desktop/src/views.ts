export type View =
  "library" | "audio" | "jobs" | "models" | "settings" | "providers";

export const GO_VIEW_EVENT = "txtcaster:go-view";
export const SETUP_CHANGED_EVENT = "txtcaster:setup-changed";

export function goToView(view: View): void {
  window.dispatchEvent(new CustomEvent<View>(GO_VIEW_EVENT, { detail: view }));
}

export function goToModels(): void {
  goToView("models");
}

export function goToAudioShelf(): void {
  goToView("audio");
}

let pendingImportPath: string | null = null;

export function requestImport(path: string): void {
  pendingImportPath = path;
}

export function consumePendingImport(): string | null {
  const path = pendingImportPath;
  pendingImportPath = null;
  return path;
}

let pendingSendToPhone = false;

export function goToSendToPhone(): void {
  pendingSendToPhone = true;
  goToAudioShelf();
}

export function consumePendingSendToPhone(): boolean {
  const pending = pendingSendToPhone;
  pendingSendToPhone = false;
  return pending;
}

export function notifySetupChanged(): void {
  window.dispatchEvent(new Event(SETUP_CHANGED_EVENT));
}

export const VOICE_KEYS_CHANGED_EVENT = "txtcaster:voice-keys-changed";

export function notifyVoiceKeysChanged(): void {
  window.dispatchEvent(new Event(VOICE_KEYS_CHANGED_EVENT));
}

// Settings → Providers → <provider> addressable state (Hermes S-5 IA
// ruling): a deep link targets the provider page without exposing any key
// identifier. Consumed once by the Providers page on mount.
let pendingProvider: string | null = null;

export function goToProvider(providerId: string): void {
  pendingProvider = providerId;
  goToView("providers");
}

export function consumePendingProvider(): string | null {
  const pending = pendingProvider;
  pendingProvider = null;
  return pending;
}

// Talk entries route to the READER surface first (F8 D2): App owns the
// overlays, so the request rides a window event App handles and
// tutorEntryRoute picks the surface. The tutor panel below is the
// destination only when a book has nothing readable to render.
export const BOOK_TUTOR_EVENT = "txtcaster:book-tutor";

export function requestBookTutor(bookId: string): void {
  window.dispatchEvent(
    new CustomEvent<string>(BOOK_TUTOR_EVENT, { detail: bookId }),
  );
}

// Opening a book from its cover when it has no cast to study: App owns the
// reader, so the request rides a window event. No tutor session is started
// or reset by it — that is what Talk is for.
export const READER_SHOW_TUTOR_EVENT = "txtcaster:reader-show-tutor";

export function showReaderTutor(): void {
  window.dispatchEvent(new Event(READER_SHOW_TUTOR_EVENT));
}

export const BOOK_OPEN_EVENT = "txtcaster:book-open";

export function requestBookOpen(bookId: string): void {
  window.dispatchEvent(
    new CustomEvent<string>(BOOK_OPEN_EVENT, { detail: bookId }),
  );
}

// Casting a book from inside its own Book: the cast dialog lives in the
// Library, so the request lands there through a one-shot latch (the Library
// may not be mounted yet) plus an event (it may already be).
export const CAST_BOOK_EVENT = "txtcaster:cast-book";

let pendingCastBookId: string | null = null;

export function goToCastBook(bookId: string): void {
  pendingCastBookId = bookId;
  goToView("library");
  window.dispatchEvent(new Event(CAST_BOOK_EVENT));
}

export function consumePendingCastBook(): string | null {
  const pending = pendingCastBookId;
  pendingCastBookId = null;
  return pending;
}

// Tutor panel hand-off: the shell opens its tutor panel on this event. A
// book rides the same one-shot latch pattern as the provider deep link
// above — consumed once by the panel at mount; no book id ever rides the
// event itself. Without a book the panel is the standalone tutor.
export const OPEN_TUTOR_EVENT = "txtcaster:open-tutor";
export const RETURN_TUTOR_EVENT = "txtcaster:return-tutor";

export function returnToTutor(): void {
  window.dispatchEvent(new Event(RETURN_TUTOR_EVENT));
}

let pendingTutorBookId: string | null = null;

export function openTutor(bookId: string | null = null): void {
  pendingTutorBookId = bookId;
  window.dispatchEvent(new Event(OPEN_TUTOR_EVENT));
}

export function consumePendingTutorBook(): string | null {
  const pending = pendingTutorBookId;
  pendingTutorBookId = null;
  return pending;
}

export const REPORT_ISSUE_EVENT = "txtcaster:report-issue";

export function openReportIssueModal(): void {
  window.dispatchEvent(new Event(REPORT_ISSUE_EVENT));
}
