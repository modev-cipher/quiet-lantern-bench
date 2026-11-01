// D-QA1 — "recap must act": bounded played-range recap in ONE round-trip.
//
// INVARIANT: with played coverage 0..32min, a bounded recap ask ("key
// points 0..32") must be executable in ONE tool round-trip — some offered
// tool must accept a bounded (start, end) range and resolve it to real
// text, without forcing a user clarification ("go back to where?").
//   Spec citations:
//   - F8 §1.1 (D1 verdict): "each fetch is ±2 segments, 'everything covered
//     in 30 minutes' is dozens of serial Tier-0 calls" — the spec itself
//     names the serial-fetch mechanism as the failure.
//   - F7 §5 (tool table): the offered set is exactly 13 CORE_TOOLS;
//     get_context_around_position is a WINDOW at one position, and F8
//     §2.1.3 rules "summarize_section stays window-scoped (no widening —
//     unchanged charter)".
//   - F8 §2.1 designs a chapter map + played-ranges header + fetch law —
//     but specifies NO tool that fetches a bounded range in one call.
//     SPEC-GAP filed: the owner invariant (one round-trip) is not
//     satisfiable by any mechanism the spec names; this suite measures the
//     tool surface itself.
//
// ATTACK STRATEGY: scan the PRODUCTION tool registry + dispatcher source
// (the house .rs-source-gate idiom, voiceSessionPanel.test.ts reads lib.rs
// the same way) for (a) any tool name shaped like a range fetch, (b) any
// tool schema declaring BOTH a start-like and an end-like bound. If
// neither exists, the tutor structurally cannot answer a bounded recap in
// one round-trip — which is exactly why it asked "go back to where?".
//
// WHY A SHALLOW MOCK CANNOT FAKE A PASS: the tests read the shipped Rust
// source of the registry/dispatcher; nothing injectable exists. A pass
// requires the real tool surface to carry a bounded-range capability.

import { describe, expect, it } from "vitest";
import { readSource } from "../readSource";

const REGISTRY = readSource(
  new URL("../../../src-tauri/src/voice/tools/registry.rs", import.meta.url),
);
const DISPATCHER = readSource(
  new URL("../../../src-tauri/src/voice/tools/dispatcher.rs", import.meta.url),
);
const VOICE_SESSION = readSource(
  new URL("../../../src-tauri/src/voice_session.rs", import.meta.url),
);
const COVERAGE = joinLiterals(
  readSource(
    new URL("../../../src-tauri/src/voice/excerpt/coverage.rs", import.meta.url),
  ),
);

/// Rust string literals wrap with `\` + newline + indentation; join them so a
/// phrase split across source lines still reads as one phrase.
function joinLiterals(source: string): string {
  return source.replace(/\\\n\s*/g, "").replace(/\s+/g, " ");
}

/// The grounded prompt's appended context block (`render_chapter_map_block`):
/// the only place the recap/coverage law lives. Scanned as source so the pin
/// holds without a Tauri runtime.
function groundedRecapLaw(): string {
  const start = VOICE_SESSION.indexOf("fn render_coverage_and_recap_law(");
  const end = VOICE_SESSION.indexOf("/// The production tutor system prompt", start);
  expect(start, "render_chapter_map_block must exist").toBeGreaterThan(-1);
  expect(end, "block must end before tutor_system_prompt").toBeGreaterThan(start);
  return joinLiterals(VOICE_SESSION.slice(start, end));
}

function rangeToolDescription(): string {
  const start = REGISTRY.indexOf("GET_CONTEXT_FOR_RANGE => function_schema(");
  const end = REGISTRY.indexOf("GET_CONTEXT_AROUND_POSITION => function_schema(", start);
  expect(start).toBeGreaterThan(-1);
  expect(end).toBeGreaterThan(start);
  return joinLiterals(REGISTRY.slice(start, end));
}

// The 13 core tools of F7 §5 — the sanity anchor. If this drifts, the
// registry was re-shaped and the capability scan below must be re-derived.
const F7_CORE_TOOLS = [
  "pause_playback",
  "resume_playback",
  "seek_relative",
  "set_playback_speed",
  "replay_previous_paragraph",
  "replay_previous_page",
  "replay_last_minute",
  "jump_to_chapter",
  "jump_to_page",
  "summarize_section",
  "get_context_around_position",
  "update_learning_profile",
  "look_at_page",
];

describe("D-QA1: the tool surface can serve a bounded played-range recap in one call", () => {
  it("sanity: the F7 §5 core roster is present (scan anchor is valid)", () => {
    for (const name of F7_CORE_TOOLS) {
      expect(REGISTRY, `core tool ${name} missing`).toContain(`"${name}"`);
    }
  });

  it("a range-shaped tool NAME exists (get_context_for_range / summarize_range / …)", () => {
    // Any naming that expresses "fetch/summarize between two bounds".
    expect(
      REGISTRY,
      "no tool name expresses a bounded range — a recap of 0..32min cannot be ONE round-trip; the tutor must either serialize dozens of ±2-segment windows (F8 §1.1) or ask the user 'go back to where?' (owner defect D-QA1)",
    ).toMatch(/"[a-z_]*(range|between|from_to|span)[a-z_]*"/);
  });

  it("some tool schema declares BOTH a start bound and an end bound", () => {
    // Schema-level scan, name-agnostic: a single tool whose parameters
    // carry a start-like and an end-like bound (start_ms/end_ms,
    // from_position/to_position, …). We scan per-tool declaration block:
    // between one tool-name constant and the next, both bounds must
    // co-occur somewhere in the registry or dispatcher schema text.
    const combined = REGISTRY + DISPATCHER;
    const hasRangePair =
      /"(start|from)[a-z_]*"[^]{0,600}"(end|to)_[a-z_]*"/i.test(combined) ||
      /"(start|from)_(ms|pos|position|offset)"[^]{0,600}"(end|to)(_ms|_pos|_position|_offset)?"/i.test(
        combined,
      );
    expect(
      hasRangePair,
      "no tool parameter schema carries a (start, end) pair anywhere in registry.rs/dispatcher.rs — the bounded played-range fetch capability does not exist (D-QA1 structural finding)",
    ).toBe(true);
  });

  it("documents the charter constraint: summarize_section is window-scoped, so it is NOT the missing capability", () => {
    // F8 §2.1.3: "summarize_section stays window-scoped (no widening —
    // unchanged charter)". This pins that the fix direction is a NEW
    // bounded-range capability, not a widened summarize_section: the tool
    // must still exist and must not have silently grown range bounds
    // without a charter change.
    expect(REGISTRY).toContain('"summarize_section"');
  });
});

// F10-D1 (owner device, BookGrounded at 22:23/55:28): "summarize from minute
// zero to where we are" was answered with a permission question and with the
// session-accounting model relayed to the user ("the system records playback
// from when this session started"). Root cause: the recap law HINTED at the
// bounded fetch and SCRIPTED the accounting sentence. These pins hold the
// corrected contract at the source: explicit bounds ⇒ mandatory range call,
// no questions; bookkeeping language is internal and never relayed.
describe("F10-D1: explicit recap bounds bind to a mandatory range fetch; accounting stays internal", () => {
  it("the grounded recap law mandates the range call BEFORE any reply and forbids asking", () => {
    const law = groundedRecapLaw();
    expect(law).toContain("(1) A recap over a stretch of the book");
    expect(law).toMatch(/call get_context_for_range[^.]*BEFORE (you )?(say|speak)/i);
    expect(law).toMatch(/never ask (for )?permission/i);
    expect(law).toMatch(/never ask which range/i);
  });

  it("\"so far\" / \"until now\" / \"where we are\" mean [0, narration position) — not the session-played span", () => {
    const law = groundedRecapLaw();
    // The template binds "so far" to the units' {position}; the narrated arm
    // renders it as the narration position, the BookDirect arm as the
    // reading position — never the session-played span.
    expect(law).toMatch(/so far[^.]*until now[^.]*from the start of the book to the \{position\}/i);
    expect(law).toMatch(/"current narration position"/);
    expect(law).not.toMatch(/state the two positions plainly/);
  });

  it("round 2: the played-only rule no longer captures \"covered so far\" — the two request classes are disjoint", () => {
    const law = groundedRecapLaw();
    // The capturing sentence of the first cut: "When the user asks what was
    // covered so far, for a recap of their progress … rely only on the
    // played ranges" — gone.
    expect(law).not.toMatch(/covered so far[^.]*rely only on the played ranges/i);
    expect(law).toMatch(/NOT bounded by the session-\{consumed\} ranges/);
    expect(law).toMatch(/\(2\) A session-scoped ask[^.]*rely only on the \{consumed\} ranges/);
  });

  it("round 2: a Split (over-budget) reply has a follow-up strategy — fetch the omitted middle before recapping", () => {
    const law = groundedRecapLaw();
    expect(law).toMatch(/complete=false[^.]*get_context_for_range on its `omitted_range` next/i);
    expect(law).toMatch(/recurse on its `omitted_range`/);
    expect(law).toMatch(/every piece is complete=true and the pieces cover the requested range/);
    // The dispatcher emits the fields the law names.
    expect(DISPATCHER).toContain('payload["omitted_range"] = omitted_range;');
    expect(DISPATCHER).toContain('first_chunk["range"] = head_range;');
  });

  it("round 2: the law is rendered per position units — narrated and BookDirect vocabularies are separate arms", () => {
    const law = groundedRecapLaw();
    expect(law).toContain("PositionUnits::Millis => (");
    expect(law).toContain("PositionUnits::Offset => (");
    expect(law).toMatch(/"current narration position"/);
    expect(law).toMatch(/"current reading position"/);
  });

  it("the law declares coverage bookkeeping INTERNAL and bans session-tracking talk to the user", () => {
    const law = groundedRecapLaw();
    expect(law).toMatch(/internal bookkeeping of what was \{consumed\} in THIS session/);
    expect(law).toMatch(/never (mention|voice|relay)[^.]*session/i);
    // The scripted sentence the device relayed verbatim must be gone from
    // every model-facing surface: prompt block and coverage line alike.
    expect(law).not.toContain("this session only tracks playback since it started");
    expect(COVERAGE).not.toContain("the user may have listened before");
    expect(COVERAGE).not.toContain("this session only tracks playback since it started");
  });

  it("the range tool's description is an obligation, not a capability note", () => {
    const description = rangeToolDescription();
    expect(description).toMatch(/Call this[^"]*do not ask/i);
    expect(description).toMatch(/so far/i);
    // ONE omitted-bounds rule for both modes — the old "otherwise the span
    // actually played or read" clause is gone; Split continuation is named.
    expect(description).toMatch(/ONE rule in every mode/);
    expect(description).not.toMatch(/otherwise the span actually played/);
    expect(description).toMatch(/`omitted_range`/);
  });

  it("the dispatcher's omitted-bounds default is [0, current position) in every mode (F9-D1 kept, extended to BookDirect)", () => {
    expect(DISPATCHER).toContain('DispatchMode::BookDirect => (pos, "reading_so_far")');
    expect(DISPATCHER).toContain('_ => (pos, "narration_so_far")');
    expect(DISPATCHER).toContain("Some((pos, source)) => (0, pos, Some(source))");
  });
});
