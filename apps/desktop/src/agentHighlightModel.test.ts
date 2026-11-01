import { describe, expect, it } from "vitest";

import {
  agentCitationFrom,
  agentCitationReducer,
  INITIAL_AGENT_CITATION,
  type AgentCitation,
  type AgentCitationState,
} from "./agentHighlightModel";
import { readSource } from "./tests/readSource";

const citation = (
  gen: number,
  overrides: Partial<AgentCitation> = {},
): AgentCitation => ({
  chapter: 0,
  page: 12,
  segmentIdx: 40,
  startWord: null,
  wordCount: null,
  gen,
  ackCallId: null,
  ...overrides,
});

const cite = (state: AgentCitationState, c: AgentCitation) =>
  agentCitationReducer(state, { type: "citation", citation: c });

describe("payload parsing is strict and text-free", () => {
  it("accepts the id-only contract shape", () => {
    expect(
      agentCitationFrom({ chapter: 1, page: 5, segmentIdx: 7, gen: 3 }),
    ).toEqual(citation(3, { chapter: 1, page: 5, segmentIdx: 7 }));
  });

  it("accepts a valid word span and rejects a broken one", () => {
    const withSpan = agentCitationFrom({
      chapter: 0,
      page: 1,
      segmentIdx: 2,
      startWord: 4,
      wordCount: 3,
      gen: 1,
    });
    expect(withSpan?.startWord).toBe(4);
    // A half-formed or broken span rejects the whole event — never a silent
    // widening to a whole-passage citation.
    for (const span of [
      { startWord: 4, wordCount: 0 },
      { startWord: 4 },
      { wordCount: 3 },
      { startWord: -1, wordCount: 3 },
    ]) {
      expect(
        agentCitationFrom({
          chapter: 0,
          page: 1,
          segmentIdx: 2,
          gen: 1,
          ...span,
        }),
      ).toBeNull();
    }
  });

  it("rejects out-of-contract payloads", () => {
    for (const bad of [
      null,
      "text",
      { chapter: -1, page: 1, segmentIdx: 1, gen: 1 },
      { chapter: 0, page: 1, segmentIdx: 1 },
      { chapter: 0, page: 1, segmentIdx: 1, gen: 1.5 },
    ]) {
      expect(agentCitationFrom(bad)).toBeNull();
    }
  });
});

describe("one active citation, generation-ordered (charter A8, §4.5)", () => {
  it("a citation shows and bumps the sync nonce exactly once", () => {
    const s = cite(INITIAL_AGENT_CITATION, citation(1));
    expect(s.shown).toEqual(citation(1));
    expect(s.syncNonce).toBe(1);
    // The same citation repeated re-arms nothing (change-only sync).
    const again = cite(s, citation(2, {}));
    expect(again.syncNonce).toBe(1);
  });

  it("a new citation replaces the previous one", () => {
    const first = cite(INITIAL_AGENT_CITATION, citation(1));
    const second = cite(first, citation(2, { page: 30, segmentIdx: 90 }));
    expect(second.shown?.page).toBe(30);
    expect(second.syncNonce).toBe(2);
  });

  it("a stale generation arriving late never moves the page", () => {
    const newer = cite(INITIAL_AGENT_CITATION, citation(5, { page: 30 }));
    const afterStale = cite(newer, citation(3, { page: 7 }));
    expect(afterStale).toEqual(newer);
  });
});

describe("manual scroll: the agent never fights the user's view", () => {
  it("is a NO-OP with no live citation (round-4 reading-regression law)", () => {
    const s = agentCitationReducer(INITIAL_AGENT_CITATION, {
      type: "manualScroll",
    });
    expect(s).toEqual(INITIAL_AGENT_CITATION);
    // A dismissed state (no shown, no latest) is equally inert.
    let d = cite(INITIAL_AGENT_CITATION, citation(1));
    d = agentCitationReducer(d, { type: "dismiss" });
    expect(agentCitationReducer(d, { type: "manualScroll" }).paused).toBe(
      false,
    );
  });

  it("pauses following; arriving citations update latest but move nothing", () => {
    let s = cite(INITIAL_AGENT_CITATION, citation(1));
    s = agentCitationReducer(s, { type: "manualScroll" });
    const whilePaused = cite(s, citation(2, { page: 44 }));
    expect(whilePaused.shown?.page).toBe(12);
    expect(whilePaused.latest?.page).toBe(44);
    expect(whilePaused.syncNonce).toBe(s.syncNonce);
  });

  it("resume jumps to the latest citation", () => {
    let s = cite(INITIAL_AGENT_CITATION, citation(1));
    s = agentCitationReducer(s, { type: "manualScroll" });
    s = cite(s, citation(2, { page: 44 }));
    const resumed = agentCitationReducer(s, { type: "resume" });
    expect(resumed.paused).toBe(false);
    expect(resumed.shown?.page).toBe(44);
    expect(resumed.syncNonce).toBe(s.syncNonce + 1);
  });
});

describe("lifecycle honesty", () => {
  it("resume re-applies a same-target citation after the view moved (UX-1.3)", () => {
    const s = agentCitationReducer(cite(INITIAL_AGENT_CITATION, citation(0)), {
      type: "manualScroll",
    });
    const repeated = cite(s, citation(1));
    expect(repeated.paused).toBe(true);
    expect(repeated.syncNonce).toBe(s.syncNonce);
    const resumed = agentCitationReducer(repeated, { type: "resume" });
    expect(resumed.paused).toBe(false);
    expect(resumed.shown).toEqual(citation(1));
    expect(resumed.syncNonce).toBe(s.syncNonce + 1);
  });

  it("resume with nothing newer still re-applies the shown citation", () => {
    const s = agentCitationReducer(cite(INITIAL_AGENT_CITATION, citation(0)), {
      type: "manualScroll",
    });
    const resumed = agentCitationReducer(s, { type: "resume" });
    expect(resumed.shown).toEqual(citation(0));
    expect(resumed.syncNonce).toBe(s.syncNonce + 1);
  });

  it("dismiss clears the citation and it does not resurrect on resume", () => {
    let s = cite(INITIAL_AGENT_CITATION, citation(1));
    s = agentCitationReducer(s, { type: "manualScroll" });
    s = agentCitationReducer(s, { type: "dismiss" });
    const resumed = agentCitationReducer(s, { type: "resume" });
    expect(resumed.shown).toBeNull();
    expect(resumed.latest).toBeNull();
  });

  it("a stale generation cannot resurrect a dismissed citation", () => {
    let s = cite(INITIAL_AGENT_CITATION, citation(5));
    s = agentCitationReducer(s, { type: "dismiss" });
    const afterStale = cite(s, citation(3, { page: 7 }));
    expect(afterStale.shown).toBeNull();
    expect(afterStale.latest).toBeNull();
  });

  it("session end clears everything", () => {
    const s = cite(INITIAL_AGENT_CITATION, citation(4));
    expect(agentCitationReducer(s, { type: "sessionEnd" })).toEqual(
      INITIAL_AGENT_CITATION,
    );
  });
});

/*
 * T3 structure pin: the Agent citation is playhead-decoupled by construction.
 * The model file must not read the playback clock, word timestamps, or the
 * narration karaoke machinery — playback movement cannot move, recreate, or
 * clear a citation because the code has no path from playback state to
 * citation state.
 */
describe("the citation model is playhead-decoupled by construction (T3)", () => {
  it("agentHighlightModel.ts references no playback or karaoke vocabulary", () => {
    const body = readSource(
      new URL("./agentHighlightModel.ts", import.meta.url),
    );
    for (const forbidden of [
      /playbackStore/,
      /position_ms|positionMs|posMs/,
      /WordTimestamps|wordTimestamps/,
      /reading-highlight/,
      /from "\.\/teacherAnchorModel"/,
    ]) {
      expect(body).not.toMatch(forbidden);
    }
  });
});
