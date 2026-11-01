import { describe, expect, it, vi } from "vitest";

import { OPEN_TUTOR_EVENT, consumePendingTutorBook, openTutor } from "./views";
import { readSource } from "./tests/readSource";

// F7 P5 (entry gating): every voice entry surface hands the BOOK to the
// tutor — the Library Talk button and the JobModal tutor card route through
// tutorEntryRoute, and the fallback opens the shell's tutor panel through a
// one-shot latch. The panel mounts book-attached with a jobId that is the
// latest completed cast's id or null, never a hidden quick-job.

describe("the tutor panel book hand-off latch", () => {
  it("is one-shot: consumed once, then empty, and no book id rides the event", () => {
    const events: Event[] = [];
    vi.stubGlobal("window", {
      dispatchEvent: (event: Event) => {
        events.push(event);
        return true;
      },
    });
    try {
      openTutor("book-7");
    } finally {
      vi.unstubAllGlobals();
    }
    expect(events.map((event) => event.type)).toEqual([OPEN_TUTOR_EVENT]);
    expect("detail" in (events[0] as object)).toBe(false);
    expect(consumePendingTutorBook()).toBe("book-7");
    expect(consumePendingTutorBook()).toBeNull();
  });

  it("opening without a book leaves the latch empty: the panel is the standalone tutor", () => {
    vi.stubGlobal("window", { dispatchEvent: () => true });
    try {
      openTutor("book-9");
      openTutor();
    } finally {
      vi.unstubAllGlobals();
    }
    expect(consumePendingTutorBook()).toBeNull();
  });
});

describe("JobModal tutor card (source contract)", () => {
  const SOURCE = readSource(new URL("./JobModal.tsx", import.meta.url));

  it("carries the book to the reader-surface router instead of dropping it", () => {
    // F8 D2: the tutor card routes through tutorEntryRoute (reader when a
    // cast exists, BookSurface when pages do, Study card as the fallback).
    expect(SOURCE).toContain("requestBookTutor(book.id)");
    expect(SOURCE).not.toContain('goToView("study")');
  });

  it("no longer claims the book needs a narration job first", () => {
    expect(SOURCE).not.toContain("once it has a narration job");
  });
});

describe("TutorPanel book tutor section (source contract)", () => {
  const SOURCE = readSource(new URL("./TutorPanel.tsx", import.meta.url));

  it("consumes the latch once at mount", () => {
    expect(SOURCE).toContain("useState(consumePendingTutorBook)");
  });

  it("mounts the panel with the book attached and the availability-derived job id", () => {
    expect(SOURCE).toContain("bookId={talkBook.id}");
    expect(SOURCE).toContain("jobId={talkJobId}");
    expect(SOURCE).toContain(
      "bookAvailability(talkBook.id, target.shelf).playable?.jobId ?? null",
    );
  });

  it("shows the no-cast hint only when no completed job exists", () => {
    const section = SOURCE.slice(
      SOURCE.indexOf('data-testid="study-book-tutor"'),
      SOURCE.indexOf('data-testid="study-standalone-tutor"'),
    );
    expect(section).toContain("talkJobId == null && (");
    expect(section).toContain("study-book-tutor-no-cast");
  });

  it("keeps exactly one standalone tutor mount, never beside a book tutor", () => {
    expect(SOURCE.match(/data-testid="study-standalone-tutor"/g)).toHaveLength(
      1,
    );
    expect(
      SOURCE.match(/<VoiceSessionPanel bookId=\{null\} jobId=\{null\}/g),
    ).toHaveLength(1);
    expect(SOURCE).toContain("resolved && talkBook != null && (");
    expect(SOURCE).toContain("resolved && talkBook == null && (");
  });
});

describe("App routes the no-render Talk fallback to the tutor panel", () => {
  const APP = readSource(new URL("./App.tsx", import.meta.url));

  it("opens the panel with the book and remounts it per request", () => {
    expect(APP).toContain("openTutor(bookId);");
    expect(APP).toMatch(/<TutorPanel\s+key=\{tutorPanel\}/);
  });

  it("never mounts the shell tutor beside the reader's own companion", () => {
    expect(APP).toContain('tutorPanel != null && overlay === "none" && (');
  });

  it("closes the shell tutor for good when an overlay opens: hiding it would remount a standalone panel on return and rebind the voice store away from a live book session", () => {
    const applyOverlay = APP.slice(
      APP.indexOf("const applyOverlay = useCallback("),
      APP.indexOf("setOverlay((current) => overlayState(current, action));"),
    );
    expect(applyOverlay).toMatch(
      /if \(action\.type === "openReading"\) \{[^}]*setTutorPanel\(null\);/,
    );
  });
});
