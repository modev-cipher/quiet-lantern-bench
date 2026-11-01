import type {
  AnchorRect,
  ReadingChapter,
  ReadingPage,
  ReadingSegment,
} from "./readingModel";
import { t, type MessageKey } from "./i18n";

export type ReadingMode = "script" | "page";

export type PageModeReason =
  "user-preference" | "teacher" | "text-layer-missing";

export type OverlayKind = "word" | "segment" | "none";

export interface OverlayBox {
  leftPct: number;
  topPct: number;
  widthPct: number;
  heightPct: number;
}

export interface OverlayPlan {
  kind: OverlayKind;
  boxes: OverlayBox[];
}

export const NO_OVERLAY: OverlayPlan = { kind: "none", boxes: [] };

export const MODE_STORAGE_KEY = "txtcaster.readingMode";

export const TEACHER_MODE = "teacher";

const ADVISORY: Record<PageModeReason, MessageKey | null> = {
  "user-preference": null,
  teacher: "reader.teacherModeAdvisory",
  "text-layer-missing": "reader.noTextLayerAdvisory",
};

const advisory = (key: MessageKey | null) => (key == null ? null : t(key));

export function readingModeFrom(stored: string | null): ReadingMode {
  return stored === "page" ? "page" : "script";
}

export function loadReadingMode(): ReadingMode {
  try {
    return readingModeFrom(localStorage.getItem(MODE_STORAGE_KEY));
  } catch {
    return "script";
  }
}

export function storeReadingMode(mode: ReadingMode): void {
  try {
    localStorage.setItem(MODE_STORAGE_KEY, mode);
  } catch {
    return;
  }
}

export function pageModeReason(
  jobMode: string,
  isScanned: boolean,
): PageModeReason {
  if (isScanned) return "text-layer-missing";
  if (jobMode.toLowerCase() === TEACHER_MODE) return "teacher";
  return "user-preference";
}

export function chapterHighlights(chapter: ReadingChapter | null): boolean {
  return (
    chapter != null &&
    chapter.anchorGeometry === "ensured" &&
    anchoredSegmentCount(chapter.segments) > 0
  );
}

export function pageModeAdvisory(
  jobMode: string,
  isScanned: boolean,
  chapter: ReadingChapter | null,
): string | null {
  const reason = pageModeReason(jobMode, isScanned);
  if (reason !== "text-layer-missing") return advisory(ADVISORY[reason]);
  // "no text layer" speaks about the chapter on screen: the claim may only
  // stand once a loaded chapter shows its anchors truly failed. Otherwise —
  // geometry ensured, or no chapter yet — the job mode alone speaks, since
  // its limits hold regardless of geometry.
  if (chapter != null && !chapterHighlights(chapter))
    return advisory(ADVISORY[reason]);
  return advisory(ADVISORY[pageModeReason(jobMode, false)]);
}

export function unionOf(rects: AnchorRect[]): AnchorRect | null {
  if (rects.length === 0) return null;
  return rects.reduce((total, rect) => ({
    x0: Math.min(total.x0, rect.x0),
    y0: Math.min(total.y0, rect.y0),
    x1: Math.max(total.x1, rect.x1),
    y1: Math.max(total.y1, rect.y1),
  }));
}

export function projectRect(
  rect: AnchorRect,
  page: ReadingPage,
): OverlayBox | null {
  const { widthPt, heightPt } = page;
  if (widthPt == null || heightPt == null) return null;
  if (widthPt <= 0 || heightPt <= 0) return null;
  const left = rect.x0 / widthPt;
  const top = rect.y0 / heightPt;
  const width = (rect.x1 - rect.x0) / widthPt;
  const height = (rect.y1 - rect.y0) / heightPt;
  if (width <= 0 || height <= 0) return null;
  if (left < 0 || top < 0 || left + width > 1.0001 || top + height > 1.0001) {
    return null;
  }
  return {
    leftPct: left * 100,
    topPct: top * 100,
    widthPct: width * 100,
    heightPct: height * 100,
  };
}

export function segmentFallbackRects(
  segment: ReadingSegment,
  page: ReadingPage,
): AnchorRect[] {
  const usable = (rect: AnchorRect) => projectRect(rect, page) != null;
  if (segment.rects.length > 0) return segment.rects.filter(usable);
  const located = segment.words
    .map((word) => word.rect)
    .filter((rect): rect is AnchorRect => rect != null)
    .filter(usable);
  const envelope = unionOf(located);
  return envelope == null ? [] : [envelope];
}

export function overlayPlan(
  segment: ReadingSegment | null,
  wordIdx: number | null,
  shownPageNo: number | null,
  page: ReadingPage | null,
  isTeacher: boolean = false,
): OverlayPlan {
  if (isTeacher) return NO_OVERLAY;
  if (segment == null || page == null) return NO_OVERLAY;
  if (shownPageNo == null || shownPageNo !== segment.pageNo) return NO_OVERLAY;
  if (page.pageNo !== shownPageNo) return NO_OVERLAY;

  const isWordAnchored =
    segment.anchor === "word" || segment.words.some((w) => w.rect != null);

  if (isWordAnchored) {
    if (wordIdx == null) return NO_OVERLAY;
    const directWord = segment.words[wordIdx]?.rect ?? null;
    if (directWord != null) {
      const box = projectRect(directWord, page);
      if (box != null) return { kind: "word", boxes: [box] };
    }
    for (let i = wordIdx - 1; i >= 0; i -= 1) {
      const candidate = segment.words[i]?.rect ?? null;
      if (candidate != null) {
        const box = projectRect(candidate, page);
        if (box != null) return { kind: "word", boxes: [box] };
      }
    }
    return NO_OVERLAY;
  }

  const boxes = segmentFallbackRects(segment, page)
    .map((rect) => projectRect(rect, page))
    .filter((box): box is OverlayBox => box != null);
  if (boxes.length === 0) return NO_OVERLAY;
  return { kind: "segment", boxes };
}

export interface WordHit {
  segmentIdx: number;
  wordIdx: number;
  startMs: number;
}

function containsPoint(rect: AnchorRect, xPt: number, yPt: number): boolean {
  if (rect.x1 <= rect.x0 || rect.y1 <= rect.y0) return false;
  return xPt >= rect.x0 && xPt <= rect.x1 && yPt >= rect.y0 && yPt <= rect.y1;
}

export function wordAtPagePoint(
  segments: ReadingSegment[],
  pageNo: number,
  xPt: number,
  yPt: number,
): WordHit | null {
  for (let segmentIdx = 0; segmentIdx < segments.length; segmentIdx += 1) {
    const segment = segments[segmentIdx];
    if (segment.pageNo !== pageNo) continue;
    for (let wordIdx = 0; wordIdx < segment.words.length; wordIdx += 1) {
      const rect = segment.words[wordIdx].rect;
      if (rect == null) continue;
      if (containsPoint(rect, xPt, yPt)) {
        return { segmentIdx, wordIdx, startMs: segment.words[wordIdx].startMs };
      }
    }
  }
  return null;
}

export interface TapOutcome {
  pageNo: number | null;
  xPt: number | null;
  yPt: number | null;
  hit: WordHit | null;
}

export function tapAtFraction(
  segments: ReadingSegment[],
  pageNo: number | null,
  page: ReadingPage | null,
  fx: number,
  fy: number,
): TapOutcome {
  const unmapped: TapOutcome = { pageNo, xPt: null, yPt: null, hit: null };
  if (pageNo == null || page == null || page.pageNo !== pageNo) return unmapped;
  const { widthPt, heightPt } = page;
  if (widthPt == null || heightPt == null) return unmapped;
  if (widthPt <= 0 || heightPt <= 0) return unmapped;
  if (!Number.isFinite(fx) || !Number.isFinite(fy)) return unmapped;
  const xPt = fx * widthPt;
  const yPt = fy * heightPt;
  return { pageNo, xPt, yPt, hit: wordAtPagePoint(segments, pageNo, xPt, yPt) };
}

export function anchoredSegmentCount(segments: ReadingSegment[]): number {
  return segments.filter(
    (segment) =>
      segment.rects.length > 0 ||
      segment.words.some((word) => word.rect != null),
  ).length;
}
