import { describe, expect, it } from "vitest";

import {
  activityLabelKey,
  activityMessage,
  IDLE_ACTIVITY,
  stepMessage,
  adoptedSessionNotice,
  audioBadgeOf,
  isAlreadyRunningStartError,
  INITIAL_VOICE_SESSION,
  micBadgeOf,
  reduceVoiceSession,
  statusLabelOf,
  voiceSessionEventFrom,
  voiceprintFrom,
  VOICEPRINT_BARS,
  pagesReadIn,
  type VoiceSessionEvent,
  type VoiceSessionState,
  noticeText,
} from "./voiceSessionModel";

function reduceAll(
  events: VoiceSessionEvent[],
  from: VoiceSessionState = INITIAL_VOICE_SESSION,
): VoiceSessionState {
  return events.reduce(reduceVoiceSession, from);
}

describe("navigation surface receipts", () => {
  function requested() {
    const request = voiceSessionEventFrom({
      kind: "navigation_requested",
      call_id: "page-28",
      generation: 0,
      tool: "jump_to_page",
      pos_ms: 32420,
      page_no: 28,
    });
    expect(request).not.toBeNull();
    return reduceAll([
      { kind: "activity", state: "tool", detail: "jump_to_page", target: "28" },
      request!,
      { kind: "activity", state: "thinking", detail: "after_tool" },
    ]);
  }

  it("a request is pending rather than a completed page visit", () => {
    const state = requested();
    expect(stepMessage(state.activity.steps[0]).key).toBe(
      "voice.step.navigationPending",
    );
    expect(pagesReadIn(state.activity.steps)).toEqual([]);
  });

  it("only an applied surface result promotes the matching step", () => {
    const pending = requested();
    const unrelated = reduceVoiceSession(pending, {
      kind: "navigation_settled",
      callId: "other",
      tool: "jump_to_page",
      applied: true,
      held: false,
    });
    expect(unrelated.activity.steps[0].status).toBe("pending");
    const applied = reduceVoiceSession(pending, {
      kind: "navigation_settled",
      callId: "page-28",
      tool: "jump_to_page",
      applied: true,
      held: false,
    });
    expect(applied.activity.steps[0].status).toBeUndefined();
    expect(pagesReadIn(applied.activity.steps)).toEqual([28]);
  });

  it("a held page remains unvisited and an unavailable page is failed", () => {
    const pending = requested();
    const held = reduceVoiceSession(pending, {
      kind: "navigation_settled",
      callId: "page-28",
      tool: "jump_to_page",
      applied: false,
      held: true,
    });
    expect(stepMessage(held.activity.steps[0]).key).toBe(
      "voice.step.navigationHeld",
    );
    expect(pagesReadIn(held.activity.steps)).toEqual([]);
    const failed = reduceVoiceSession(pending, {
      kind: "navigation_settled",
      callId: "page-28",
      tool: "jump_to_page",
      applied: false,
      held: false,
    });
    expect(failed.activity.steps[0].failed).toBe(true);
    expect(failed.navigation).toBeNull();
  });

  it("rejects malformed request identity and page numbers", () => {
    const valid = {
      kind: "navigation_requested",
      call_id: "page-28",
      generation: 0,
      tool: "jump_to_page",
      pos_ms: 32420,
      page_no: 28,
    };
    for (const change of [
      { page_no: -1 },
      { page_no: 1.5 },
      { generation: -1 },
      { generation: Number.NaN },
      { call_id: "" },
    ]) {
      expect(voiceSessionEventFrom({ ...valid, ...change })).toBeNull();
    }
  });
});

describe("voiceSessionEventFrom", () => {
  it("parses every runtime event kind", () => {
    expect(
      voiceSessionEventFrom({ kind: "lifecycle", state: "connected" }),
    ).toEqual({ kind: "lifecycle", state: "connected" });
    expect(
      voiceSessionEventFrom({ kind: "transcript", role: "user", text: "hi" }),
    ).toEqual({ kind: "transcript", role: "user", text: "hi" });
    expect(voiceSessionEventFrom({ kind: "ended", reason: "stopped" })).toEqual(
      { kind: "ended", reason: "stopped" },
    );
    expect(
      voiceSessionEventFrom({ kind: "failed", code: "x", message: "y" }),
    ).toEqual({ kind: "failed", code: "x", message: "y" });
    expect(voiceSessionEventFrom({ kind: "turn_boundary" })).toEqual({
      kind: "turn_boundary",
    });
  });

  it("refuses malformed payloads instead of guessing", () => {
    expect(voiceSessionEventFrom(null)).toBeNull();
    expect(voiceSessionEventFrom("connected")).toBeNull();
    expect(voiceSessionEventFrom({ kind: "lifecycle" })).toBeNull();
    expect(
      voiceSessionEventFrom({ kind: "transcript", role: "system", text: "x" }),
    ).toBeNull();
    expect(
      voiceSessionEventFrom({ kind: "transcript", role: "user" }),
    ).toBeNull();
    expect(voiceSessionEventFrom({ kind: "failed", code: "x" })).toBeNull();
    expect(voiceSessionEventFrom({ kind: "surprise" })).toBeNull();
  });
});

describe("reduceVoiceSession", () => {
  it("walks the lifecycle: connected → renewing → reconnected", () => {
    let state = reduceAll([{ kind: "lifecycle", state: "connected" }]);
    expect(state.status).toBe("live");
    expect(noticeText(state)).toBeNull();

    state = reduceVoiceSession(state, { kind: "lifecycle", state: "renewing" });
    expect(state.status).toBe("renewing");
    expect(noticeText(state)).toMatch(/reconnecting/i);

    state = reduceVoiceSession(state, {
      kind: "lifecycle",
      state: "reconnected",
    });
    expect(state.status).toBe("live");
    expect(noticeText(state)).toBeNull();
  });

  it("merges consecutive same-role transcript deltas into one entry", () => {
    const state = reduceAll([
      { kind: "transcript", role: "assistant", text: "The loss " },
      { kind: "transcript", role: "assistant", text: "curve flattens." },
      { kind: "transcript", role: "user", text: "why?" },
      { kind: "transcript", role: "assistant", text: "Because…" },
    ]);
    expect(state.transcript).toEqual([
      { role: "assistant", text: "The loss curve flattens.", sealed: true },
      { role: "user", text: "why?", sealed: true },
      { role: "assistant", text: "Because…" },
    ]);
  });

  it("seals the open assistant bubble at a turn boundary — the reply after a suppressed user flip never coalesces (D1, 2026-09-28)", () => {
    const state = reduceAll([
      {
        kind: "transcript",
        role: "assistant",
        text: "واضح الفرق بين الاتنين؟",
      },
      { kind: "turn_boundary" },
      {
        kind: "transcript",
        role: "assistant",
        text: "بالظبط، فهمك مظبوط جداً.",
      },
      {
        kind: "transcript",
        role: "user",
        text: "اه يعني اللي انا فهمته",
        spanFinal: true,
      },
    ]);
    expect(state.transcript).toEqual([
      { role: "assistant", text: "واضح الفرق بين الاتنين؟", sealed: true },
      { role: "assistant", text: "بالظبط، فهمك مظبوط جداً.", sealed: true },
      { role: "user", text: "اه يعني اللي انا فهمته" },
    ]);
  });

  it("corrects the last user row in place when the late stream final arrives (4a, 2026-09-28)", () => {
    const state = reduceAll([
      { kind: "transcript", role: "user", text: "تمام" },
      {
        kind: "transcript",
        role: "user",
        text: "تمام كمل",
        correctsLast: true,
      },
    ]);
    expect(state.transcript).toEqual([{ role: "user", text: "تمام كمل" }]);
  });

  it("splits pre-tool and post-tool speech at the boundary, mirroring the journal rows (2026-09-28 sync gate)", () => {
    const state = reduceAll([
      {
        kind: "transcript",
        role: "assistant",
        text: "Let's check Chapter 1. ",
      },
      { kind: "turn_boundary" },
      {
        kind: "navigation",
        tool: "jump_to_chapter",
        posMs: 69730,
        pageNo: 73,
        chapterIdx: 15,
      },
      { kind: "transcript", role: "assistant", text: "Now we see it." },
    ]);
    expect(state.transcript).toEqual([
      { role: "assistant", text: "Let's check Chapter 1. ", sealed: true },
      { role: "assistant", text: "Now we see it." },
    ]);
  });

  it("keeps consecutive typed user messages as separate entries", () => {
    const state = reduceAll([
      { kind: "transcript", role: "user", text: "hello", typed: true },
      { kind: "transcript", role: "user", text: "are you there?", typed: true },
      { kind: "transcript", role: "assistant", text: "yes." },
    ]);
    expect(state.transcript).toEqual([
      { role: "user", text: "hello", sealed: true, typed: true },
      { role: "user", text: "are you there?", sealed: true, typed: true },
      { role: "assistant", text: "yes." },
    ]);
  });

  it("seals user transcripts verbatim — no word mapping ever rewrites them", () => {
    // The English-word aligner is gone (it fabricated words in user bubbles,
    // owner sessions 2026-08-28/29); what the provider transcribed is what
    // the bubble shows, byte for byte.
    const state = reduceAll([
      { kind: "transcript", role: "user", text: " افهم البي" },
      { kind: "transcript", role: "user", text: "دج ده" },
      { kind: "audio_state", state: "speaking", detail: "" },
    ]);
    expect(state.transcript).toEqual([
      { role: "user", text: " افهم البيدج ده", sealed: true },
    ]);
  });

  it("the speaking edge seals the open user entry as heard", () => {
    const state = reduceAll([
      { kind: "transcript", role: "user", text: "الكونسبتس" },
      { kind: "audio_state", state: "speaking", detail: "" },
    ]);
    expect(state.transcript).toEqual([
      { role: "user", text: "الكونسبتس", sealed: true },
    ]);
  });

  it("a punctuation-only voice delta never opens a bubble (owner lone-dot, 2026-08-28)", () => {
    // The trailing "." of an utterance lands after the speaking edge sealed
    // the user entry; it must not start a fresh lone-dot bubble.
    const state = reduceAll([
      { kind: "transcript", role: "user", text: "you hear me" },
      { kind: "audio_state", state: "speaking", detail: "" },
      { kind: "transcript", role: "user", text: "." },
      { kind: "transcript", role: "assistant", text: "نعم، أسمعك." },
    ]);
    expect(state.transcript).toEqual([
      { role: "user", text: "you hear me", sealed: true },
      { role: "assistant", text: "نعم، أسمعك." },
    ]);
  });

  it("a turn that accumulated only punctuation is dropped at seal, any edge", () => {
    const speaking = reduceAll([
      { kind: "transcript", role: "user", text: "…" },
      { kind: "transcript", role: "user", text: "." },
      { kind: "audio_state", state: "speaking", detail: "" },
    ]);
    expect(speaking.transcript).toEqual([]);
    const roleFlip = reduceAll([
      { kind: "transcript", role: "user", text: ".." },
      { kind: "transcript", role: "assistant", text: "ok" },
    ]);
    expect(roleFlip.transcript).toEqual([{ role: "assistant", text: "ok" }]);
  });

  it("an assistant reply may OPEN with pure punctuation — bubble matches journal", () => {
    const state = reduceAll([
      { kind: "transcript", role: "user", text: "go on" },
      { kind: "transcript", role: "assistant", text: "…" },
      { kind: "transcript", role: "assistant", text: "right." },
    ]);
    expect(state.transcript).toEqual([
      { role: "user", text: "go on", sealed: true },
      { role: "assistant", text: "…right." },
    ]);
  });

  it("punctuation deltas still extend an open lexical bubble", () => {
    const state = reduceAll([
      { kind: "transcript", role: "user", text: "wait" },
      { kind: "transcript", role: "user", text: "..." },
    ]);
    expect(state.transcript).toEqual([{ role: "user", text: "wait..." }]);
  });

  it("typed sends land sealed, marked typed, byte-identical", () => {
    const state = reduceAll([
      { kind: "transcript", role: "user", text: "البيدج", typed: true },
    ]);
    expect(state.transcript).toEqual([
      { role: "user", text: "البيدج", sealed: true, typed: true },
    ]);
  });

  it("coalesces spoken user deltas into one entry, exactly as sent", () => {
    const state = reduceAll([
      { kind: "transcript", role: "user", text: "wha" },
      { kind: "transcript", role: "user", text: "t is entro" },
      { kind: "transcript", role: "user", text: "py?" },
    ]);
    expect(state.transcript).toEqual([
      { role: "user", text: "what is entropy?" },
    ]);
  });

  it("spoken deltas never merge into a typed entry", () => {
    const state = reduceAll([
      { kind: "transcript", role: "user", text: "define entropy", typed: true },
      { kind: "transcript", role: "user", text: "and also " },
      { kind: "transcript", role: "user", text: "enthalpy" },
    ]);
    expect(state.transcript).toEqual([
      { role: "user", text: "define entropy", sealed: true, typed: true },
      { role: "user", text: "and also enthalpy" },
    ]);
  });

  it("audio edges never split a reply: a chunked stream reassembles losslessly", () => {
    // Owner session 2026-08-29: playback drains between audio chunks, so the
    // plane's speaking/idle edges fire MID-reply. Sealing there shredded
    // "three different tables: … data …" into one-word bubbles. The pin: a
    // reply whose transcription arrives interleaved with arbitrary
    // speaking/idle edges lands as ONE bubble carrying every received
    // character, and the next user utterance is what closes it.
    const state = reduceAll([
      { kind: "transcript", role: "user", text: "how many tables?" },
      { kind: "audio_state", state: "speaking", detail: "" },
      { kind: "transcript", role: "assistant", text: "three" },
      { kind: "audio_state", state: "idle", detail: "" },
      { kind: "audio_state", state: "speaking", detail: "" },
      { kind: "transcript", role: "assistant", text: " different tables:" },
      { kind: "audio_state", state: "idle", detail: "" },
      {
        kind: "transcript",
        role: "assistant",
        text: " orders and customers hold the",
      },
      { kind: "audio_state", state: "speaking", detail: "" },
      { kind: "transcript", role: "assistant", text: " data" },
      { kind: "audio_state", state: "idle", detail: "" },
      { kind: "transcript", role: "assistant", text: " from these tables." },
      { kind: "transcript", role: "user", text: "شكراً" },
    ]);
    expect(state.transcript).toEqual([
      { role: "user", text: "how many tables?", sealed: true },
      {
        role: "assistant",
        text: "three different tables: orders and customers hold the data from these tables.",
        sealed: true,
      },
      { role: "user", text: "شكراً" },
    ]);
  });

  it("session end seals the open entries", () => {
    const state = reduceAll([
      { kind: "transcript", role: "assistant", text: "Bye." },
      { kind: "ended", reason: "stopped" },
      { kind: "transcript", role: "assistant", text: "straggler" },
    ]);
    expect(state.transcript).toEqual([
      { role: "assistant", text: "Bye.", sealed: true },
      { role: "assistant", text: "straggler" },
    ]);
  });

  it("caps the transcript instead of growing unbounded", () => {
    const alternating: VoiceSessionEvent[] = Array.from(
      { length: 500 },
      (_, at) => ({
        kind: "transcript",
        role: at % 2 === 0 ? "user" : "assistant",
        text: `turn ${at}`,
      }),
    );
    const state = reduceAll(alternating);
    expect(state.transcript.length).toBeLessThanOrEqual(200);
    expect(state.transcript[state.transcript.length - 1].text).toBe("turn 499");
  });

  it("keeps failures visible without killing the session", () => {
    const state = reduceAll([
      { kind: "lifecycle", state: "connected" },
      { kind: "failed", code: "send_text", message: "socket closed" },
    ]);
    expect(state.status).toBe("live");
    expect(noticeText(state)).toBe("send_text: socket closed");
  });

  it("ends with the runtime's reason on display", () => {
    const state = reduceAll([
      { kind: "lifecycle", state: "connected" },
      { kind: "ended", reason: "the provider session expired" },
    ]);
    expect(state.status).toBe("ended");
    expect(noticeText(state)).toBe(
      "Session ended: the provider session expired",
    );
  });

  // Kritias round 5 finding 3: the PRODUCTION sequence at teardown is a
  // voice_turn_uncertain / voice_turn_lost failure immediately followed by
  // Ended — the terminal line must fold the loss in, not overwrite it.
  it("a teardown turn-loss notice survives the ended event", () => {
    for (const code of ["voice_turn_uncertain", "voice_turn_lost"]) {
      const state = reduceAll([
        { kind: "lifecycle", state: "connected" },
        {
          kind: "failed",
          code,
          message:
            "The tutor may not have heard part of what you said — it could " +
            "not be verified against your transcript. Please repeat if it " +
            "went unanswered.",
        },
        { kind: "ended", reason: "stopped" },
      ]);
      expect(state.status).toBe("ended");
      expect(noticeText(state)).toContain("Session ended: stopped");
      expect(noticeText(state)).toContain(
        "may not have heard part of what you said",
      );
    }
  });

  it("a non-loss failure does not ride into the terminal line", () => {
    const state = reduceAll([
      { kind: "lifecycle", state: "connected" },
      { kind: "failed", code: "send_text", message: "socket closed" },
      { kind: "ended", reason: "stopped" },
    ]);
    expect(noticeText(state)).toBe("Session ended: stopped");
  });

  it("announces expiry before the ended event lands", () => {
    const state = reduceAll([
      { kind: "lifecycle", state: "connected" },
      { kind: "lifecycle", state: "expired" },
    ]);
    expect(state.status).toBe("live");
    expect(noticeText(state)).toMatch(/expired/i);
  });
});

describe("audio state (VA6 output plane)", () => {
  it("parses audio_state and refuses malformed ones", () => {
    expect(
      voiceSessionEventFrom({
        kind: "audio_state",
        state: "active",
        detail: "",
      }),
    ).toEqual({ kind: "audio_state", state: "active", detail: "" });
    expect(
      voiceSessionEventFrom({ kind: "audio_state", state: "active" }),
    ).toBeNull();
    expect(
      voiceSessionEventFrom({ kind: "audio_state", state: 1, detail: "" }),
    ).toBeNull();
  });

  it("tracks speaking edges over a live device", () => {
    let state = reduceAll([
      { kind: "lifecycle", state: "connected" },
      { kind: "audio_state", state: "active", detail: "" },
    ]);
    expect(state.audio).toBe("active");
    state = reduceVoiceSession(state, {
      kind: "audio_state",
      state: "speaking",
      detail: "",
    });
    expect(state.audio).toBe("speaking");
    state = reduceVoiceSession(state, {
      kind: "audio_state",
      state: "idle",
      detail: "",
    });
    expect(state.audio).toBe("active");
    expect(noticeText(state)).toBeNull();
  });

  it("says so when there is no device, and when the device dies", () => {
    const unavailable = reduceAll([
      { kind: "audio_state", state: "unavailable", detail: "no device" },
    ]);
    expect(unavailable.audio).toBe("unavailable");
    expect(noticeText(unavailable)).toMatch(/text/i);

    const lost = reduceAll([
      { kind: "audio_state", state: "active", detail: "" },
      { kind: "audio_state", state: "speaking", detail: "" },
      { kind: "audio_state", state: "lost", detail: "ring jammed" },
    ]);
    expect(lost.audio).toBe("lost");
    expect(noticeText(lost)).toMatch(/lost/i);
  });

  it("ignores audio states it does not know (forward compat)", () => {
    const state = reduceAll([
      { kind: "audio_state", state: "active", detail: "" },
      { kind: "audio_state", state: "quadraphonic", detail: "" },
    ]);
    expect(state.audio).toBe("active");
  });

  it("badges every audio state honestly", () => {
    expect(audioBadgeOf("unknown")).toBeNull();
    expect(audioBadgeOf("active")).toBe("Audio on");
    expect(audioBadgeOf("speaking")).toBe("Speaking…");
    expect(audioBadgeOf("unavailable")).toBe("Text only");
    expect(audioBadgeOf("lost")).toBe("Text only");
  });
});

describe("the runtime wire, parsed then reduced (VA6 output, G5)", () => {
  // The exact JSON the Rust side emits (SessionUiEvent serde shape, pinned
  // byte-for-byte in voice_session.rs tests) — through the SAME two steps
  // the panel runs: defensive parse, then reduce. Role A pinned each step
  // alone; a drift between them (a parse that narrows, a reducer keyed on
  // a string the parse never yields) only shows up composed.
  function arrive(state: VoiceSessionState, payload: unknown) {
    const parsed = voiceSessionEventFrom(payload);
    expect(
      parsed,
      `the runtime payload was refused: ${JSON.stringify(payload)}`,
    ).not.toBeNull();
    return reduceVoiceSession(state, parsed as VoiceSessionEvent);
  }

  it("drives the badge through one audible response, then a mid-speech death", () => {
    let state = arrive(INITIAL_VOICE_SESSION, {
      kind: "audio_state",
      state: "active",
      detail: "",
    });
    expect(audioBadgeOf(state.audio)).toBe("Audio on");

    state = arrive(state, {
      kind: "audio_state",
      state: "speaking",
      detail: "",
    });
    expect(audioBadgeOf(state.audio)).toBe("Speaking…");

    state = arrive(state, { kind: "audio_state", state: "idle", detail: "" });
    expect(audioBadgeOf(state.audio)).toBe("Audio on");

    state = arrive(state, {
      kind: "audio_state",
      state: "speaking",
      detail: "",
    });
    state = arrive(state, {
      kind: "audio_state",
      state: "lost",
      detail:
        "playback ring refused audio for 100 ticks; the output device stopped draining",
    });
    expect(audioBadgeOf(state.audio)).toBe("Text only");
    expect(noticeText(state)).toMatch(/lost/i);
  });

  it("renders text-first from the very first event when no device opened", () => {
    const state = arrive(INITIAL_VOICE_SESSION, {
      kind: "audio_state",
      state: "unavailable",
      detail: "no output device",
    });
    expect(audioBadgeOf(state.audio)).toBe("Text only");
    expect(noticeText(state)).toMatch(/text/i);
  });

  it("keeps a failure notice on display while the session flows on", () => {
    let state = arrive(INITIAL_VOICE_SESSION, {
      kind: "lifecycle",
      state: "connected",
    });
    state = arrive(state, {
      kind: "failed",
      code: "interrupt",
      message: "socket closed",
    });
    // Non-fatal by contract: later traffic must not scrub the notice —
    // only an explicit lifecycle recovery may.
    state = arrive(state, {
      kind: "transcript",
      role: "assistant",
      text: "…as I was saying",
    });
    state = arrive(state, {
      kind: "audio_state",
      state: "speaking",
      detail: "",
    });
    expect(state.status).toBe("live");
    expect(noticeText(state)).toBe("interrupt: socket closed");

    state = arrive(state, { kind: "lifecycle", state: "reconnected" });
    expect(noticeText(state)).toBeNull();
  });
});

describe("mic state (R2 Interactive mode)", () => {
  it("parses mic_state and refuses malformed ones", () => {
    expect(
      voiceSessionEventFrom({
        kind: "mic_state",
        state: "listening",
        detail: "",
      }),
    ).toEqual({ kind: "mic_state", state: "listening", detail: "" });
    expect(
      voiceSessionEventFrom({ kind: "mic_state", state: "muted" }),
    ).toBeNull();
    expect(voiceSessionEventFrom({ kind: "mic_state", detail: "" })).toBeNull();
  });

  it("starts idle: no mic promise before the runtime says anything", () => {
    expect(INITIAL_VOICE_SESSION.mic).toBe("idle");
    expect(micBadgeOf("idle")).toBeNull();
  });

  it("walks Interactive mode: listening → muted → listening", () => {
    let state = reduceAll([
      { kind: "lifecycle", state: "connected" },
      { kind: "mic_state", state: "listening", detail: "" },
    ]);
    expect(state.mic).toBe("listening");
    state = reduceVoiceSession(state, {
      kind: "mic_state",
      state: "muted",
      detail: "",
    });
    expect(state.mic).toBe("muted");
    state = reduceVoiceSession(state, {
      kind: "mic_state",
      state: "listening",
      detail: "",
    });
    expect(state.mic).toBe("listening");
  });

  it("says so when there is no mic, and when the mic dies", () => {
    const unavailable = reduceAll([
      { kind: "mic_state", state: "unavailable", detail: "no device" },
    ]);
    expect(unavailable.mic).toBe("unavailable");
    expect(noticeText(unavailable)).toBe(
      "No microphone — type to ask instead.",
    );

    const lost = reduceAll([
      { kind: "mic_state", state: "listening", detail: "" },
      { kind: "mic_state", state: "lost", detail: "stopped delivering" },
    ]);
    expect(lost.mic).toBe("lost");
    expect(noticeText(lost)).toBe(
      "The microphone was lost — continuing without it.",
    );
  });

  it("ignores mic states it does not know (forward compat)", () => {
    const state = reduceAll([
      { kind: "mic_state", state: "listening", detail: "" },
      { kind: "mic_state", state: "quantum", detail: "" },
    ]);
    expect(state.mic).toBe("listening");
  });

  it("keeps the mic and audio planes independent", () => {
    // Listening while the tutor speaks is the full-duplex promise: neither
    // plane's edge may clobber the other's state.
    const state = reduceAll([
      { kind: "lifecycle", state: "connected" },
      { kind: "audio_state", state: "active", detail: "" },
      { kind: "mic_state", state: "listening", detail: "" },
      { kind: "audio_state", state: "speaking", detail: "" },
    ]);
    expect(state.mic).toBe("listening");
    expect(state.audio).toBe("speaking");
  });

  it("badges every mic state honestly, in words distinct from the audio badge", () => {
    expect(micBadgeOf("listening")).toBe("Listening");
    expect(micBadgeOf("muted")).toBe("Muted");
    expect(micBadgeOf("unavailable")).toBe("No mic");
    expect(micBadgeOf("lost")).toBe("No mic");
    expect(micBadgeOf("listening")).not.toBe(audioBadgeOf("speaking"));
  });
});

describe("mic state, adversarial (R2 hardening)", () => {
  it("returns to idle when the runtime says idle (Mute on a plane-less mic)", () => {
    const state = reduceAll([
      { kind: "mic_state", state: "listening", detail: "" },
      { kind: "mic_state", state: "idle", detail: "" },
    ]);
    expect(state.mic).toBe("idle");
    expect(micBadgeOf(state.mic)).toBeNull();
  });

  it("refuses mic_state payloads with non-string fields", () => {
    expect(
      voiceSessionEventFrom({ kind: "mic_state", state: 42, detail: "" }),
    ).toBeNull();
    expect(
      voiceSessionEventFrom({ kind: "mic_state", state: "muted", detail: 7 }),
    ).toBeNull();
    expect(
      voiceSessionEventFrom({ kind: "mic_state", state: null, detail: "" }),
    ).toBeNull();
  });

  it("rides only mic_state events: lifecycle churn never moves the mic", () => {
    // The UI half of 'the mic survives renewals' (runtime pin): a renewal
    // round-trip must not invent or drop Interactive mode.
    const state = reduceAll([
      { kind: "lifecycle", state: "connected" },
      { kind: "mic_state", state: "muted", detail: "" },
      { kind: "lifecycle", state: "renewing" },
      { kind: "lifecycle", state: "reconnected" },
    ]);
    expect(state.mic).toBe("muted");
    expect(state.status).toBe("live");
  });

  it("keeps the mic through non-fatal failures (the renewing refusal)", () => {
    // A refused mid-recovery Mute surfaces as Failed{renewing}; the mic is
    // still hot and the badge must keep saying so (runtime pin: a refused
    // Mute does not latch).
    const state = reduceAll([
      { kind: "mic_state", state: "listening", detail: "" },
      { kind: "failed", code: "renewing", message: "try again in a moment" },
    ]);
    expect(state.mic).toBe("listening");
    expect(noticeText(state)).toBe("renewing: try again in a moment");
  });

  it("drops the mic to idle when the session ends (no hot badge on a dead session)", () => {
    // The runtime drops the mic plane with the session; a Listening badge
    // (and its live Mute button) on an ended session promises a capture
    // state that no longer exists — the OS mic indicator is already dark.
    const state = reduceAll([
      { kind: "lifecycle", state: "connected" },
      { kind: "mic_state", state: "listening", detail: "" },
      { kind: "ended", reason: "stopped" },
    ]);
    expect(state.status).toBe("ended");
    expect(state.mic).toBe("idle");
  });
});

describe("statusLabelOf", () => {
  it("labels every status", () => {
    expect(statusLabelOf("idle")).toBe("Off");
    expect(statusLabelOf("starting")).toBe("Starting…");
    expect(statusLabelOf("live")).toBe("Live");
    expect(statusLabelOf("renewing")).toBe("Reconnecting");
    expect(statusLabelOf("ended")).toBe("Ended");
  });
});

describe("adopting an already-running session (the dead-end fix)", () => {
  it("recognizes the runtime's AlreadyRunning refusal, nothing else", () => {
    expect(
      isAlreadyRunningStartError("a voice session is already running"),
    ).toBe(true);
    expect(isAlreadyRunningStartError("channel connect failed: x")).toBe(false);
    expect(
      isAlreadyRunningStartError("no usable key for the selected provider"),
    ).toBe(false);
  });

  it("adoption turns a forgotten session live with an honest gap notice", () => {
    const state = reduceAll([{ kind: "adopted" }]);
    expect(state.status).toBe("live");
    expect(noticeText(state)).toBe(adoptedSessionNotice());
  });

  it("adoption claims nothing about the mic or audio planes", () => {
    // The runtime never replayed their state; promising either would be a
    // false badge until real events arrive.
    const state = reduceAll([{ kind: "adopted" }]);
    expect(state.mic).toBe("idle");
    expect(state.audio).toBe("unknown");
    expect(state.transcript).toEqual([]);
  });

  it("real runtime events supersede the adoption notice", () => {
    const state = reduceAll([
      { kind: "adopted" },
      { kind: "lifecycle", state: "connected" },
    ]);
    expect(state.status).toBe("live");
    expect(noticeText(state)).toBeNull();
  });
});

describe("O3 display join (per-span stream finals, owner ruling)", () => {
  it("parses the runtime's span_final display hint", () => {
    expect(
      voiceSessionEventFrom({
        kind: "transcript",
        role: "user",
        text: "one.",
        span_final: true,
      }),
    ).toEqual({
      kind: "transcript",
      role: "user",
      text: "one.",
      spanFinal: true,
    });
    // Absent or non-true stays the exact prior shape.
    expect(
      voiceSessionEventFrom({ kind: "transcript", role: "user", text: "one." }),
    ).toEqual({ kind: "transcript", role: "user", text: "one." });
  });

  it("joins per-span finals of one utterance into one cohesive bubble", () => {
    // The client VAD split one spoken utterance at an intra-utterance pause;
    // the runtime emitted TWO per-span finals (rows stay per-span). Only a
    // speaking edge intervened — no assistant reply — so the display shows
    // one bubble.
    const state = reduceAll([
      {
        kind: "transcript",
        role: "user",
        text: "What is a graph database,",
        spanFinal: true,
      },
      { kind: "audio_state", state: "speaking", detail: "" },
      {
        kind: "transcript",
        role: "user",
        text: "and when should I use one?",
        spanFinal: true,
      },
    ]);
    expect(state.transcript).toEqual([
      {
        role: "user",
        text: "What is a graph database, and when should I use one?",
        sealed: true,
      },
    ]);
  });

  it("never joins across a real turn boundary: an assistant reply", () => {
    const state = reduceAll([
      {
        kind: "transcript",
        role: "user",
        text: "First question.",
        spanFinal: true,
      },
      { kind: "transcript", role: "assistant", text: "An answer." },
      {
        kind: "transcript",
        role: "user",
        text: "Second question.",
        spanFinal: true,
      },
    ]);
    expect(state.transcript).toEqual([
      { role: "user", text: "First question.", sealed: true },
      { role: "assistant", text: "An answer.", sealed: true },
      { role: "user", text: "Second question." },
    ]);
  });

  it("never joins across a real turn boundary: a typed send", () => {
    const state = reduceAll([
      { kind: "transcript", role: "user", text: "typed line", typed: true },
      {
        kind: "transcript",
        role: "user",
        text: "spoken final",
        spanFinal: true,
      },
    ]);
    expect(state.transcript).toEqual([
      { role: "user", text: "typed line", sealed: true, typed: true },
      { role: "user", text: "spoken final" },
    ]);
  });

  it("plain streaming deltas keep their exact prior behavior", () => {
    // No span_final: a sealed user bubble still closes and the next delta
    // opens a fresh one — the join is opt-in by the runtime's hint only.
    const state = reduceAll([
      { kind: "transcript", role: "user", text: "first turn" },
      { kind: "audio_state", state: "speaking", detail: "" },
      { kind: "transcript", role: "user", text: "second turn" },
    ]);
    expect(state.transcript).toEqual([
      { role: "user", text: "first turn", sealed: true },
      { role: "user", text: "second turn" },
    ]);
  });
});

// F8 §2.4 D4: the runtime's Navigation events (successful jumps) parse from
// the wire and accumulate as a nonce-stamped edge the reader consumes.
describe("navigation events", () => {
  it("a navigation that reached no page surfaces as a notice, never vanishes (F10-D2)", () => {
    // The runtime only sends Navigation for an applied move; a move the
    // reader cannot turn to must still be visible, or the tutor's narration
    // is the sole witness that anything happened.
    const state = reduceAll([
      { kind: "lifecycle", state: "live" },
      {
        kind: "navigation",
        tool: "jump_to_chapter",
        posMs: 20,
        pageNo: null,
        chapterIdx: 1,
      },
    ]);
    expect(state.navigation?.pageNo).toBeNull();
    expect(noticeText(state)).toMatch(/page/i);
  });

  it("navigation notice overrides prior keyed notice (TC-UIM-01)", () => {
    const state = reduceAll([
      { kind: "lifecycle", state: "live" },
      { kind: "mic_state", state: "unavailable", detail: "no device" },
      {
        kind: "navigation",
        tool: "jump_to_chapter",
        posMs: 20,
        pageNo: null,
        chapterIdx: 1,
      },
    ]);
    expect(state.noticeKey).toBe("voice.notice.navigatedNoPage");
    expect(noticeText(state)).toMatch(/page/i);
  });

  it("parses the wire shape, tolerating absent page/chapter", () => {
    expect(
      voiceSessionEventFrom({
        kind: "navigation",
        tool: "jump_to_page",
        pos_ms: 2000,
        page_no: 2,
        chapter_idx: 1,
      }),
    ).toEqual({
      kind: "navigation",
      tool: "jump_to_page",
      posMs: 2000,
      pageNo: 2,
      chapterIdx: 1,
    });
    expect(
      voiceSessionEventFrom({
        kind: "navigation",
        tool: "replay_last_minute",
        pos_ms: 500,
      }),
    ).toEqual({
      kind: "navigation",
      tool: "replay_last_minute",
      posMs: 500,
      pageNo: null,
      chapterIdx: null,
    });
    expect(voiceSessionEventFrom({ kind: "navigation" })).toBeNull();
  });

  it("each event advances the nonce so a repeat jump is still an edge", () => {
    const state = reduceAll([
      {
        kind: "navigation",
        tool: "jump_to_page",
        posMs: 2000,
        pageNo: 2,
        chapterIdx: 1,
      },
      {
        kind: "navigation",
        tool: "jump_to_page",
        posMs: 2000,
        pageNo: 2,
        chapterIdx: 1,
      },
    ]);
    expect(state.navigation).toEqual({
      nonce: 2,
      tool: "jump_to_page",
      posMs: 2000,
      pageNo: 2,
      chapterIdx: 1,
    });
  });

  it("navigation preserves the open assistant entry unsealed without altering text or notice", () => {
    const state = reduceAll([
      { kind: "transcript", role: "assistant", text: "jumping" },
      {
        kind: "navigation",
        tool: "jump_to_chapter",
        posMs: 0,
        pageNo: 1,
        chapterIdx: 0,
      },
    ]);
    expect(state.transcript).toEqual([{ role: "assistant", text: "jumping" }]);
    expect(noticeText(state)).toBeNull();
  });
});

// Field session 2026-10-09: the user could not tell the writing tutor from
// the voice tutor, and a minute of waiting showed nothing but an error.
describe("tutor mode and activity", () => {
  it("parses mode and activity events, and refuses malformed ones", () => {
    expect(
      voiceSessionEventFrom({ kind: "mode", mode: "voice", model: "m" }),
    ).toEqual({ kind: "mode", mode: "voice", model: "m" });
    expect(
      voiceSessionEventFrom({ kind: "activity", state: "tool", detail: "x" }),
    ).toEqual({ kind: "activity", state: "tool", detail: "x", target: "" });
    expect(
      voiceSessionEventFrom({
        kind: "activity",
        state: "tool",
        detail: "jump_to_page",
        target: "23",
      }),
    ).toEqual({
      kind: "activity",
      state: "tool",
      detail: "jump_to_page",
      target: "23",
    });
    expect(
      voiceSessionEventFrom({ kind: "mode", mode: "radio", model: "m" }),
    ).toBeNull();
    expect(voiceSessionEventFrom({ kind: "activity", state: 3 })).toBeNull();
  });

  it("back on the writing tutor drops the voice tutor's mic and audio badges", () => {
    const state = reduceAll([
      { kind: "lifecycle", state: "connected" },
      { kind: "mode", mode: "voice", model: "m" },
      { kind: "audio_state", state: "speaking", detail: "" },
      { kind: "mic_state", state: "listening", detail: "" },
      { kind: "mode", mode: "text", model: "m" },
    ]);
    expect(state.mode).toBe("text");
    expect(state.mic).toBe("idle");
    expect(state.audio).toBe("unknown");
  });

  it("walks thinking → tool → writing → idle, and a failure ends the wait", () => {
    let state = reduceAll([
      { kind: "lifecycle", state: "connected" },
      { kind: "activity", state: "thinking", detail: "" },
    ]);
    expect(state.activity.state).toBe("thinking");
    state = reduceVoiceSession(state, {
      kind: "activity",
      state: "tool",
      detail: "jump_to_page",
    });
    expect(activityLabelKey(state.activity)).toBe("voice.activity.page");
    state = reduceVoiceSession(state, {
      kind: "transcript",
      role: "assistant",
      text: "Here is the figure.",
    });
    expect(state.activity.state).toBe("writing");
    expect(
      activityLabelKey(state.activity),
      "the reply is the progress",
    ).toBeNull();
    state = reduceVoiceSession(state, {
      kind: "activity",
      state: "thinking",
      detail: "",
    });
    state = reduceVoiceSession(state, {
      kind: "failed",
      code: "provider",
      message: "503",
    });
    expect(state.activity.state).toBe("idle");
  });

  it("names each tool step in words, never a tool id", () => {
    const label = (detail: string) =>
      activityLabelKey({ ...IDLE_ACTIVITY, state: "tool", detail, seq: 1 });
    expect(label("look_at_page")).toBe("voice.activity.look");
    expect(label("get_context_around_position")).toBe("voice.activity.read");
    expect(label("update_learning_profile")).toBe("voice.activity.profile");
    expect(label("some_new_tool")).toBe("voice.activity.tool");
    expect(
      activityLabelKey({
        ...IDLE_ACTIVITY,
        state: "fallback",
        detail: "x",
        seq: 1,
      }),
      "no model name reaches the user (ruling #50)",
    ).toBe("voice.activity.fallback");
  });

  it("names the page or chapter a step acts on, and only when the runtime said", () => {
    const at = (detail: string, target: string) =>
      activityMessage({
        ...IDLE_ACTIVITY,
        state: "tool",
        detail,
        target,
        seq: 1,
      });
    expect(at("jump_to_page", "23")).toEqual({
      key: "voice.activity.pageTo",
      params: { page: 23 },
    });
    expect(at("look_at_page", "23")).toEqual({
      key: "voice.activity.lookAt",
      params: { page: 23 },
    });
    expect(at("jump_to_chapter", "Architectures")).toEqual({
      key: "voice.activity.chapterTo",
      params: { title: "Architectures" },
    });
    expect(at("jump_to_page", ""), "no target, no claim").toEqual({
      key: "voice.activity.page",
    });
    expect(at("jump_to_page", "23abc")).toEqual({ key: "voice.activity.page" });
    expect(
      activityMessage({
        ...IDLE_ACTIVITY,
        state: "thinking",
        detail: "after_tool",
        seq: 1,
      }),
    ).toEqual({ key: "voice.activity.prepare" });
    expect(
      activityMessage({
        ...IDLE_ACTIVITY,
        state: "retrying",
        detail: "2",
        seq: 1,
      }),
    ).toEqual({ key: "voice.activity.retryingN", params: { attempt: 2 } });
    expect(
      activityMessage({
        ...IDLE_ACTIVITY,
        state: "connecting",
        detail: "typing",
        seq: 1,
      }),
    ).toEqual({ key: "voice.activity.typingBack" });
  });

  it("keeps each reply's real steps with it, and marks a failed one", () => {
    let state = reduceAll([
      { kind: "lifecycle", state: "connected" },
      {
        kind: "transcript",
        role: "user",
        text: "show me the figure",
        typed: true,
      },
      { kind: "activity", state: "thinking", detail: "" },
      { kind: "activity", state: "tool", detail: "jump_to_page", target: "23" },
      { kind: "activity", state: "thinking", detail: "after_tool" },
      { kind: "activity", state: "tool", detail: "look_at_page", target: "23" },
      { kind: "activity", state: "thinking", detail: "after_tool" },
      { kind: "transcript", role: "assistant", text: "The figure shows" },
      { kind: "transcript", role: "assistant", text: " three layers." },
    ]);
    const reply = state.transcript[state.transcript.length - 1];
    expect(reply.text).toBe("The figure shows three layers.");
    expect(reply.steps).toEqual([
      { tool: "jump_to_page", target: "23", failed: false },
      { tool: "look_at_page", target: "23", failed: false },
    ]);
    expect(state.activity.steps, "the next reply starts fresh").toEqual([]);
    expect(stepMessage(reply.steps![0])).toEqual({
      key: "voice.step.page",
      params: { page: 23 },
    });

    state = reduceAll(
      [
        {
          kind: "transcript",
          role: "user",
          text: "go to page 999",
          typed: true,
        },
        {
          kind: "activity",
          state: "tool",
          detail: "jump_to_page",
          target: "999",
        },
        {
          kind: "failed",
          code: "navigation_unresolved_page",
          message: "jump_to_page did not move the book",
        },
        {
          kind: "transcript",
          role: "assistant",
          text: "That page is not in the book.",
        },
      ],
      state,
    );
    expect(state.transcript[state.transcript.length - 1].steps).toEqual([
      { tool: "jump_to_page", target: "999", failed: true },
    ]);
  });

  it("an unknown activity state changes nothing", () => {
    const state = reduceAll([{ kind: "lifecycle", state: "connected" }]);
    expect(
      reduceVoiceSession(state, {
        kind: "activity",
        state: "dancing",
        detail: "",
      }),
    ).toBe(state);
  });

  it("a dead mic that kept the session on typing reads as a catalog line", () => {
    const state = reduceAll([
      { kind: "lifecycle", state: "connected" },
      {
        kind: "failed",
        code: "mic_unavailable_typing",
        message:
          "The microphone could not open (no device); staying in typing mode.",
      },
    ]);
    expect(state.noticeKey).toBe("voice.notice.micTyping");
    expect(state.notice).toBeNull();
  });

  it("a failed reply on the typing tutor reads as a catalog line and ends the wait", () => {
    const state = reduceAll([
      { kind: "lifecycle", state: "connected" },
      { kind: "activity", state: "tool", detail: "jump_to_page" },
      {
        kind: "failed",
        code: "reply_failed",
        message: "The tutor's reply failed: refused",
      },
    ]);
    expect(state.noticeKey).toBe("voice.notice.replyFailed");
    expect(state.activity.state).toBe("idle");
  });

  it("a mode switch seals the reply it cut, so the next tutor opens a new bubble", () => {
    const state = reduceAll([
      { kind: "lifecycle", state: "connected" },
      {
        kind: "transcript",
        role: "assistant",
        text: "The text tutor was saying",
      },
      { kind: "mode", mode: "voice", model: "m" },
      { kind: "transcript", role: "assistant", text: "Hi, I can hear you." },
    ]);
    const replies = state.transcript.filter(
      (entry) => entry.role === "assistant",
    );
    expect(replies.map((entry) => entry.text)).toEqual([
      "The text tutor was saying",
      "Hi, I can hear you.",
    ]);
  });
});

describe("voiceprints", () => {
  const BARS = Array.from({ length: VOICEPRINT_BARS }, (_, at) => at * 12);

  it("parses only a well-formed twenty-bar payload", () => {
    expect(voiceSessionEventFrom({ kind: "voiceprint", bars: BARS })).toEqual({
      kind: "voiceprint",
      bars: BARS,
    });
    expect(
      voiceSessionEventFrom({ kind: "voiceprint", bars: BARS.slice(1) }),
    ).toBeNull();
    expect(
      voiceSessionEventFrom({
        kind: "voiceprint",
        bars: [...BARS.slice(1), 256],
      }),
    ).toBeNull();
    expect(
      voiceSessionEventFrom({
        kind: "voiceprint",
        bars: [...BARS.slice(1), 1.5],
      }),
    ).toBeNull();
    expect(voiceSessionEventFrom({ kind: "voiceprint" })).toBeNull();
  });

  it("lands on the last spoken user row, never on a typed one", () => {
    const spoken = reduceAll([
      { kind: "transcript", role: "user", text: "what is it", spanFinal: true },
      { kind: "voiceprint", bars: BARS },
    ]);
    expect(spoken.transcript[0].voiceprint).toEqual(BARS);

    const typed = reduceAll([
      { kind: "transcript", role: "user", text: "typed", typed: true },
      { kind: "voiceprint", bars: BARS },
    ]);
    expect(typed.transcript[0].voiceprint).toBeUndefined();

    const none = reduceAll([{ kind: "voiceprint", bars: BARS }]);
    expect(none.transcript).toEqual([]);
  });

  it("folds a joined second span into one twenty-bar print", () => {
    const loud = Array.from({ length: VOICEPRINT_BARS }, () => 255);
    const quiet = Array.from({ length: VOICEPRINT_BARS }, () => 40);
    const joined = reduceAll([
      { kind: "transcript", role: "user", text: "first half", spanFinal: true },
      { kind: "voiceprint", bars: loud },
      {
        kind: "transcript",
        role: "user",
        text: "second half",
        spanFinal: true,
      },
      { kind: "voiceprint", bars: quiet },
    ]);
    expect(joined.transcript).toHaveLength(1);
    expect(joined.transcript[0].voiceprint).toEqual([
      ...Array.from({ length: VOICEPRINT_BARS / 2 }, () => 255),
      ...Array.from({ length: VOICEPRINT_BARS / 2 }, () => 40),
    ]);
  });

  it("keeps the print when the late final corrects the row's text", () => {
    const corrected = reduceAll([
      { kind: "transcript", role: "user", text: "draft", spanFinal: true },
      { kind: "voiceprint", bars: BARS },
      { kind: "transcript", role: "user", text: "final", correctsLast: true },
    ]);
    expect(corrected.transcript[0]).toMatchObject({
      text: "final",
      voiceprint: BARS,
    });
  });

  it("accepts a stored print from history and rejects a malformed one", () => {
    expect(voiceprintFrom(BARS)).toEqual(BARS);
    expect(voiceprintFrom("00ff")).toBeNull();
    expect(voiceprintFrom([...BARS, 1])).toBeNull();
    expect(voiceprintFrom(null)).toBeNull();
  });
});

describe("pagesReadIn", () => {
  const step = (tool: string, target: string, failed = false) => ({
    tool,
    target,
    failed,
  });

  it("keeps the pages the tutor really turned to or looked at, once each, in order", () => {
    expect(
      pagesReadIn([
        step("jump_to_page", "23"),
        step("get_context_around_position", ""),
        step("look_at_page", "23"),
        step("look_at_page", "24"),
        step("jump_to_chapter", "Introduction"),
      ]),
    ).toEqual([23, 24]);
  });

  it("drops failed steps, non-page targets and impossible pages, and caps the list", () => {
    expect(
      pagesReadIn([
        step("jump_to_page", "9", true),
        step("jump_to_page", "0"),
        step("jump_to_page", "12a"),
        step("look_at_page", ""),
      ]),
    ).toEqual([]);
    expect(
      pagesReadIn(
        ["1", "2", "3", "4", "5"].map((page) => step("jump_to_page", page)),
      ),
    ).toEqual([1, 2, 3]);
    expect(pagesReadIn(undefined)).toEqual([]);
  });
});

describe("a refused page look", () => {
  const look = {
    kind: "activity",
    state: "tool",
    detail: "look_at_page",
    target: "12",
  } as const;
  const reply = {
    kind: "transcript",
    role: "assistant",
    text: "I could not see it.",
  } as const;

  it("parses only a step_failed that names its tool", () => {
    expect(
      voiceSessionEventFrom({ kind: "step_failed", tool: "look_at_page" }),
    ).toEqual({
      kind: "step_failed",
      tool: "look_at_page",
    });
    expect(voiceSessionEventFrom({ kind: "step_failed" })).toBeNull();
    expect(voiceSessionEventFrom({ kind: "step_failed", tool: 3 })).toBeNull();
  });

  it("earns no pill, keeps the quiet state and spares the earlier jump", () => {
    const state = reduceAll([
      { kind: "activity", state: "tool", detail: "jump_to_page", target: "9" },
      look,
      { kind: "step_failed", tool: "look_at_page" },
      { kind: "activity", state: "thinking", detail: "after_tool" },
      reply,
    ]);
    const steps = state.transcript[state.transcript.length - 1].steps;
    expect(steps?.map((step) => step.failed)).toEqual([false, true]);
    expect(pagesReadIn(steps)).toEqual([9]);
    expect(state.notice).toBeNull();
    expect(state.noticeKey).toBeNull();
  });

  it("marks the step on the reply when the reply already took it", () => {
    const state = reduceAll([
      look,
      reply,
      { kind: "step_failed", tool: "look_at_page" },
    ]);
    const steps = state.transcript[state.transcript.length - 1].steps;
    expect(steps?.[0].failed).toBe(true);
    expect(pagesReadIn(steps)).toEqual([]);
  });

  it("leaves the activity line where it was", () => {
    const before = reduceAll([look]);
    const after = reduceVoiceSession(before, {
      kind: "step_failed",
      tool: "look_at_page",
    });
    expect(after.activity.state).toBe(before.activity.state);
    expect(after.activity.detail).toBe(before.activity.detail);
  });
});
