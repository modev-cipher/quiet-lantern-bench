import { describe, expect, it } from "vitest";

import {
  displayedPageReport,
  isBookSurface,
  navigationPageToApply,
  type BookSurfaceItem,
} from "./viewportSyncModel";
import type { VoiceNavigation } from "./voiceSessionModel";

function nav(nonce: number, pageNo: number | null = 7): VoiceNavigation {
  return { nonce, tool: "jump_to_page", posMs: 2000, pageNo, chapterIdx: 1 };
}

describe("navigationPageToApply (P8)", () => {
  it("applies a fresh event's page while following", () => {
    expect(navigationPageToApply(nav(1), 0, false)).toBe(7);
  });

  it("manual browsing does not cancel a later tutor destination", () => {
    expect(navigationPageToApply(nav(1), 0, false)).toBe(7);
  });

  it("an explicit hold keeps the current page", () => {
    expect(navigationPageToApply(nav(1), 0, true)).toBeNull();
  });

  it("an explicit view hold applies consistently to every navigation tool", () => {
    const turn: VoiceNavigation = { ...nav(1, 4), tool: "turn_page" };
    expect(navigationPageToApply(turn, 0, false)).toBe(4);
    expect(navigationPageToApply(turn, 0, true)).toBeNull();
    expect(navigationPageToApply(turn, 1, false)).toBeNull();
  });

  it("consumes each event exactly once (edge, not level)", () => {
    expect(navigationPageToApply(nav(1), 1, false)).toBeNull();
    expect(navigationPageToApply(nav(1), 2, false)).toBeNull();
  });

  it("an observed event is not automatically replayed; Return uses the retained destination", () => {
    // Suppressed while paused — the caller records the nonce as consumed.
    expect(navigationPageToApply(nav(1), 0, true)).toBeNull();
    // After resume, the same (spent) event stays dead…
    expect(navigationPageToApply(nav(1), 1, false)).toBeNull();
    // …and a fresh event follows again.
    expect(navigationPageToApply(nav(2, 9), 1, false)).toBe(9);
  });

  it("an event with no resolvable page moves nothing", () => {
    expect(navigationPageToApply(nav(1, null), 0, false)).toBeNull();
    expect(navigationPageToApply(null, 0, false)).toBeNull();
  });
});

describe("isBookSurface", () => {
  it("discriminates the job-independent mount from a shelf item", () => {
    const surface: BookSurfaceItem = {
      kind: "bookSurface",
      bookId: "b1",
      title: "T",
      isScanned: false,
      pageCount: 12,
    };
    expect(isBookSurface(surface)).toBe(true);
    expect(isBookSurface({ jobId: "j1", bookId: "b1" })).toBe(false);
  });
});

describe("displayedPageReport", () => {
  it("names a page the reader shows away from narration and clears when it follows again", () => {
    expect(displayedPageReport(2, 1)).toBe(2);
    expect(displayedPageReport(3, 3)).toBeNull();
    expect(displayedPageReport(null, 4)).toBeNull();
  });
});
