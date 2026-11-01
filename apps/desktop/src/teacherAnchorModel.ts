// Teacher-mode anchoring (VOICE_AGENT §11.1): the live-tutor presentation
// contract over the reading surface. The Rust event pump surfaces only
// live-generation TeacherAnchor events (stale ones are dropped and acked
// upstream); this model owns everything the UI still has to decide:
//
// - Ordering: events cross the Tauri boundary asynchronously, so a lower
//   generation arriving after a higher one is a superseded response's anchor
//   and must never move the page (§11.1 staleness, defended monotonically —
//   the UI cannot see the live barge-in counter, only order).
// - Change-only sync: the preview scrolls on anchor CHANGE only; the same
//   anchor repeated re-arms nothing.
// - Scroll pause: a manual user scroll pauses following ("Following: off —
//   resume", the log-viewer pattern); anchors arriving while paused update
//   `latest` but never move the page (`synced` freezes); resume jumps to the
//   latest anchor. Sync never fights the user's scroll.
// - No active anchor → the page does not move (this model then contributes
//   nothing to the shown page).
//
// Zero text highlighting here by design: a live tutor session explains, it
// does not read (§11.1's hard rule) — this file deals exclusively in region
// and block geometry, never words.

import type {
  AnchorRect,
  ReadingFigure,
  ReadingPage,
  ReadingSegment,
} from "./readingModel";
import { projectRect, unionOf, type OverlayBox } from "./readingPageModel";

export const ANCHOR_KINDS = ["section", "figure", "equation", "table"] as const;

export type TeacherAnchorKind = (typeof ANCHOR_KINDS)[number];

export interface TeacherAnchor {
  kind: TeacherAnchorKind;
  id: string;
  gen: number;
}

export interface TeacherAnchorState {
  /** The newest live anchor the tutor issued, or null (nothing active). */
  latest: TeacherAnchor | null;
  /** The anchor the preview follows — freezes while paused. */
  synced: TeacherAnchor | null;
  /** A manual scroll turned following off; resume re-syncs to `latest`. */
  paused: boolean;
  /** Bumps exactly when the preview should sync to `synced` (change-only). */
  syncNonce: number;
}

export const INITIAL_TEACHER_ANCHOR: TeacherAnchorState = {
  latest: null,
  synced: null,
  paused: false,
  syncNonce: 0,
};

export type TeacherAnchorAction =
  | { type: "anchor"; anchor: TeacherAnchor }
  | { type: "pause" }
  | { type: "resume" }
  | { type: "release" }
  | { type: "sessionEnd" };

/** Parses the Tauri event payload; anything out of vocabulary is ignored. */
export function teacherAnchorFrom(payload: unknown): TeacherAnchor | null {
  if (typeof payload !== "object" || payload == null) return null;
  const { kind, id, gen } = payload as Record<string, unknown>;
  if (!ANCHOR_KINDS.includes(kind as TeacherAnchorKind)) return null;
  if (typeof id !== "string" || id.length === 0) return null;
  if (typeof gen !== "number" || !Number.isFinite(gen) || gen < 0) return null;
  return { kind: kind as TeacherAnchorKind, id, gen };
}

function sameTarget(a: TeacherAnchor | null, b: TeacherAnchor): boolean {
  return a != null && a.kind === b.kind && a.id === b.id;
}

export function teacherAnchorReducer(
  state: TeacherAnchorState,
  action: TeacherAnchorAction,
): TeacherAnchorState {
  switch (action.type) {
    case "anchor": {
      const next = action.anchor;
      // A superseded response's anchor arriving late must never move the
      // page (§11.1); order is the only staleness signal available here.
      if (state.latest != null && next.gen < state.latest.gen) return state;
      if (state.paused) {
        // Following is off: remember the target, move nothing.
        return { ...state, latest: next };
      }
      // Scroll on anchor change only.
      const changed = !sameTarget(state.synced, next);
      return {
        latest: next,
        synced: next,
        paused: false,
        syncNonce: changed ? state.syncNonce + 1 : state.syncNonce,
      };
    }
    case "pause":
      if (state.synced == null || state.paused) return state;
      return { ...state, paused: true };
    case "resume": {
      if (!state.paused) return state;
      return {
        latest: state.latest,
        synced: state.latest,
        paused: false,
        syncNonce: state.latest ? state.syncNonce + 1 : state.syncNonce,
      };
    }
    case "release":
      if (state.synced == null && !state.paused) return state;
      return { ...state, synced: null, paused: false };
    case "sessionEnd":
      return state.latest == null && state.synced == null && !state.paused
        ? state
        : INITIAL_TEACHER_ANCHOR;
    default:
      return state;
  }
}

/** Region kinds spotlight a layout region box; sections outline a heading. */
export function isRegionKind(
  kind: TeacherAnchorKind,
): kind is "figure" | "equation" | "table" {
  return kind !== "section";
}

export interface AnchorTarget {
  pageNo: number;
  /** Page-space rects to spotlight (pt). Empty: scroll only, nothing drawn. */
  rects: AnchorRect[];
  /** Region box (outline + dim) vs heading block (outline only, §11.1). */
  shape: "region" | "block";
}

function rectFrom(bbox: [number, number, number, number]): AnchorRect {
  const [x0, y0, x1, y1] = bbox;
  return {
    x0: Math.min(x0, x1),
    y0: Math.min(y0, y1),
    x1: Math.max(x0, x1),
    y1: Math.max(y0, y1),
  };
}

/**
 * Resolves an anchor id against the book data the view already holds:
 * region kinds against the layout-stage regions (`figure_region` rows —
 * figures, tables and equations all live there), sections against the
 * chapter's heading segments. An id that resolves nowhere returns null and
 * the page stays put — the UI half of the §5 existence check.
 */
export function anchorTarget(
  anchor: TeacherAnchor | null,
  figures: ReadingFigure[],
  segments: ReadingSegment[],
): AnchorTarget | null {
  if (anchor == null) return null;
  if (isRegionKind(anchor.kind)) {
    const region = figures.find((entry) => entry.figureId === anchor.id);
    if (region == null) return null;
    return {
      pageNo: region.pageNo,
      rects: region.bbox == null ? [] : [rectFrom(region.bbox)],
      shape: "region",
    };
  }
  const heading = segments.find(
    (entry) => entry.kind === "heading" && entry.segmentId === anchor.id,
  );
  if (heading == null) return null;
  return { pageNo: heading.pageNo, rects: heading.rects, shape: "block" };
}

/**
 * What the anchor layer contributes to the shown page: the synced anchor's
 * page, or null (no active anchor / unresolvable id → the page stays put).
 * Pausing freezes `synced`, so the contribution is stable under new
 * arrivals until resume.
 */
export function anchorPage(target: AnchorTarget | null): number | null {
  return target?.pageNo ?? null;
}

export interface SpotlightPlan {
  /** "region": outline + surrounding dim. "block": outline only (§11.1). */
  shape: "region" | "block";
  box: OverlayBox;
}

/**
 * The spotlight to draw on the shown page, or null: setting off, wrong
 * page, or no usable geometry (then the scroll alone syncs).
 */
export function spotlightPlan(
  target: AnchorTarget | null,
  shownPageNo: number | null,
  page: ReadingPage | null,
  enabled: boolean,
): SpotlightPlan | null {
  if (!enabled || target == null) return null;
  if (shownPageNo == null || page == null) return null;
  if (target.pageNo !== shownPageNo || page.pageNo !== shownPageNo) return null;
  const envelope = unionOf(target.rects);
  if (envelope == null) return null;
  const box = projectRect(envelope, page);
  if (box == null) return null;
  return { shape: target.shape, box };
}

// One user setting disables the spotlight entirely (§11.1). Stored like the
// reading mode: local presentation preference, not provider-bound data.
export const SPOTLIGHT_STORAGE_KEY = "txtcaster.teacherSpotlight";

export function spotlightEnabledFrom(stored: string | null): boolean {
  return stored !== "off";
}

export function loadSpotlightEnabled(): boolean {
  try {
    return spotlightEnabledFrom(localStorage.getItem(SPOTLIGHT_STORAGE_KEY));
  } catch {
    return true;
  }
}

export function storeSpotlightEnabled(enabled: boolean): void {
  try {
    localStorage.setItem(SPOTLIGHT_STORAGE_KEY, enabled ? "on" : "off");
  } catch {
    return;
  }
}
