// D-QA3 — dead air: a provider tool-call turn must never yield ZERO events.
//
// INVARIANT: a provider TOOL CALL turn always ends in a rendered assistant
// response OR a typed failure UI event — never silence on screen.
//   Spec citations:
//   - F8 §1.4 (D4 verdict): "A hallucinated name … gets a typed
//     `unknown_tool` refusal to the provider ONLY … the user sees nothing"
//     — the diagnosed silent branch.
//   - F8 §2.4.3 (remediation): "the unknown-tool refusal is now also
//     surfaced as a `Failed { code: "unknown_tool" }` UI event so a
//     bound-to-nothing request is never silent on screen."
//   - F8 §1.4: failed jumps emit `navigation_<code>` UI events (the
//     2026-08-31 D3 fix) — the tool-EXECUTION-error half must render too.
//   - SPEC-GAP (filed): no spec clause guarantees a UI event for a tool
//     SUCCESS whose turn produces no spoken reply (success-with-payload,
//     zero assistant text). F8 P6 covers navigation successes only. That
//     half of the owner defect cannot be asserted against any spec text,
//     and its emission site is the Rust pump — beyond this fence.
//
// ATTACK STRATEGY: two layers. (1) Behavioral: the production reducer must
// RENDER the typed failure shapes the spec promises — a Failed event with
// an unknown_tool / navigation_<code> code must land as a visible notice
// while the session stays live, and must not be scrubbed by later
// non-lifecycle traffic. (2) Structural (.rs source gate, the house
// lib.rs-scan idiom): the shipped Rust must actually emit the F8 §2.4.3
// `unknown_tool` UI event — if the refusal is still provider-only, the
// screen provably stays still and the gate fails.
//
// WHY A SHALLOW MOCK CANNOT FAKE A PASS: the reducer tests run the shipped
// reduceVoiceSession with zero doubles; the source gates read the shipped
// voice_session.rs/dispatcher.rs text — only a real emission site
// satisfies them.

import { describe, expect, it } from "vitest";

import {
  INITIAL_VOICE_SESSION,
  reduceVoiceSession,
  voiceSessionEventFrom,
  type VoiceSessionEvent,
  type VoiceSessionState,
} from "../../voiceSessionModel";
import { readSource } from "../readSource";

const VOICE_SESSION_RS = readSource(
  new URL("../../../src-tauri/src/voice_session.rs", import.meta.url),
);
const DISPATCHER_RS = readSource(
  new URL("../../../src-tauri/src/voice/tools/dispatcher.rs", import.meta.url),
);

function reduceAll(events: VoiceSessionEvent[]): VoiceSessionState {
  return events.reduce(reduceVoiceSession, INITIAL_VOICE_SESSION);
}

describe("D-QA3 behavioral: typed tool failures RENDER (no dead air at the reducer)", () => {
  it("a hallucinated tool name lands on screen: Failed{unknown_tool} is a visible notice on a live session", () => {
    const state = reduceAll([
      { kind: "lifecycle", state: "connected" },
      {
        kind: "failed",
        code: "unknown_tool",
        message: "the tutor tried a tool that does not exist",
      },
    ]);
    expect(state.status).toBe("live");
    expect(state.notice).toContain("unknown_tool");
  });

  it("a tool execution error lands on screen: navigation_<code> renders and survives later traffic", () => {
    const state = reduceAll([
      { kind: "lifecycle", state: "connected" },
      {
        kind: "failed",
        code: "navigation_unknown_chapter",
        message: "that chapter does not exist in this book",
      },
      // Later non-lifecycle traffic must NOT scrub the failure (only an
      // explicit lifecycle recovery may — pinned contract).
      { kind: "transcript", role: "assistant", text: "…" },
      { kind: "audio_state", state: "speaking", detail: "" },
    ]);
    expect(state.status).toBe("live");
    expect(state.notice).toContain("navigation_unknown_chapter");
  });

  it("the wire parse never silently swallows a failed event with unusual codes", () => {
    for (const code of [
      "unknown_tool",
      "tool_error",
      "navigation_no_context",
    ]) {
      const parsed = voiceSessionEventFrom({
        kind: "failed",
        code,
        message: "m",
      });
      expect(parsed, `Failed{${code}} was refused by the parse`).toEqual({
        kind: "failed",
        code,
        message: "m",
      });
    }
  });
});

describe("D-QA3 structural: the Rust side emits the promised UI events (source gates)", () => {
  it("F8 §2.4.3: unknown_tool is surfaced as a UI event, not a provider-only refusal", () => {
    // The dispatcher types the refusal ("unknown_tool" exists there — F8
    // §1.4 cites dispatcher.rs:75,131). The remediation requires the
    // SESSION layer to ALSO emit it to the webview. We demand the code
    // appears in voice_session.rs in a UI-event emission context.
    expect(
      DISPATCHER_RS,
      "the typed unknown_tool refusal itself vanished — re-derive this gate",
    ).toContain("unknown_tool");
    const uiSites = [...VOICE_SESSION_RS.matchAll(/unknown_tool/g)];
    expect(
      uiSites.length,
      "voice_session.rs never mentions unknown_tool: the refusal is provider-only, the user sees NOTHING when the model hallucinates a tool (owner defect D-QA3 / F8 §2.4.3 unimplemented)",
    ).toBeGreaterThan(0);
    // And at least one mention sits in an emission context (Failed /
    // SessionUiEvent / emit) rather than a comment.
    const nearEmission = uiSites.some((match) => {
      const at = match.index ?? 0;
      const region = VOICE_SESSION_RS.slice(Math.max(0, at - 600), at + 600);
      return /SessionUiEvent|Failed|emit/i.test(region);
    });
    expect(
      nearEmission,
      "unknown_tool appears in voice_session.rs but never near a UI emission (SessionUiEvent/Failed/emit) — the refusal still does not reach the screen",
    ).toBe(true);
  });

  it("tool failures reach the UI as navigation_<code> events (2026-08-31 D3 fix present)", () => {
    expect(
      /navigation_/.test(VOICE_SESSION_RS) || /navigation_/.test(DISPATCHER_RS),
      "no navigation_<code> UI failure emission found — failed tool executions are silent",
    ).toBe(true);
  });
});
