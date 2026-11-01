import type { Page } from "@playwright/test";

import { fixtureBook } from "./tauriMock";

export const VOICE_SESSION_ID = "0192a7c8-1f2e-7c3d-9a4b-5c6d7e8f9a0b";

export async function emitVoice(
  page: Page,
  event: Record<string, unknown>,
  bookId: string = fixtureBook().id,
): Promise<void> {
  await page.evaluate(
    ({ sessionId, scopedBookId, payload }) => {
      (
        window as unknown as {
          __mockEmitEvent: (name: string, payload: unknown) => void;
        }
      ).__mockEmitEvent("voice-session-event", {
        session_id: sessionId,
        book_id: scopedBookId,
        event: payload,
      });
    },
    { sessionId: VOICE_SESSION_ID, scopedBookId: bookId, payload: event },
  );
}
