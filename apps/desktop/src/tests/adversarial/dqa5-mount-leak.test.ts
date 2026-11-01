// D-QA5 (CRITICAL) — cross-book mount leak.
//
// INVARIANT: a newly mounted Talk panel for book B renders ZERO rows from a
// previous session — INCLUDING the window BEFORE any new session id exists
// (the mount path), while a same-book mid-session remount keeps its rows.
//   Spec citations:
//   - F8 §1.3 (D3 verdict): "the transcript view renders a module-level
//     singleton store that is never reset"; "No load-at-mount path exists:
//     … Mount-time invokes are key probes only".
//   - F8 §2.3.2: the id guard makes stale-ID EVENTS unrenderable, and
//     "markStarting clears session.transcript optimistically as well" —
//     but markStarting is the Start PRESS, not the mount. The spec's own
//     test-gap note (§1.3): "No test distinguishes 'same-session remount'
//     (desired) from 'new session' (the bug)". This suite adds the third
//     case both missed: 'different-BOOK mount before any new session'.
//   - F8 §2.3.3: same-session remount durability preserved byte-for-byte —
//     the over-clear guard.
//
// ATTACK STRATEGY: drive the PRODUCTION store (no mocks) through the exact
// owner sequence — session over book A leaves rows (live, then also ended),
// then a panel for book B mounts. A mount can only do what the public store
// surface offers a mount (F8 §1.3: subscribe + getSnapshot; key probes
// don't touch this store). We assert the snapshot such a mount renders is
// empty. If the store offers no book-scoped mount seam at all — VoiceUiState
// carries no book identity — the assertion fails structurally: that IS the
// defect, documented precisely.
//
// WHY A SHALLOW MOCK CANNOT FAKE A PASS: every test dispatches real wire
// envelopes through the production dispatchVoiceEvent → reducer chain and
// reads the production snapshot; the only tauri seam (event listen) is the
// same inert stub the store's own suite uses. There is no assertion a stub
// can satisfy without the store actually isolating mounts by book.

import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

import * as store from "../../voiceSessionStore";
import {
  dispatchVoiceEvent,
  getVoiceSnapshot,
  resetVoiceSessionStore,
  subscribeVoice,
} from "../../voiceSessionStore";

const SESSION_A = "01920000-0000-7000-8000-00000000000a";

// Arbitration r2: wire carries book_id per the current public contract; arrangement only.
function wire(sessionId: string, bookId: string, event: unknown): void {
  dispatchVoiceEvent({ session_id: sessionId, book_id: bookId, event });
}

// Everything a panel MOUNT may execute against this store, per F8 §1.3
// ("Mount-time invokes are key probes only" — probes never touch the
// transcript store): subscribe, then render from the snapshot. The panel
// knows its own bookId prop; if the store exposed a book-scoped mount
// seam, this is where a mount would call it. It exposes none (see the
// structural test below), so this models the production mount faithfully.
// Adaptive: if the store ever grows a book-scoped mount export (any
// function whose name mentions "book"), the mount uses it — so a real fix
// turns these tests green without edits, while today's store (no such
// seam) is measured as-is.
function mountPanelForBook(bookId: string): {
  rendered: () => unknown;
  unmount: () => void;
} {
  for (const [name, value] of Object.entries(store)) {
    if (/book/i.test(name) && typeof value === "function") {
      (value as (bookId: string) => unknown)(bookId);
    }
  }
  const unmount = subscribeVoice(() => {});
  return {
    rendered: () => getVoiceSnapshot().session.transcript,
    unmount,
  };
}

function leaveResidueFromBookA(): void {
  wire(SESSION_A, "book-A", { kind: "lifecycle", state: "connected" });
  wire(SESSION_A, "book-A", {
    kind: "transcript",
    role: "user",
    text: "question about book A",
  });
  wire(SESSION_A, "book-A", {
    kind: "transcript",
    role: "assistant",
    text: "answer grounded in book A",
  });
}

beforeEach(() => {
  resetVoiceSessionStore();
});

describe("D-QA5: a different book's panel mount renders zero prior rows", () => {
  it("owner shape — session A ENDED, book B panel mounts: the mount window shows no rows", () => {
    leaveResidueFromBookA();
    wire(SESSION_A, "book-A", { kind: "ended", reason: "stopped" });
    // Book B's panel mounts. No Start pressed yet, no new session id exists
    // anywhere — this is precisely the window the owner watched a foreign
    // transcript in.
    const mount = mountPanelForBook("book-B");
    expect(
      mount.rendered(),
      "a fresh mount for a DIFFERENT book rendered the previous session's transcript (owner defect D-QA5, mount path)",
    ).toEqual([]);
    mount.unmount();
  });

  it("harsher: session A still LIVE, book B panel mounts — still zero foreign rows", () => {
    leaveResidueFromBookA();
    const mount = mountPanelForBook("book-B");
    expect(
      mount.rendered(),
      "book B's mount rendered book A's LIVE transcript — cross-book leakage before any B session exists",
    ).toEqual([]);
    mount.unmount();
  });

  it("structural root: the store offers a mount seam that knows the book (else the two tests above CANNOT be satisfied)", () => {
    // VoiceUiState/VoiceSessionState carry sessionId (F8 §2.3) but no book
    // identity, and no exported operation takes a bookId. Without either, a
    // book-B mount is indistinguishable from a book-A remount, so the
    // invariant is structurally unsatisfiable. This test names the missing
    // capability exactly.
    const exports = Object.keys(store);
    const bookScoped = exports.filter((name) => /book/i.test(name));
    const snapshotHasBook =
      "bookId" in getVoiceSnapshot() || "bookId" in getVoiceSnapshot().session;
    expect(
      bookScoped.length > 0 || snapshotHasBook,
      `no book-scoped mount seam exists on the store (exports: ${exports.join(", ")}) and the snapshot carries no bookId — a different-book mount cannot clear or scope, which is the D-QA5 root`,
    ).toBe(true);
  });
});

describe("D-QA5 over-clear guard: same-book remount keeps its rows", () => {
  it("mid-session remount (same book, same session) still renders its transcript", () => {
    // F8 §2.3.3: same-id durability byte-for-byte — the stranded-Start fix.
    // Whatever mechanism fixes the tests above must NOT clear here.
    leaveResidueFromBookA();
    const first = mountPanelForBook("book-A");
    expect(first.rendered()).toHaveLength(2);
    first.unmount();
    const remount = mountPanelForBook("book-A");
    expect(
      remount.rendered(),
      "over-clear: a same-book mid-session remount lost its own rows",
    ).toHaveLength(2);
    remount.unmount();
  });
});
