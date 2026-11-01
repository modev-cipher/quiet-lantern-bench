import { describe, expect, it } from "vitest";

import {
  resolveAck,
  agentCitationPlan,
  citedSegment,
} from "./agentCitationApplier";
import type { AgentCitation } from "./agentHighlightModel";
import type { ReadingChapter, ReadingPage } from "./readingModel";

const citation = (overrides: Partial<AgentCitation> = {}): AgentCitation => ({
  chapter: 0,
  page: 5,
  segmentIdx: 40,
  startWord: null,
  wordCount: null,
  gen: 1,
  ackCallId: "call-7",
  ...overrides,
});

const chapter = (): ReadingChapter =>
  ({
    anchorGeometry: "ensured",
    chapterId: "ch-0",
    startMs: 0,
    endMs: 10_000,
    origin: "measured",
    segments: [
      {
        segmentId: "seg-40",
        idx: 40,
        pageNo: 5,
        kind: "text",
        figureId: null,
        text: "the kamal measured polaris",
        startMs: 0,
        endMs: 900,
        provenance: "missing",
        words: [],
        anchor: "block",
        rects: [{ x: 0.1, y: 0.2, w: 0.5, h: 0.05 }],
      },
    ],
  }) as unknown as ReadingChapter;

const page = (): ReadingPage =>
  ({ pageNo: 5, widthPt: 500, heightPt: 700 }) as unknown as ReadingPage;

describe("citation geometry resolves from the loaded chapter only", () => {
  it("finds the cited segment by job-global idx and page", () => {
    expect(citedSegment(chapter(), citation())).not.toBeNull();
    expect(citedSegment(chapter(), citation({ segmentIdx: 41 }))).toBeNull();
    expect(citedSegment(chapter(), citation({ page: 6 }))).toBeNull();
    expect(citedSegment(null, citation())).toBeNull();
  });

  it("plans only on the shown page with usable geometry", () => {
    const plan = agentCitationPlan(citation(), chapter(), 5, page());
    expect(plan).not.toBeNull();
    expect(plan?.pageNo).toBe(5);
    expect(plan?.chapter).toBe(0);
    expect(agentCitationPlan(citation(), chapter(), 4, page())).toBeNull();
    expect(agentCitationPlan(null, chapter(), 5, page())).toBeNull();
  });
});

describe("the applied-effect ack is honest (B-1 gate)", () => {
  const plan = {
    box: { leftPct: 10, topPct: 20, widthPct: 30, heightPct: 5 },
    pageNo: 5,
    chapter: 0,
  };
  it("applied comes ONLY from the overlay's own rendered plan", () => {
    expect(resolveAck(plan, true, false, 5, 5, true)).toEqual({
      done: true,
      applied: true,
      reason: null,
    });
    // A NaN plan is invalid geometry, never applied.
    const nanPlan = {
      ...plan,
      box: { ...plan.box, topPct: Number.NaN },
    };
    expect(resolveAck(nanPlan, true, false, 5, 5, true)).toEqual({
      done: true,
      applied: false,
      reason: "invalid_geometry",
    });
    // Script mode renders nothing: honest, terminal.
    expect(resolveAck(null, false, false, 5, 5, true)).toEqual({
      done: true,
      applied: false,
      reason: "script_mode",
    });
    // Paused following: retained, honestly not applied.
    expect(resolveAck(null, true, true, 5, 5, true)).toEqual({
      done: true,
      applied: false,
      reason: "following_paused",
    });
    // On the cited page with page data loaded but no plan: geometry failure.
    expect(resolveAck(null, true, false, 5, 5, true)).toEqual({
      done: true,
      applied: false,
      reason: "no_geometry",
    });
    // Page turn / page data still in flight: pending (the Rust deadline owns
    // the timeout) — never a premature verdict either way.
    expect(resolveAck(null, true, false, 4, 5, true).done).toBe(false);
    expect(resolveAck(null, true, false, 5, 5, false).done).toBe(false);
  });
});
