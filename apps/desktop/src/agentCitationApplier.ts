// The Book Agent citation applier (charter §4.5, B-4 — closes the B-1
// applied-effect gate): the webview half that turns one `voice-agent-highlight`
// event into a visible citation and answers the Rust side with what actually
// happened. The tool ack the model hears is derived from THIS answer, never
// from event emission.
//
// Deliberately parallel to the teacher spotlight plumbing but a separate
// visual channel (T3): citation geometry comes from the cited segment's own
// rects, never from karaoke/word-timing state, and playback movement cannot
// move or clear it.

import { invoke } from "@tauri-apps/api/core";

import type { AgentCitation } from "./agentHighlightModel";
import type {
  ReadingChapter,
  ReadingPage,
  ReadingSegment,
} from "./readingModel";
import {
  projectRect,
  segmentFallbackRects,
  unionOf,
  type OverlayBox,
} from "./readingPageModel";

export const AGENT_HIGHLIGHT_EVENT = "voice-agent-highlight";
export const AGENT_HIGHLIGHT_CLEAR_EVENT = "voice-agent-highlight-clear";

export interface AgentCitationPlan {
  box: OverlayBox;
  pageNo: number;
  /** Accessible status the reading surface exposes to AT. */
  /** 0-based chapter index; the overlay renders the accessible label from
   * pageNo + chapter (I18N-2: no t() in this render-path builder). */
  chapter: number;
}

/**
 * The cited segment within the CURRENTLY loaded chapter, or null. The reading
 * view loads one chapter at a time; `ReadingSegment.idx` is the job-global
 * index the citation carries. A citation into an unloaded chapter resolves
 * null and acks honestly — the §5.2 protocol navigates (go_to_passage, which
 * loads the chapter) before it highlights, so the applied path is the loaded
 * one. Recorded bound, not an accident.
 */
export function citedSegment(
  chapter: ReadingChapter | null,
  citation: AgentCitation,
): ReadingSegment | null {
  const segment = chapter?.segments.find((s) => s.idx === citation.segmentIdx);
  if (segment == null) return null;
  return segment.pageNo === citation.page ? segment : null;
}

/**
 * The visible citation plan for the shown page, or null (wrong page, no
 * geometry). A null plan on the cited page is an HONEST non-application —
 * the ack must say so rather than pretending a highlight rendered.
 */
export function agentCitationPlan(
  citation: AgentCitation | null,
  chapter: ReadingChapter | null,
  shownPageNo: number | null,
  page: ReadingPage | null,
): AgentCitationPlan | null {
  if (citation == null || shownPageNo == null || page == null) return null;
  if (citation.page !== shownPageNo || page.pageNo !== shownPageNo) return null;
  const segment = citedSegment(chapter, citation);
  if (segment == null) return null;
  const envelope = unionOf(segmentFallbackRects(segment, page));
  if (envelope == null) return null;
  const box = projectRect(envelope, page);
  if (box == null) return null;
  return {
    box,
    pageNo: citation.page,
    chapter: citation.chapter,
  };
}

/**
 * Answer the Rust highlight port (the applied-effect round trip). Fire-and-
 * forget from the UI's perspective; the Rust side owns the timeout honesty.
 */
export function ackCitation(
  callId: string | null,
  applied: boolean,
  reason: string | null,
): void {
  if (callId == null) return;
  void invoke("agent_highlight_ack", { callId, applied, reason }).catch(() => {
    // A failed ack invoke degrades to the Rust-side timeout answer.
  });
}

/**
 * The post-render ack resolution (Hermes finale item 2): the applied verdict
 * comes from the SAME plan the overlay renders — a non-null, finite plan on
 * the committed citation is Applied; terminal conditions the plan inputs
 * prove (script mode, paused following, geometry failure on the cited page)
 * answer honestly now; anything still in flight (page data loading, page
 * turn pending) stays pending and the Rust-side deadline owns the timeout.
 */
export function resolveAck(
  plan: AgentCitationPlan | null,
  pageMode: boolean,
  paused: boolean,
  shownPageNo: number | null,
  citedPage: number,
  pageLoaded: boolean,
): { done: boolean; applied: boolean; reason: string | null } {
  if (plan != null) {
    const finite = [
      plan.box.leftPct,
      plan.box.topPct,
      plan.box.widthPct,
      plan.box.heightPct,
    ].every((n) => Number.isFinite(n));
    if (finite) return { done: true, applied: true, reason: null };
    return { done: true, applied: false, reason: "invalid_geometry" };
  }
  if (!pageMode) return { done: true, applied: false, reason: "script_mode" };
  if (paused) return { done: true, applied: false, reason: "following_paused" };
  if (shownPageNo === citedPage && pageLoaded) {
    return { done: true, applied: false, reason: "no_geometry" };
  }
  // Page turn / page data still in flight: not resolvable yet.
  return { done: false, applied: false, reason: null };
}
