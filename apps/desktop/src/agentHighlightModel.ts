// Book Agent citation model (charter §4.5, B-1): the webview half of the
// `voice-agent-highlight` contract. Deliberately parallel to
// `teacherAnchorModel.ts` — and deliberately NOT it: an Agent citation is
// playhead-decoupled evidence, not narration sync.
//
// Laws this model owns:
// - Ordering: events cross the Tauri boundary asynchronously; a lower
//   generation arriving after a higher one is a superseded response's
//   citation and never moves the page (monotonic gen defense).
// - One active citation at a time (charter A8): a new citation replaces the
//   previous; retained until the next citation, explicit dismiss, session
//   end, or view teardown. Playback movement alone NEVER clears or moves it —
//   nothing in this file reads the playback clock, word timings, or karaoke
//   state (T3), and the vitest structure pin enforces that absence.
// - Manual scroll pauses citation following ("Following: off — resume", the
//   established pattern): citations arriving while paused update `latest`
//   but never move the page; resume jumps to the latest citation. The Agent
//   may not fight the user's view.
// - No citation → this model contributes nothing to the shown page.
//
// The payload carries validated navigation ids only (chapter/page/segment,
// optional word span, generation) — never passage text (§4.5).

export interface AgentCitation {
  chapter: number;
  page: number;
  segmentIdx: number;
  startWord: number | null;
  wordCount: number | null;
  gen: number;
  /** Applied-effect ack correlation (B-4); null on synthetic citations. */
  ackCallId: string | null;
}

export interface AgentCitationState {
  /** The newest live citation, or null (nothing active). */
  latest: AgentCitation | null;
  /** The citation the view shows — freezes while following is paused. */
  shown: AgentCitation | null;
  /** A manual scroll turned following off; resume re-syncs to `latest`. */
  paused: boolean;
  /** Bumps exactly when the view should scroll to `shown` (change-only). */
  syncNonce: number;
  /**
   * Highest generation ever seen this session. Survives dismiss on purpose:
   * a dismissed citation must not resurrect when a stale (lower-gen) event
   * crosses the Tauri boundary late.
   */
  highWaterGen: number;
}

export const INITIAL_AGENT_CITATION: AgentCitationState = {
  latest: null,
  shown: null,
  paused: false,
  syncNonce: 0,
  highWaterGen: -1,
};

export type AgentCitationAction =
  | { type: "citation"; citation: AgentCitation }
  | { type: "manualScroll" }
  | { type: "resume" }
  | { type: "dismiss" }
  | { type: "sessionEnd" };

/** Parses the Tauri event payload; anything out of contract is ignored. */
export function agentCitationFrom(payload: unknown): AgentCitation | null {
  if (typeof payload !== "object" || payload == null) return null;
  const { chapter, page, segmentIdx, startWord, wordCount, gen, ackCallId } =
    payload as Record<string, unknown>;
  const nat = (v: unknown): v is number =>
    typeof v === "number" && Number.isInteger(v) && v >= 0;
  if (!nat(chapter) || !nat(page) || !nat(segmentIdx) || !nat(gen)) return null;
  // Both-or-neither, mirroring the backend contract: a half-formed or broken
  // span rejects the event rather than silently widening the citation to the
  // whole passage.
  const callId =
    typeof ackCallId === "string" && ackCallId.length > 0 ? ackCallId : null;
  if (startWord === undefined && wordCount === undefined) {
    return {
      chapter,
      page,
      segmentIdx,
      gen,
      startWord: null,
      wordCount: null,
      ackCallId: callId,
    };
  }
  if (!nat(startWord) || !nat(wordCount) || wordCount < 1) return null;
  return {
    chapter,
    page,
    segmentIdx,
    gen,
    startWord,
    wordCount,
    ackCallId: callId,
  };
}

function sameTarget(a: AgentCitation | null, b: AgentCitation): boolean {
  return (
    a != null &&
    a.chapter === b.chapter &&
    a.page === b.page &&
    a.segmentIdx === b.segmentIdx &&
    a.startWord === b.startWord &&
    a.wordCount === b.wordCount
  );
}

export function agentCitationReducer(
  state: AgentCitationState,
  action: AgentCitationAction,
): AgentCitationState {
  switch (action.type) {
    case "citation": {
      const { citation } = action;
      // Monotonic generation defense: a stale citation never moves the page
      // and never replaces a newer one — including after a dismiss (the
      // watermark outlives `latest`).
      if (citation.gen < state.highWaterGen) return state;
      const marked = { ...state, highWaterGen: citation.gen };
      if (state.paused) {
        // Following is off: remember it, move nothing.
        return { ...marked, latest: citation };
      }
      if (sameTarget(state.shown, citation)) {
        // Change-only sync: the same citation repeated re-arms nothing.
        return { ...marked, latest: citation, shown: citation };
      }
      return {
        ...marked,
        latest: citation,
        shown: citation,
        syncNonce: state.syncNonce + 1,
      };
    }
    case "manualScroll":
      // A no-op unless a citation is actually live (round-4 regression law:
      // ordinary reading gestures must never poison follow state through
      // this reducer). With one live, the user owns the view; the shown
      // citation stays visible where it is (dismissal is explicit).
      if (state.shown == null && state.latest == null) return state;
      return state.paused ? state : { ...state, paused: true };
    case "resume": {
      if (!state.paused) return state;
      const next = { ...state, paused: false };
      if (state.latest != null) {
        return {
          ...next,
          shown: state.latest,
          syncNonce: state.syncNonce + 1,
        };
      }
      return next;
    }
    case "dismiss":
      // Explicit dismiss clears the visible citation but keeps following
      // state; `latest` is gone too — a dismissed citation must not
      // resurrect on resume.
      return { ...state, latest: null, shown: null };
    case "sessionEnd":
      return INITIAL_AGENT_CITATION;
    default:
      return state;
  }
}
