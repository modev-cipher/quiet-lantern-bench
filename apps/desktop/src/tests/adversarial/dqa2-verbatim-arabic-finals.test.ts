// D-QA2 — dropped words: short Arabic span finals must render VERBATIM.
//
// INVARIANT: a short Arabic final admitted as a span final ("رجعها لي.")
// renders byte-verbatim through span-join and transcript reduction — no
// dropped words, no rewriting, no punctuation-rule casualties.
//   Spec citations:
//   - voiceSessionModel.test.ts (contract, "seals user transcripts
//     verbatim — no word mapping ever rewrites them"): what the provider
//     transcribed is what the bubble shows, byte for byte.
//   - O3 display join (same contract file): consecutive span finals with
//     no real turn boundary join into ONE bubble; rows stay per-span.
//   - Punctuation law (same contract): a turn that accumulated ONLY
//     punctuation is dropped at seal — the attack below verifies a short
//     LEXICAL Arabic final is never misclassified into that law.
//   - F8 §2.3 (wire): rows arrive as `{ session_id, event }` envelopes
//     with snake_case `span_final` — the store path is attacked too.
//
// ATTACK STRATEGY: drive the production reducer AND the production store
// (envelope path) with the owner's exact utterance plus adversarial
// shapes: trailing Arabic punctuation (؟ ، .), very short utterances,
// RTL-mark-carrying text, and mixed-direction tokens ("ال points"). Byte
// equality is asserted with the exact JS string literal — any
// normalization, trimming, reordering, or punctuation-drop misfire fails.
//
// WHY A SHALLOW MOCK CANNOT FAKE A PASS: no mocks exist in the reducer
// tests at all — they call the shipped reduceVoiceSession on the shipped
// INITIAL state; the store tests stub only the inert tauri listener (the
// store suite's own idiom) and still exercise the full production
// dispatch → parse → reduce chain.

import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

import {
  INITIAL_VOICE_SESSION,
  reduceVoiceSession,
  type VoiceSessionEvent,
  type VoiceSessionState,
} from "../../voiceSessionModel";
import {
  dispatchVoiceEvent,
  getVoiceSnapshot,
  resetVoiceSessionStore,
} from "../../voiceSessionStore";

function reduceAll(events: VoiceSessionEvent[]): VoiceSessionState {
  return events.reduce(reduceVoiceSession, INITIAL_VOICE_SESSION);
}

const SESSION = "01920000-0000-7000-8000-0000000000d2";

beforeEach(() => resetVoiceSessionStore());

describe("D-QA2: the owner utterance, verbatim through the reducer", () => {
  it("'رجعها لي.' as a span final seals byte-verbatim", () => {
    const state = reduceAll([
      { kind: "transcript", role: "user", text: "رجعها لي.", spanFinal: true },
      { kind: "audio_state", state: "speaking", detail: "" },
    ]);
    expect(state.transcript).toEqual([
      { role: "user", text: "رجعها لي.", sealed: true },
    ]);
  });

  it("a very short Arabic final ('لا.') survives the punctuation-drop law", () => {
    // The law drops punctuation-ONLY turns; Arabic letters are lexical
    // content — a two-letter word with a trailing dot must never be
    // classified as punctuation and silently dropped at seal.
    const state = reduceAll([
      { kind: "transcript", role: "user", text: "لا.", spanFinal: true },
      { kind: "audio_state", state: "speaking", detail: "" },
    ]);
    expect(state.transcript).toEqual([
      { role: "user", text: "لا.", sealed: true },
    ]);
  });

  it("trailing Arabic punctuation (؟ ،) is kept verbatim, and a lone '؟' is not a word", () => {
    const kept = reduceAll([
      { kind: "transcript", role: "user", text: "رجعها لي؟", spanFinal: true },
      { kind: "audio_state", state: "speaking", detail: "" },
    ]);
    expect(kept.transcript).toEqual([
      { role: "user", text: "رجعها لي؟", sealed: true },
    ]);
    // The inverse guard: Arabic PUNCTUATION alone is still punctuation-only
    // (the law that must not eat the words above must still eat this).
    const lone = reduceAll([
      { kind: "transcript", role: "user", text: "؟" },
      { kind: "audio_state", state: "speaking", detail: "" },
    ]);
    expect(lone.transcript).toEqual([]);
  });

  it("two Arabic span finals of one utterance join without losing a byte", () => {
    const state = reduceAll([
      { kind: "transcript", role: "user", text: "رجعها", spanFinal: true },
      { kind: "audio_state", state: "speaking", detail: "" },
      { kind: "transcript", role: "user", text: "لي.", spanFinal: true },
    ]);
    expect(state.transcript).toHaveLength(1);
    const joined = state.transcript[0].text;
    // Every source byte present, in order — the join may add a single
    // separator but may never drop or reorder the words.
    expect(joined).toContain("رجعها");
    expect(joined).toContain("لي.");
    expect(joined.indexOf("رجعها")).toBeLessThan(joined.indexOf("لي."));
  });

  it("mixed-direction token ('ال points') passes byte-verbatim, no reordering", () => {
    const state = reduceAll([
      { kind: "transcript", role: "user", text: "ال points", spanFinal: true },
      { kind: "audio_state", state: "speaking", detail: "" },
    ]);
    expect(state.transcript).toEqual([
      { role: "user", text: "ال points", sealed: true },
    ]);
  });

  it("explicit RTL/LRM marks in the payload are preserved, never stripped", () => {
    const marked = "‏رجعها لي.‎";
    const state = reduceAll([
      { kind: "transcript", role: "user", text: marked, spanFinal: true },
      { kind: "audio_state", state: "speaking", detail: "" },
    ]);
    expect(state.transcript).toEqual([
      { role: "user", text: marked, sealed: true },
    ]);
  });

  it("an Arabic assistant reply is not rewritten either", () => {
    const state = reduceAll([
      { kind: "transcript", role: "user", text: "طيب", spanFinal: true },
      { kind: "transcript", role: "assistant", text: "تمام، رجعتها لك." },
      { kind: "transcript", role: "user", text: "شكراً" },
    ]);
    expect(state.transcript[1]).toEqual({
      role: "assistant",
      text: "تمام، رجعتها لك.",
      sealed: true,
    });
  });
});

describe("D-QA2: the same bytes through the wire envelope (store path)", () => {
  it("the snake_case span_final envelope renders the owner utterance verbatim", () => {
    dispatchVoiceEvent({
      session_id: SESSION,
      event: { kind: "lifecycle", state: "connected" },
    });
    dispatchVoiceEvent({
      session_id: SESSION,
      event: {
        kind: "transcript",
        role: "user",
        text: "رجعها لي.",
        span_final: true,
      },
    });
    dispatchVoiceEvent({
      session_id: SESSION,
      event: { kind: "audio_state", state: "speaking", detail: "" },
    });
    expect(getVoiceSnapshot().session.transcript).toEqual([
      { role: "user", text: "رجعها لي.", sealed: true },
    ]);
  });

  it("a short final is not glued onto a previous session's unsealed bubble", () => {
    // The F8 §1.3 coalescing hazard, attacked with the D-QA2 shape: an
    // unsealed trailing bubble from session A must never absorb B's first
    // short Arabic final.
    const OTHER = "01920000-0000-7000-8000-0000000000aa";
    dispatchVoiceEvent({
      session_id: OTHER,
      event: { kind: "transcript", role: "user", text: "leftover" },
    });
    dispatchVoiceEvent({
      session_id: SESSION,
      event: {
        kind: "transcript",
        role: "user",
        text: "رجعها لي.",
        span_final: true,
      },
    });
    expect(getVoiceSnapshot().session.transcript).toEqual([
      { role: "user", text: "رجعها لي." },
    ]);
  });
});
