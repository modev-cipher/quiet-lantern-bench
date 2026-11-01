// The store exists so the session's UI state outlives any one panel mount —
// these tests pin that lifecycle: events land whether or not a subscriber
// is present, adoption clears the start-in-flight flags, and stale errors
// die on lifecycle progress instead of haunting a healthy session.

import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

import { adoptedSessionNotice, noticeText } from "./voiceSessionModel";
import {
  adoptRunningSession,
  bindVoicePanelBook,
  clearEarlierConversation,
  showEarlierConversation,
  freshVoicePanelForBook,
  getVoiceBookScope,
  dispatchVoiceEvent,
  getVoiceSnapshot,
  getActiveVoiceSnapshot,
  synchronizeVoiceSession,
  activeSessionFrom,
  markStarting,
  markVoiceError,
  noteTypedSend,
  noteVoiceCommand,
  resetVoiceSessionStore,
  subscribeVoice,
  voiceSessionLogText,
} from "./voiceSessionStore";

beforeEach(() => {
  resetVoiceSessionStore();
});

describe("active session survives view navigation", () => {
  it("preserves matching Continue history when the first native event follows a start error", () => {
    bindVoicePanelBook("book-A");
    markStarting();
    showEarlierConversation("book-A", "past-A", [
      { role: "user", text: "A's earlier question" },
    ]);
    markVoiceError("start response unavailable", { bookId: "book-A" });
    dispatchVoiceEvent({
      session_id: TEST_SESSION,
      book_id: "book-A",
      job_id: null,
      purpose: "tutor",
      event: { kind: "lifecycle", state: "connected" },
    });
    expect(getActiveVoiceSnapshot().earlier?.sourceId).toBe("past-A");
  });

  it("starting Continue for another book clears foreign history before its first event", () => {
    bindVoicePanelBook("book-A");
    showEarlierConversation("book-A", "past-A", [
      { role: "user", text: "A's earlier question" },
    ]);
    bindVoicePanelBook("book-B");
    markStarting({ bookId: "book-B", jobId: null });
    expect(getActiveVoiceSnapshot().earlier).toBeNull();
  });
  it("a delayed start belongs to its initiating book rather than the displayed panel", () => {
    bindVoicePanelBook("book-B");
    markStarting({ bookId: "book-A", jobId: null });
    expect(getActiveVoiceSnapshot().pendingTarget?.bookId).toBe("book-A");
    expect(getVoiceSnapshot().starting).toBe(false);
    bindVoicePanelBook("book-A");
    expect(getVoiceSnapshot().starting).toBe(true);
  });
  it("a different panel's history response cannot enter the active conversation", () => {
    bindVoicePanelBook("book-A");
    dispatchVoiceEvent({
      session_id: TEST_SESSION,
      book_id: "book-A",
      job_id: null,
      purpose: "tutor",
      event: { kind: "lifecycle", state: "connected" },
    });
    bindVoicePanelBook("book-B");
    showEarlierConversation("book-B", "past-B", [
      { role: "user", text: "Private history of B" },
    ]);
    expect(getActiveVoiceSnapshot().earlier).toBeNull();
  });

  it("keeps a matching history response received while another book is displayed", () => {
    bindVoicePanelBook("book-A");
    markStarting();
    bindVoicePanelBook("book-B");
    showEarlierConversation("book-A", "past-A", [
      { role: "user", text: "A's earlier question" },
    ]);
    expect(getVoiceSnapshot().earlier).toBeNull();
    bindVoicePanelBook("book-A");
    expect(getVoiceSnapshot().earlier?.sourceId).toBe("past-A");
  });

  it("a newly bound native session clears the previous book's earlier conversation", () => {
    bindVoicePanelBook("book-A");
    markStarting();
    showEarlierConversation("book-A", "past-A", [
      { role: "user", text: "Old A recap" },
    ]);
    dispatchVoiceEvent({
      session_id: TEST_SESSION,
      book_id: "book-B",
      job_id: null,
      purpose: "tutor",
      event: { kind: "lifecycle", state: "connected" },
    });
    expect(getActiveVoiceSnapshot().earlier).toBeNull();
  });
  it("does not let a panel or inconsistent same-session event relabel native identity", () => {
    bindVoicePanelBook("book-A");
    dispatchVoiceEvent({
      session_id: TEST_SESSION,
      book_id: "book-A",
      job_id: "cast-A",
      purpose: "tutor",
      event: { kind: "lifecycle", state: "connected" },
    });
    bindVoicePanelBook("book-B");
    dispatchVoiceEvent({
      session_id: TEST_SESSION,
      book_id: "book-B",
      job_id: "cast-B",
      purpose: "tutor",
      event: { kind: "lifecycle", state: "connected" },
    });
    expect(getActiveVoiceSnapshot().identity).toEqual({
      sessionId: TEST_SESSION,
      bookId: "book-A",
      jobId: "cast-A",
      purpose: "tutor",
    });
    expect(getVoiceSnapshot().session.transcript).toEqual([]);
  });

  it("ignores an old inactive probe after a native event confirms a session", async () => {
    let resolve!: (value: unknown) => void;
    invokeMock.mockImplementationOnce(
      () =>
        new Promise((r) => {
          resolve = r;
        }),
    );
    const query = synchronizeVoiceSession();
    await vi.waitFor(() => expect(resolve).toBeTypeOf("function"));
    dispatchVoiceEvent({
      session_id: TEST_SESSION,
      book_id: "book-A",
      job_id: null,
      purpose: "tutor",
      event: { kind: "lifecycle", state: "connected" },
    });
    resolve(null);
    await query;
    expect(getActiveVoiceSnapshot().session.status).toBe("live");
    expect(getActiveVoiceSnapshot().identity?.bookId).toBe("book-A");
  });

  it("does not reattach a session that ended while its native query was pending", async () => {
    let resolve!: (value: unknown) => void;
    invokeMock.mockImplementationOnce(
      () =>
        new Promise((r) => {
          resolve = r;
        }),
    );
    const query = synchronizeVoiceSession();
    await vi.waitFor(() => expect(resolve).toBeTypeOf("function"));
    dispatchVoiceEvent({
      session_id: TEST_SESSION,
      book_id: "book-A",
      job_id: null,
      purpose: "tutor",
      event: { kind: "ended", reason: "stopped" },
    });
    resolve({
      sessionId: TEST_SESSION,
      bookId: "book-A",
      jobId: null,
      purpose: "tutor",
    });
    expect(await query).toBeNull();
    expect(getActiveVoiceSnapshot().session.status).toBe("ended");
  });

  it("rejects malformed descriptors instead of guessing the visible book", () => {
    expect(() =>
      activeSessionFrom({ sessionId: TEST_SESSION, purpose: "tutor" }),
    ).toThrow();
    expect(() =>
      activeSessionFrom({
        sessionId: TEST_SESSION,
        bookId: "A",
        jobId: 12,
        purpose: "tutor",
      }),
    ).toThrow();
  });
  it("keeps A while standalone and B render no foreign rows, including events received away", () => {
    bindVoicePanelBook("book-A");
    dispatchVoiceEvent({
      session_id: TEST_SESSION,
      book_id: "book-A",
      job_id: null,
      purpose: "tutor",
      event: { kind: "lifecycle", state: "connected" },
    });
    dispatchVoiceEvent({
      session_id: TEST_SESSION,
      book_id: "book-A",
      event: {
        kind: "transcript",
        role: "user",
        text: "Keep this conversation",
      },
    });
    dispatchVoiceEvent({
      session_id: TEST_SESSION,
      book_id: "book-A",
      event: {
        kind: "mic_state",
        state: "muted",
        detail: "user",
      },
    });
    const active = getActiveVoiceSnapshot();
    bindVoicePanelBook(null);
    expect(getVoiceSnapshot().session.transcript).toEqual([]);
    expect(getActiveVoiceSnapshot()).toBe(active);
    bindVoicePanelBook("book-B");
    expect(getVoiceSnapshot().session.transcript).toEqual([]);
    dispatchVoiceEvent({
      session_id: TEST_SESSION,
      book_id: "book-A",
      event: {
        kind: "transcript",
        role: "assistant",
        text: "Still answering while away",
      },
    });
    bindVoicePanelBook("book-A");
    expect(getVoiceSnapshot().session.transcript.map((t) => t.text)).toEqual([
      "Keep this conversation",
      "Still answering while away",
    ]);
    expect(getVoiceSnapshot().session.mic).toBe("muted");
  });

  it("receives End while another panel is displayed", () => {
    bindVoicePanelBook("book-A");
    dispatchVoiceEvent({
      session_id: TEST_SESSION,
      book_id: "book-A",
      event: {
        kind: "lifecycle",
        state: "connected",
      },
    });
    bindVoicePanelBook("book-B");
    dispatchVoiceEvent({
      session_id: TEST_SESSION,
      book_id: "book-A",
      event: {
        kind: "ended",
        reason: "stopped",
      },
    });
    bindVoicePanelBook("book-A");
    expect(getVoiceSnapshot().session.status).toBe("ended");
  });
});

// The wire shape is the D3 envelope (SessionUiEnvelope in voice_session.rs):
// these lifecycle tests all speak as one session.
const TEST_SESSION = "01920000-0000-7000-8000-000000000001";
function wireEvent(event: unknown): void {
  dispatchVoiceEvent({ session_id: TEST_SESSION, event });
}

describe("voiceSessionStore", () => {
  it("keeps the native session purpose across remounts and rejects stale purpose changes", () => {
    dispatchVoiceEvent({
      session_id: TEST_SESSION,
      purpose: "agent",
      event: { kind: "lifecycle", state: "connected" },
    });
    expect(getVoiceSnapshot().session.purpose).toBe("agent");
    subscribeVoice(() => {})();
    wireEvent({
      kind: "transcript",
      role: "assistant",
      text: "A source-backed answer",
    });
    expect(getVoiceSnapshot().session.purpose).toBe("agent");
    dispatchVoiceEvent({
      session_id: "01910000-0000-7000-8000-000000000001",
      purpose: "tutor",
      event: { kind: "lifecycle", state: "connected" },
    });
    expect(getVoiceSnapshot().session.purpose).toBe("agent");
    dispatchVoiceEvent({
      session_id: TEST_SESSION,
      purpose: "unexpected",
      event: { kind: "lifecycle", state: "connected" },
    });
    expect(getVoiceSnapshot().session.purpose).toBe("agent");
  });
  it("state survives subscriber churn (the remount that caused the dead end)", () => {
    const unsubscribe = subscribeVoice(() => {});
    wireEvent({ kind: "lifecycle", state: "connected" });
    unsubscribe();
    expect(getVoiceSnapshot().session.status).toBe("live");
    subscribeVoice(() => {})();
    expect(getVoiceSnapshot().session.status).toBe("live");
  });

  it("adoption lands live with the honest notice and no start in flight", () => {
    markStarting();
    adoptRunningSession({
      sessionId: TEST_SESSION,
      bookId: null,
      jobId: null,
      purpose: "tutor",
    });
    const { session, starting, startError } = getVoiceSnapshot();
    expect(session.status).toBe("live");
    expect(noticeText(session)).toBe(adoptedSessionNotice());
    expect(starting).toBe(false);
    expect(startError).toBeNull();
  });

  it("any runtime event ends the starting flag — no eternal Starting…", () => {
    markStarting();
    expect(getVoiceSnapshot().starting).toBe(true);
    wireEvent({ kind: "lifecycle", state: "connected" });
    expect(getVoiceSnapshot().starting).toBe(false);
  });

  it("lifecycle progress clears a stale error; other events keep it visible", () => {
    markVoiceError("channel connect failed: boom");
    wireEvent({ kind: "transcript", role: "user", text: "hi" });
    expect(getVoiceSnapshot().startError).toBe("channel connect failed: boom");
    wireEvent({ kind: "lifecycle", state: "connected" });
    expect(getVoiceSnapshot().startError).toBeNull();
  });

  it("malformed payloads change nothing", () => {
    const before = getVoiceSnapshot();
    wireEvent({ kind: "lifecycle" });
    dispatchVoiceEvent(null);
    expect(getVoiceSnapshot()).toBe(before);
  });

  it("notifies subscribers on every state change", () => {
    const seen: string[] = [];
    subscribeVoice(() => {
      seen.push(getVoiceSnapshot().session.status);
    });
    markStarting();
    wireEvent({ kind: "lifecycle", state: "connected" });
    wireEvent({ kind: "ended", reason: "stopped" });
    expect(seen).toEqual(["idle", "live", "ended"]);
  });

  it("a registered typed send lands as its own sealed entry; voice deltas coalesce", () => {
    noteTypedSend("what is entropy?");
    wireEvent({ kind: "transcript", role: "user", text: "what is entropy?" });
    wireEvent({ kind: "transcript", role: "user", text: "and enth" });
    wireEvent({ kind: "transcript", role: "user", text: "alpy" });
    expect(getVoiceSnapshot().session.transcript).toEqual([
      { role: "user", text: "what is entropy?", sealed: true, typed: true },
      { role: "user", text: "and enthalpy" },
    ]);
  });

  it("the typed-send match is consumed once", () => {
    noteTypedSend("again");
    wireEvent({ kind: "transcript", role: "user", text: "again" });
    wireEvent({ kind: "transcript", role: "user", text: "again" });
    expect(getVoiceSnapshot().session.transcript).toEqual([
      { role: "user", text: "again", sealed: true, typed: true },
      { role: "user", text: "again" },
    ]);
  });

  it("journals raw events and commands with timestamps, capped", () => {
    noteVoiceCommand("start");
    wireEvent({ kind: "lifecycle", state: "connected" });
    dispatchVoiceEvent({ not: "parseable" });
    const lines = voiceSessionLogText().split("\n");
    expect(lines).toHaveLength(3);
    for (const line of lines) {
      const entry = JSON.parse(line) as { at: string; source: string };
      expect(entry.at).toMatch(/^\d{4}-\d{2}-\d{2}T/);
      expect(["event", "command"]).toContain(entry.source);
    }
    for (let at = 0; at < 600; at += 1) {
      wireEvent({ kind: "lifecycle", state: "connected" });
    }
    expect(voiceSessionLogText().split("\n").length).toBeLessThanOrEqual(500);
  });

  it("journals wire truth: typed content only as length until the runtime echoes it", () => {
    noteTypedSend("refused secret never sent");
    expect(voiceSessionLogText()).not.toContain("refused secret");
    expect(voiceSessionLogText()).toContain('"length":25');
    wireEvent({
      kind: "transcript",
      role: "assistant",
      text: "echo diagnosis needs verbatim wire events",
    });
    expect(voiceSessionLogText()).toContain(
      "echo diagnosis needs verbatim wire events",
    );
  });

  it("the bubble carries the wire's transcription verbatim — no rewriting layer", () => {
    wireEvent({ kind: "transcript", role: "user", text: "البيدج" });
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    expect(getVoiceSnapshot().session.transcript).toEqual([
      { role: "user", text: "البيدج", sealed: true },
    ]);
    expect(voiceSessionLogText()).toContain("البيدج");
  });

  it("reset clears the journal and pending typed sends", () => {
    noteTypedSend("gone");
    wireEvent({ kind: "lifecycle", state: "connected" });
    resetVoiceSessionStore();
    expect(voiceSessionLogText()).toBe("");
    wireEvent({ kind: "transcript", role: "user", text: "gone" });
    expect(getVoiceSnapshot().session.transcript).toEqual([
      { role: "user", text: "gone" },
    ]);
  });
});

// D3 session isolation (F8 §2.3): the wire envelope carries the session id;
// the store discards stale-id events and replaces state wholesale on a new
// id. UUIDv7 ids are time-ordered, so lexicographic comparison IS the
// stale-response guard.
const SESSION_A = "01920000-0000-7000-8000-00000000000a";
const SESSION_B = "01920000-0001-7000-8000-00000000000b";

function wire(sessionId: string, event: unknown): void {
  dispatchVoiceEvent({ session_id: sessionId, event });
}

describe("D3 session isolation (cross-session transcript leakage)", () => {
  it("owner shape: a new session never renders the previous session's rows", () => {
    // Session A leaves transcript rows and ends — exactly the residue the
    // owner saw leak into the next book session.
    wire(SESSION_A, { kind: "lifecycle", state: "connected" });
    wire(SESSION_A, { kind: "transcript", role: "user", text: "old question" });
    wire(SESSION_A, {
      kind: "transcript",
      role: "assistant",
      text: "old answer",
    });
    wire(SESSION_A, { kind: "ended", reason: "stopped" });
    expect(getVoiceSnapshot().session.transcript.length).toBeGreaterThan(0);
    // A new session mounts: its first event must show ZERO residual rows.
    wire(SESSION_B, { kind: "lifecycle", state: "connected" });
    expect(getVoiceSnapshot().session.transcript).toEqual([]);
    expect(getVoiceSnapshot().session.status).toBe("live");
    wire(SESSION_B, { kind: "transcript", role: "user", text: "new question" });
    expect(getVoiceSnapshot().session.transcript).toEqual([
      { role: "user", text: "new question" },
    ]);
  });

  it("interleaved sessions: the panel state holds ONLY the newest session's rows", () => {
    wire(SESSION_A, { kind: "lifecycle", state: "connected" });
    wire(SESSION_A, { kind: "transcript", role: "assistant", text: "A says" });
    wire(SESSION_B, { kind: "lifecycle", state: "connected" });
    wire(SESSION_B, { kind: "transcript", role: "assistant", text: "B says" });
    // A stale-id event arriving after B started is discarded, never rendered
    // and never coalesced onto B's bubbles.
    wire(SESSION_A, { kind: "transcript", role: "assistant", text: " A tail" });
    expect(getVoiceSnapshot().session.sessionId).toBe(SESSION_B);
    expect(getVoiceSnapshot().session.transcript).toEqual([
      { role: "assistant", text: "B says" },
    ]);
  });

  it("a stale-id ended/failed event cannot touch the live session", () => {
    wire(SESSION_B, { kind: "lifecycle", state: "connected" });
    wire(SESSION_A, { kind: "ended", reason: "stopped" });
    wire(SESSION_A, { kind: "failed", code: "x", message: "y" });
    expect(getVoiceSnapshot().session.status).toBe("live");
    expect(getVoiceSnapshot().session.notice).toBeNull();
  });

  it("same-id state survives subscriber churn (existing durability kept)", () => {
    const unsubscribe = subscribeVoice(() => {});
    wire(SESSION_A, { kind: "lifecycle", state: "connected" });
    wire(SESSION_A, { kind: "transcript", role: "user", text: "still here" });
    unsubscribe();
    subscribeVoice(() => {})();
    expect(getVoiceSnapshot().session.status).toBe("live");
    expect(getVoiceSnapshot().session.transcript).toEqual([
      { role: "user", text: "still here" },
    ]);
  });

  it("markStarting clears the transcript optimistically (belt to the id guard)", () => {
    wire(SESSION_A, { kind: "transcript", role: "user", text: "residue" });
    markStarting();
    expect(getVoiceSnapshot().session.transcript).toEqual([]);
  });

  it("an envelope-less payload is dropped, not reduced", () => {
    dispatchVoiceEvent({ kind: "lifecycle", state: "connected" });
    expect(getVoiceSnapshot().session.status).toBe("idle");
  });
});

// Talk entry (owner decision 2026-10-09): pressing Talk opens a FRESH tutor
// for the book — the previous, finished conversation's rows never greet it —
// while a session that is still running is real and is never wiped.
describe("freshVoicePanelForBook", () => {
  function bookEvent(sessionId: string, event: unknown): void {
    dispatchVoiceEvent({ session_id: sessionId, book_id: "book-A", event });
  }

  it("clears the same book's finished conversation", () => {
    bindVoicePanelBook("book-A");
    bookEvent(TEST_SESSION, { kind: "lifecycle", state: "connected" });
    bookEvent(TEST_SESSION, { kind: "transcript", role: "user", text: "hi" });
    bookEvent(TEST_SESSION, { kind: "ended", reason: "user" });
    expect(getVoiceSnapshot().session.transcript.length).toBeGreaterThan(0);

    freshVoicePanelForBook("book-A");
    bindVoicePanelBook("book-A");

    const { session, starting, startError } = getVoiceSnapshot();
    expect(session.transcript).toHaveLength(0);
    expect(session.status).toBe("idle");
    expect(starting).toBe(false);
    expect(startError).toBeNull();
    expect(getVoiceBookScope()).toEqual({ bookId: "book-A" });
  });

  it("never wipes a session that is still live", () => {
    bindVoicePanelBook("book-A");
    bookEvent(TEST_SESSION, { kind: "lifecycle", state: "connected" });
    bookEvent(TEST_SESSION, { kind: "transcript", role: "user", text: "hi" });
    const before = getVoiceSnapshot();

    freshVoicePanelForBook("book-A");

    expect(getVoiceSnapshot()).toBe(before);
    expect(getVoiceSnapshot().session.status).toBe("live");
  });

  it("never wipes a start that is in flight", () => {
    bindVoicePanelBook("book-A");
    markStarting();
    freshVoicePanelForBook("book-A");
    expect(getVoiceSnapshot().starting).toBe(true);
  });
});

// Field session 2026-10-09: Continue opened an empty chat, so it looked like
// a fresh session. The conversation it picks up is shown above the new
// session's turns, and survives that session's first event.
describe("the conversation a Continue picks up", () => {
  const NEWER = "01920000-0000-7000-8000-000000000009";
  const turns = [
    { role: "user", text: "what is transparency?" },
    { role: "assistant", text: "Hiding distribution from the user." },
    { role: "system", text: "never shown" },
    { role: "assistant", text: "   " },
  ];

  it("survives the new session's first event and keeps only real turns", () => {
    bindVoicePanelBook("book-A");
    dispatchVoiceEvent({
      session_id: TEST_SESSION,
      book_id: "book-A",
      event: { kind: "ended", reason: "user" },
    });
    markStarting();
    showEarlierConversation("book-A", TEST_SESSION, turns);
    dispatchVoiceEvent({
      session_id: NEWER,
      book_id: "book-A",
      event: { kind: "lifecycle", state: "connected" },
    });
    const snapshot = getVoiceSnapshot();
    expect(snapshot.session.sessionId).toBe(NEWER);
    expect(snapshot.earlier?.sourceId).toBe(TEST_SESSION);
    expect(snapshot.earlier?.turns.map((turn) => turn.text)).toEqual([
      "what is transparency?",
      "Hiding distribution from the user.",
    ]);
  });

  it("is never shown for another book, and a fresh start clears it", () => {
    bindVoicePanelBook("book-A");
    showEarlierConversation("book-B", TEST_SESSION, turns);
    expect(getVoiceSnapshot().earlier).toBeNull();
    showEarlierConversation("book-A", TEST_SESSION, turns);
    expect(getVoiceSnapshot().earlier).not.toBeNull();
    clearEarlierConversation();
    expect(getVoiceSnapshot().earlier).toBeNull();
  });

  it("leaves with the book", () => {
    bindVoicePanelBook("book-A");
    showEarlierConversation("book-A", TEST_SESSION, turns);
    bindVoicePanelBook("book-B");
    expect(getVoiceSnapshot().earlier).toBeNull();
  });
});
