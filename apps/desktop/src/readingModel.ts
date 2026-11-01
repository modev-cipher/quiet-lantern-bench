export type TimelineOrigin =
  | "measured"
  | "dynamic"
  | "unverified"
  | "legacy";

export type WordProvenance = "fresh" | "stale" | "missing";

export type AnchorRung = "word" | "line" | "block" | "page";

export interface AnchorRect {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

export interface ReadingWord {
  w: string;
  startMs: number;
  endMs: number;
  rect: AnchorRect | null;
}

export interface ReadingSegment {
  segmentId: string;
  idx: number;
  pageNo: number;
  kind: string;
  figureId: string | null;
  text: string;
  startMs: number;
  endMs: number;
  provenance: WordProvenance;
  words: ReadingWord[];
  anchor: AnchorRung;
  rects: AnchorRect[];
}

export interface ReadingChapterSummary {
  chapterId: string;
  idx: number;
  title: string;
  pageStart: number;
  pageEnd: number;
  startMs: number;
  endMs: number;
  segmentCount: number;
}

export interface ReadingIndex {
  jobId: string;
  bookId: string;
  origin: TimelineOrigin;
  totalMs: number;
  chapters: ReadingChapterSummary[];
}

export type GeometryOutcome = "ensured" | "unavailable";

export interface ReadingChapter {
  anchorGeometry: GeometryOutcome;
  chapterId: string;
  startMs: number;
  endMs: number;
  origin: TimelineOrigin;
  segments: ReadingSegment[];
  mode?: string;
}

export interface ReadingPage {
  pageNo: number;
  widthPt: number | null;
  heightPt: number | null;
  imagePath: string | null;
}

export interface ReadingFigure {
  figureId: string;
  pageNo: number;
  bbox: [number, number, number, number] | null;
  cropHash: string | null;
}

export interface ReadingCursor {
  segmentIdx: number | null;
  inSegment: boolean;
  wordIdx: number | null;
  heldWordIdx: number | null;
  fill: number;
}

export const MAX_ADVANCE_SEGMENTS = 4;

export const OUTSIDE_CURSOR: ReadingCursor = {
  segmentIdx: null,
  inSegment: false,
  wordIdx: null,
  heldWordIdx: null,
  fill: 0,
};

function contains(startMs: number, endMs: number, t: number): boolean {
  return t >= startMs && t < endMs;
}

function lastIndexStartingAtOrBefore(
  spans: Array<{ startMs: number }>,
  t: number,
): number | null {
  let low = 0;
  let high = spans.length - 1;
  let found: number | null = null;
  while (low <= high) {
    const mid = (low + high) >> 1;
    if (spans[mid].startMs <= t) {
      found = mid;
      low = mid + 1;
    } else {
      high = mid - 1;
    }
  }
  return found;
}

export function wordAt(segment: ReadingSegment, t: number): number | null {
  const index = lastIndexStartingAtOrBefore(segment.words, t);
  if (index == null) return null;
  const word = segment.words[index];
  return contains(word.startMs, word.endMs, t) ? index : null;
}

export function heldWordAt(segment: ReadingSegment, t: number): number | null {
  if (t >= segment.endMs) return null;
  return lastIndexStartingAtOrBefore(segment.words, t);
}

export function underlineFill(segment: ReadingSegment, t: number): number {
  const span = segment.endMs - segment.startMs;
  if (span <= 0) return 0;
  const progress = (t - segment.startMs) / span;
  return Math.min(1, Math.max(0, progress));
}

export function segmentAt(chapter: ReadingChapter, t: number): number | null {
  const index = lastIndexStartingAtOrBefore(chapter.segments, t);
  if (index == null) return null;
  const segment = chapter.segments[index];
  return contains(segment.startMs, segment.endMs, t) ? index : null;
}

export function chapterAt(index: ReadingIndex, t: number): number | null {
  const position = lastIndexStartingAtOrBefore(index.chapters, t);
  if (position == null) return null;
  const chapter = index.chapters[position];
  return contains(chapter.startMs, chapter.endMs, t) ? position : null;
}

export function chapterToLoadAt(index: ReadingIndex, t: number): number | null {
  if (index.chapters.length === 0) return null;
  const position = lastIndexStartingAtOrBefore(index.chapters, t);
  return position ?? 0;
}

function cursorAtSegment(
  chapter: ReadingChapter,
  position: number | null,
  t: number,
): ReadingCursor {
  if (position == null) return OUTSIDE_CURSOR;
  const segment = chapter.segments[position];
  if (!contains(segment.startMs, segment.endMs, t)) {
    return {
      segmentIdx: position,
      inSegment: false,
      wordIdx: null,
      heldWordIdx: null,
      fill: 1,
    };
  }
  return {
    segmentIdx: position,
    inSegment: true,
    wordIdx: wordAt(segment, t),
    heldWordIdx: heldWordAt(segment, t),
    fill: underlineFill(segment, t),
  };
}

export function resolveCursor(
  chapter: ReadingChapter,
  t: number,
): ReadingCursor {
  return cursorAtSegment(
    chapter,
    lastIndexStartingAtOrBefore(chapter.segments, t),
    t,
  );
}

export function advanceCursor(
  previous: ReadingCursor,
  chapter: ReadingChapter,
  t: number,
): ReadingCursor | null {
  const from = previous.segmentIdx;
  if (from == null) return null;
  const anchor = chapter.segments[from];
  if (anchor == null || t < anchor.startMs) return null;
  const limit = Math.min(from + MAX_ADVANCE_SEGMENTS, chapter.segments.length - 1);
  let position = from;
  while (position < limit && chapter.segments[position + 1].startMs <= t) {
    position += 1;
  }
  if (position === limit && position < chapter.segments.length - 1) {
    if (chapter.segments[position + 1].startMs <= t) return null;
  }
  return cursorAtSegment(chapter, position, t);
}
